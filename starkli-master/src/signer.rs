use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use clap::Parser;
use colored::Colorize;
use starknet::{
    core::{crypto::Signature, types::FieldElement},
    signers::{LocalWallet, Signer, SigningKey, VerifyingKey},
};

#[derive(Debug)]
pub enum AnySigner {
    LocalWallet(LocalWallet),
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub enum AnySignerGetPublicKeyError {
    LocalWallet(<LocalWallet as Signer>::GetPublicKeyError),
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub enum AnySignerSignError {
    LocalWallet(<LocalWallet as Signer>::SignError),
}

#[derive(Debug, Clone, Parser)]
pub struct SignerArgs {
    #[clap(long, help = keystore_help())]
    keystore: Option<String>,
    #[clap(
        long,
        help = "Supply keystore password from command line option instead of prompt"
    )]
    keystore_password: Option<String>,
    #[clap(long, help = private_key_help())]
    private_key: Option<String>,
}

#[derive(Debug)]
pub enum SignerResolutionTask {
    /// The user explicitly requested to use a signer, usually from the command line.
    Strong(SignerResolutionTaskContent),
    /// The signer comes from a global default or environment variable.
    Weak(SignerResolutionTaskContent),
    /// No signer option is provided at all.
    None,
}

#[derive(Debug)]
pub enum SignerResolutionTaskContent {
    Keystore(KeystoreTaskContent),
    PrivateKey(PrivateKeyTaskContent),
}

#[derive(Debug)]
pub struct KeystoreTaskContent {
    keystore: String,
    keystore_password: Option<String>,
}

#[derive(Debug)]
pub struct PrivateKeyTaskContent {
    key: String,
}

enum StringValue {
    FromCommandLine(String),
    FromEnvVar(String),
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl Signer for AnySigner {
    type GetPublicKeyError = AnySignerGetPublicKeyError;
    type SignError = AnySignerSignError;

    async fn get_public_key(&self) -> Result<VerifyingKey, Self::GetPublicKeyError> {
        match self {
            Self::LocalWallet(inner) => Ok(<LocalWallet as Signer>::get_public_key(inner)
                .await
                .map_err(Self::GetPublicKeyError::LocalWallet)?),
        }
    }

    async fn sign_hash(&self, hash: &FieldElement) -> Result<Signature, Self::SignError> {
        match self {
            Self::LocalWallet(inner) => Ok(<LocalWallet as Signer>::sign_hash(inner, hash)
                .await
                .map_err(Self::SignError::LocalWallet)?),
        }
    }
}

impl SignerArgs {
    pub fn into_signer(self) -> Result<AnySigner> {
        self.into_task()?.resolve()
    }

