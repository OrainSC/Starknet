use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use starknet::signers::SigningKey;

use crate::path::ExpandedPathbufParser;

#[derive(Debug, Parser)]
pub struct InspectPrivate {
    #[clap(
        long,
        help = "Supply password from command line option instead of prompt"
    )]
    password: Option<String>,
    #[clap(long, help = "Print the private key only")]
    raw: bool,
    #[clap(
        value_parser = ExpandedPathbufParser,
        help = "Path to the JSON keystore"
    )]
    file: PathBuf,
}

impl InspectPrivate {
    pub fn run(self) -> Result<()> {
        if self.password.is_some() {
            eprintln!(
                "{}",
                "WARNING: setting passwords via --password is generally considered insecure, \
                as they will be stored in your shell history or other log files."
                    .bright_magenta()
            );
        }

        if !self.file.exists() {
            anyhow::bail!("keystore file not found");
        }

        let password = if let Some(password) = self.password {
            password
        } else {
            rpassword::prompt_password("Enter password: ")?
        };

        let key = SigningKey::from_keystore(self.file, &password)?;

        if self.raw {
            println!("{:#064x}", key.secret_scalar());
        } else {
            println!("Private key: {:#064x}", key.secret_scalar());
        }

        Ok(())
    }
}
