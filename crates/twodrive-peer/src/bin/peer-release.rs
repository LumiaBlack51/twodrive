//! Offline publisher utility. Never linked into the peer executable or shipped to test PCs.
use anyhow::ensure;
use clap::{Parser, Subcommand};
use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use twodrive_peer::{local, update::Release};
#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    Keygen {
        #[arg(long)]
        private: PathBuf,
        #[arg(long)]
        public: PathBuf,
    },
    Sign {
        #[arg(long)]
        private: PathBuf,
        #[arg(long)]
        binary: PathBuf,
        #[arg(long)]
        version: semver::Version,
        #[arg(long)]
        platform: String,
        #[arg(long)]
        output: PathBuf,
    },
}
fn main() -> anyhow::Result<()> {
    match Args::parse().command {
        Action::Keygen { private, public } => {
            ensure!(
                !private.exists() && !public.exists(),
                "refusing to replace an existing key"
            );
            let key = SigningKey::generate(&mut OsRng);
            twodrive_core::private_file::write_secret(&private, &key.to_bytes())?;
            local::atomic(
                &public,
                format!("{}\n", hex::encode(key.verifying_key().as_bytes())).as_bytes(),
            )?;
        }
        Action::Sign {
            private,
            binary,
            version,
            platform,
            output,
        } => {
            ensure!(
                matches!(platform.as_str(), "windows-x86_64" | "linux-x86_64"),
                "unsupported platform"
            );
            let bytes = local::read(&binary, 128 * 1024 * 1024)?;
            let secret: [u8; 32] = twodrive_core::private_file::read_secret(&private)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid private key"))?;
            let key = SigningKey::from_bytes(&secret);
            let mut r = Release {
                schema: 1,
                product: "twodrive-peer".into(),
                version,
                platform: platform.clone(),
                file: if platform == "windows-x86_64" {
                    "twodrive-peer.exe"
                } else {
                    "twodrive-peer"
                }
                .into(),
                size: bytes.len() as u64,
                sha256: hex::encode(Sha256::digest(&bytes)),
                signature: String::new(),
            };
            r.signature = hex::encode(key.sign(&r.signing_bytes()).to_bytes());
            local::save(&output, &r)?;
        }
    }
    Ok(())
}