    /// Parses the options into a resolution task without immediately performing the resolution.
    /// This method allows callers to defer resolution to a later stage while still performing some
    /// initial validations.
    pub fn into_task(self) -> Result<SignerResolutionTask> {
        // We're not using the `env` derive from `clap` because we need to distinguish between
        // whether the value is supplied from the command line or the environment variable.
        //
        // This distinction is important because we would not yell at the user for having option
        // conflicts from env vars. This allows us to reject conflicts on options provided from the
        // command line while ignoring those from env vars.
        let keystore = match self.keystore {
            Some(value) => Some(StringValue::FromCommandLine(value)),
            None => match std::env::var("STARKNET_KEYSTORE") {
                Ok(value) => Some(StringValue::FromEnvVar(value)),
                Err(_) => None,
            },
        };
        let private_key = match self.private_key {
            Some(value) => Some(StringValue::FromCommandLine(value)),
            None => match std::env::var("STARKNET_PRIVATE_KEY") {
                Ok(value) => Some(StringValue::FromEnvVar(value)),
                Err(_) => None,
            },
        };

        let task = match (keystore, self.keystore_password, private_key) {
            // Options:
            //   Keystore: from command line
            //   Private key: from env var or not supplied at all
            // Resolution: use keystore
            (Some(StringValue::FromCommandLine(keystore)), keystore_password, None)
            | (
                Some(StringValue::FromCommandLine(keystore)),
                keystore_password,
                Some(StringValue::FromEnvVar(_)),
            ) => SignerResolutionTask::Strong(SignerResolutionTaskContent::Keystore(
                KeystoreTaskContent {
                    keystore,
                    keystore_password,
                },
            )),
            // Options:
            //   Keystore: from env var or not supplied at all
            //   Private key: from command line
            // Resolution: use private key
            (None, None, Some(StringValue::FromCommandLine(private_key)))
            | (
                Some(StringValue::FromEnvVar(_)),
                None,
                Some(StringValue::FromCommandLine(private_key)),
            ) => SignerResolutionTask::Strong(SignerResolutionTaskContent::PrivateKey(
                PrivateKeyTaskContent { key: private_key },
            )),
            // Options:
            //   Keystore: from env var
            //   Private key: not supplied at all
            // Resolution: use keystore (weak)
            (Some(StringValue::FromEnvVar(keystore)), keystore_password, None) => {
                SignerResolutionTask::Weak(SignerResolutionTaskContent::Keystore(
                    KeystoreTaskContent {
                        keystore,
                        keystore_password,
                    },
                ))
            }
            // Options:
            //   Keystore: not supplied at all
            //   Private key: from env var
            // Resolution: use private key (weak)
            (None, None, Some(StringValue::FromEnvVar(private_key))) => SignerResolutionTask::Weak(
                SignerResolutionTaskContent::PrivateKey(PrivateKeyTaskContent { key: private_key }),
            ),
            // Options:
            //   Keystore: from env var
            //   Private key: from env var
            // Resolution: conflict
            // (We don't really need this branch, but it's nice to show a case-specific warning.)
            (Some(StringValue::FromEnvVar(_)), _, Some(StringValue::FromEnvVar(_))) => {
                return Err(anyhow::anyhow!(
                    "using STARKNET_KEYSTORE and STARKNET_PRIVATE_KEY \
                    at the same time is not allowed"
                ))
            }
            (None, None, None) => SignerResolutionTask::None,
            _ => {
                return Err(anyhow::anyhow!(
                    "invalid signer option combination. \
                    Do not mix options of different signer sources."
                ))
            }
        };

        Ok(task)
    }
}

impl SignerResolutionTask {
    pub fn resolve(self) -> Result<AnySigner> {
        match self {
            Self::Strong(task) | Self::Weak(task) => match task {
                SignerResolutionTaskContent::Keystore(inner) => inner.resolve(),
                SignerResolutionTaskContent::PrivateKey(inner) => inner.resolve(),
            },
            Self::None => Err(anyhow::anyhow!(
                "no valid signer option provided. \
                Consider using a keystore by providing a --keystore option.\
                \n\nFor more information, see: https://book.starkli.rs/signers"
            )),
        }
    }
}

impl KeystoreTaskContent {
    pub fn resolve(self) -> Result<AnySigner> {
        if self.keystore.is_empty() {
            anyhow::bail!("empty keystore path");
        }

        let keystore = PathBuf::from(shellexpand::tilde(&self.keystore).into_owned());

        if self.keystore_password.is_some() {
            eprintln!(
                "{}",
                "WARNING: setting keystore passwords via --password is generally \
                        considered insecure, as they will be stored in your shell history or other \
                        log files."
                    .bright_magenta()
            );
        }

        if !keystore.exists() {
            anyhow::bail!("keystore file not found");
        }

        let password = if let Some(password) = self.keystore_password {
            password
        } else {
            rpassword::prompt_password("Enter keystore password: ")?
        };

        let key = SigningKey::from_keystore(keystore, &password)?;

        Ok(AnySigner::LocalWallet(LocalWallet::from_signing_key(key)))
    }
}

impl PrivateKeyTaskContent {
    pub fn resolve(self) -> Result<AnySigner> {
        let print_warning = match std::env::var("STARKLI_NO_PLAIN_KEY_WARNING") {
            Ok(value) => value == "false",
            Err(_) => true,
        };

        // TODO: change to recommend hardware wallets when they become available
        if print_warning {
            eprintln!(
                "{}",
                "WARNING: using private key in plain text is highly insecure, and you should \
                ONLY do this for development. Consider using an encrypted keystore instead. \
                (Check out https://book.starkli.rs/signers on how to suppress this warning)"
                    .bright_magenta()
            );
        }

        let private_key = FieldElement::from_hex_be(&self.key)?;
        let key = SigningKey::from_secret_scalar(private_key);

        Ok(AnySigner::LocalWallet(LocalWallet::from_signing_key(key)))
    }
}

fn keystore_help() -> String {
    format!(
        "Path to keystore JSON file [env: STARKNET_KEYSTORE={}]",
        std::env::var("STARKNET_KEYSTORE").unwrap_or_default()
    )
}

fn private_key_help() -> String {
    format!(
        "Private key in hex in plain text [env: STARKNET_PRIVATE_KEY={}]",
        std::env::var("STARKNET_PRIVATE_KEY").unwrap_or_default()
    )
}
