use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::SystemTime,
};

use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use rand::{rngs::StdRng, RngCore, SeedableRng};
use rayon::prelude::*;
use starknet::core::{
    crypto::{compute_hash_on_elements, pedersen_hash},
    types::FieldElement,
    utils::{normalize_address, UdcUniqueSettings, UdcUniqueness},
};

/// The default UDC address: 0x041a78e741e5af2fec34b695679bc6891742439f7afb8484ecd7766661ad02bf.
const DEFAULT_UDC_ADDRESS: FieldElement = FieldElement::from_mont([
    15144800532519055890,
    15685625669053253235,
    9333317513348225193,
    121672436446604875,
]);

// Cairo string of "STARKNET_CONTRACT_ADDRESS"
const CONTRACT_ADDRESS_PREFIX: FieldElement = FieldElement::from_mont([
    3829237882463328880,
    17289941567720117366,
    8635008616843941496,
    533439743893157637,
]);

#[derive(Debug, Parser)]
pub struct MineUdcSalt {
    #[clap(
        long,
        help = "Prefix bits in BINARY representation, ASSUMING 252-BIT ADDRESSES"
    )]
    prefix: String,
    #[clap(long, help = "Suffix bits in BINARY representation")]
    suffix: String,
    #[clap(long, help = "Do not derive contract address from deployer address")]
    not_unique: bool,
    #[clap(
        long,
        help = "Deployer address. Needed if and only if not using --no-unique"
    )]
    deployer_address: Option<FieldElement>,
    #[clap(long, default_value = "1", help = "The number of parallel jobs to run")]
    jobs: u32,
    #[clap(help = "Class hash")]
    class_hash: FieldElement,
    #[clap(help = "Raw constructor arguments (argument resolution not supported yet)")]
    ctor_args: Vec<FieldElement>,
}

struct Miner {
    udc_uniqueness: UdcUniqueness,
    class_hash: FieldElement,
    ctor_hash: FieldElement,
    bloom: [bool; 252],
    prefix_length: usize,
    suffix_length: usize,
    start_nonce: FieldElement,
    cancellation_token: Arc<AtomicBool>,
}

#[derive(Debug)]
struct MineResult {
    nonce: FieldElement,
    deployed_address: FieldElement,
}

impl MineUdcSalt {
    pub fn run(self) -> Result<()> {
        let udc_uniqueness = match (self.not_unique, self.deployer_address) {
            (true, Some(_)) => {
                anyhow::bail!("--deployer-address must not be used when --not-unique is on");
            }
            (false, None) => {
                anyhow::bail!("--deployer-address must be used when --not-unique is off");
            }
            (true, None) => UdcUniqueness::NotUnique,
            (false, Some(deployer_address)) => {
                eprintln!(
                    "{}",
                    "WARNING: mining without --not-unique is slower. \
                    Try using --no-unique instead \
                    (you need to also use this option for the deploy command)."
                        .bright_magenta()
                );

                UdcUniqueness::Unique(UdcUniqueSettings {
                    deployer_address,
                    udc_contract_address: DEFAULT_UDC_ADDRESS,
                })
            }
        };

        if self.prefix.len() > 252 {
            anyhow::bail!("invalid prefix length");
        }
        if self.suffix.len() > 252 {
            anyhow::bail!("invalid suffix length");
        }

        let prefix_bits = self
            .suffix
            .chars()
            .rev()
            .map(|bit| match bit {
                '1' => Ok(true),
                '0' => Ok(false),
                _ => anyhow::bail!("invalid bit: {}", bit),
            })
            .collect::<Result<Vec<_>>>()?;
        let suffix_bits = self
            .prefix
            .chars()
            .rev()
            .map(|bit| match bit {
                '1' => Ok(true),
                '0' => Ok(false),
                _ => anyhow::bail!("invalid bit: {}", bit),
            })
            .collect::<Result<Vec<_>>>()?;

        let prefix_len = prefix_bits.len();
        let suffix_len = suffix_bits.len();

        let mut bloom = [false; 252];
        bloom[..prefix_len].copy_from_slice(&prefix_bits);
        bloom[(252 - suffix_len)..].copy_from_slice(&suffix_bits);

        if Self::validate_bloom(&bloom).is_err() {
            anyhow::bail!("prefix/suffix out of range and impossible to mine");
        }

        let ctor_hash = compute_hash_on_elements(&self.ctor_args);

        let start_time = SystemTime::now();

        let cancellation_token = Arc::new(AtomicBool::new(false));

        // Randomizes starting nonces so that the mining process can be horizontally scaled.
        let mut rng = StdRng::from_entropy();
        let mut nonce_offset = [0u8; 32];
        rng.fill_bytes(&mut nonce_offset[1..]);

        // We only filled 31 bytes so this value is always in range.
        let nonce_offset = FieldElement::from_bytes_be(&nonce_offset).unwrap();

        let result = (0..self.jobs)
            .into_par_iter()
            .map(|job_id| {
                let start_nonce = FieldElement::MAX.floor_div(self.jobs.into())
                    * FieldElement::from(job_id)
                    + nonce_offset;

                let miner = Miner {
                    udc_uniqueness: udc_uniqueness.clone(),
                    class_hash: self.class_hash,
                    ctor_hash,
                    bloom,
                    prefix_length: prefix_len,
                    suffix_length: suffix_len,
                    start_nonce,
                    cancellation_token: cancellation_token.clone(),
                };

                miner.mine()
            })
            .find_map_any(|result| result.ok())
            .expect("at least one job should return success");

        let end_time = SystemTime::now();

        let duration = end_time.duration_since(start_time)?;

        println!(
            "Time spent: {}",
            format!("{}s", duration.as_secs()).bright_yellow()
        );

        println!(
            "Salt: {}",
            format!("{:#064x}", result.nonce).bright_yellow()
        );
        println!(
            "Address: {}",
            format!("{:#064x}", result.deployed_address).bright_yellow()
        );

        Ok(())
    }

