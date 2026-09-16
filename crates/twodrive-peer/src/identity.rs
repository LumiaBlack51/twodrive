//! Persistent identity adapted from relay-lab; cloud manifests are discovery, never trust anchors.
use anyhow::ensure;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use twodrive_core::private_file::{read_secret, write_secret};
use x25519_dalek::{PublicKey, StaticSecret};

pub fn nonce() -> String {
    let mut v = [0u8; 32];
    OsRng.fill_bytes(&mut v);
    hex::encode(v)
}
pub fn valid_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn bytes32(value: &str) -> anyhow::Result<[u8; 32]> {
    Ok(hex::decode(value)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid key length"))?)
}
pub struct Identity {
    signing: SigningKey,
    secret: StaticSecret,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u8,
    signing: [u8; 32],
    encryption: [u8; 32],
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Presence {
    pub protocol: u8,
    pub device: String,
    pub signing: String,
    pub encryption: String,
    pub session: String,
    pub issued: i64,
    pub expires: i64,
    pub version: String,
    pub platform: String,
    pub signature: String,
}
impl Identity {
    pub fn generate() -> Self {
        Self {
            signing: SigningKey::generate(&mut OsRng),
            secret: StaticSecret::random_from_rng(OsRng),
        }
    }
    pub fn load_or_create(path: &Path) -> anyhow::Result<Self> {
        if path.exists() {
            let s: Stored = serde_json::from_slice(&read_secret(path)?)?;
            ensure!(s.version == 1, "unsupported local identity version");
            Ok(Self {
                signing: SigningKey::from_bytes(&s.signing),
                secret: StaticSecret::from(s.encryption),
            })
        } else {
            let id = Self::generate();
            write_secret(
                path,
                &serde_json::to_vec(&Stored {
                    version: 1,
                    signing: id.signing.to_bytes(),
                    encryption: id.secret.to_bytes(),
                })?,
            )?;
            Ok(id)
        }
    }
    pub fn id(&self) -> String {
        hex::encode(Sha256::digest(self.signing.verifying_key().as_bytes()))
    }
    pub fn sign(&self, data: &[u8]) -> String {
        hex::encode(self.signing.sign(data).to_bytes())
    }
    pub fn shared(&self, public: &str) -> anyhow::Result<[u8; 32]> {
        let shared = self
            .secret
            .diffie_hellman(&PublicKey::from(bytes32(public)?));
        ensure!(shared.was_contributory(), "non-contributory X25519 key");
        Ok(*shared.as_bytes())
    }
    pub fn presence(&self, session: &str, now: i64) -> Presence {
        let mut p = Presence {
            protocol: 1,
            device: self.id(),
            signing: hex::encode(self.signing.verifying_key().as_bytes()),
            encryption: hex::encode(PublicKey::from(&self.secret).as_bytes()),
            session: session.into(),
            issued: now,
            expires: now + 180,
            version: crate::VERSION.into(),
            platform: platform().into(),
            signature: String::new(),
        };
        p.signature = self.sign(&p.signing_bytes());
        p
    }
}
pub fn platform() -> &'static str {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "windows-x86_64"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "linux-x86_64"
    } else {
        "unsupported"
    }
}
impl Presence {
    fn signing_bytes(&self) -> Vec<u8> {
        let mut p = self.clone();
        p.signature.clear();
        let mut b = b"TwoDrive/presence/v1\0".to_vec();
        b.extend(serde_json::to_vec(&p).unwrap());
        b
    }
    pub fn verify(&self, now: i64) -> anyhow::Result<()> {
        ensure!(
            self.protocol == 1 && valid_id(&self.device) && valid_id(&self.session),
            "invalid presence shape/version"
        );
        ensure!(
            self.issued <= now + 30
                && self.expires > now
                && self.expires <= now + 210
                && self.expires.checked_sub(self.issued) == Some(180),
            "stale or invalid presence time"
        );
        ensure!(
            self.version.len() < 40 && self.platform.len() < 40,
            "invalid presence metadata"
        );
        let key = bytes32(&self.signing)?;
        ensure!(
            hex::encode(Sha256::digest(key)) == self.device,
            "device fingerprint mismatch"
        );
        bytes32(&self.encryption)?;
        verify(&self.signing, &self.signing_bytes(), &self.signature)
    }
}
pub fn verify(key: &str, data: &[u8], signature: &str) -> anyhow::Result<()> {
    VerifyingKey::from_bytes(&bytes32(key)?)?
        .verify_strict(data, &Signature::from_slice(&hex::decode(signature)?)?)?;
    Ok(())
}