    fn validate_bloom(bloom: &[bool]) -> Result<()> {
        let mut bloom_256 = [false; 256];
        bloom_256[..252].copy_from_slice(bloom);
        bloom_256.reverse();

        let bytes = bloom_256
            .chunks_exact(8)
            .map(|bits| {
                (if bits[0] { 128u8 } else { 0 })
                    + (if bits[1] { 64u8 } else { 0 })
                    + (if bits[2] { 32u8 } else { 0 })
                    + (if bits[3] { 16u8 } else { 0 })
                    + (if bits[4] { 8u8 } else { 0 })
                    + (if bits[5] { 4u8 } else { 0 })
                    + (if bits[6] { 2u8 } else { 0 })
                    + (if bits[7] { 1u8 } else { 0 })
            })
            .collect::<Vec<_>>();

        FieldElement::from_byte_slice_be(&bytes)?;

        Ok(())
    }
}

impl Miner {
    fn mine(&self) -> Result<MineResult> {
        let bloom = self.bloom;
        let prefix_len = self.prefix_length;
        let suffix_len = self.suffix_length;

        let mut nonce = self.start_nonce;

        while !self.cancellation_token.load(Ordering::Relaxed) {
            let (effective_salt, effective_deployer) = match &self.udc_uniqueness {
                UdcUniqueness::NotUnique => (nonce, FieldElement::ZERO),
                UdcUniqueness::Unique(settings) => (
                    pedersen_hash(&settings.deployer_address, &nonce),
                    settings.udc_contract_address,
                ),
            };

            let deployed_address = normalize_address(compute_hash_on_elements(&[
                CONTRACT_ADDRESS_PREFIX,
                effective_deployer,
                effective_salt,
                self.class_hash,
                self.ctor_hash,
            ]));

            let address_bits = deployed_address.to_bits_le();

            if Self::validate_address(&address_bits[..252], &bloom, prefix_len, suffix_len) {
                self.cancellation_token.store(true, Ordering::Relaxed);

                return Ok(MineResult {
                    nonce,
                    deployed_address,
                });
            }

            nonce += FieldElement::ONE;
        }

        Err(anyhow::anyhow!("job cancelled"))
    }

    #[inline(always)]
    fn validate_address(
        address: &[bool],
        bloom: &[bool],
        prefix_len: usize,
        suffix_len: usize,
    ) -> bool {
        for ind in 0..prefix_len {
            unsafe {
                if address.get_unchecked(ind) != bloom.get_unchecked(ind) {
                    return false;
                }
            }
        }

        for ind in (252 - suffix_len)..252 {
            unsafe {
                if address.get_unchecked(ind) != bloom.get_unchecked(ind) {
                    return false;
                }
            }
        }

        true
    }
}
