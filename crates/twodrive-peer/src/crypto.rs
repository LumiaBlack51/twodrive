//! Sign the complete routing/session/expiry metadata and plaintext, then seal.
use crate::{
    identity::{Identity, Presence, bytes32, nonce, valid_id, verify},
    protocol::Message,
};
use anyhow::ensure;
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use hkdf::Hkdf;
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use twodrive_backend::control::MAX_CONTROL_BYTES;
use x25519_dalek::{PublicKey, StaticSecret};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub protocol: u8,
    pub id: String,
    pub from: String,
    pub to: String,
    pub from_session: String,
    pub to_session: String,
    pub issued: i64,
    pub expires: i64,
    pub ephemeral: String,
    pub nonce: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub header: Header,
    pub ciphertext: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Signed {
    message: Message,
    signature: String,
}
fn aad(header: &Header) -> Vec<u8> {
    let mut b = b"TwoDrive/control/v1\0".to_vec();
    b.extend(serde_json::to_vec(header).unwrap());
    b
}
fn signing(header: &Header, message: &Message) -> Vec<u8> {
    let mut b = aad(header);
    b.extend(serde_json::to_vec(message).unwrap());
    b
}
fn key(shared: &[u8; 32], header: &Header) -> anyhow::Result<[u8; 32]> {
    let mut key = [0; 32];
    Hkdf::<Sha256>::new(Some(header.id.as_bytes()), shared)
        .expand(&aad(header), &mut key)
        .map_err(|_| anyhow::anyhow!("HKDF failure"))?;
    Ok(key)
}
pub fn seal(
    identity: &Identity,
    session: &str,
    peer: &Presence,
    message: Message,
    now: i64,
) -> anyhow::Result<Envelope> {
    peer.verify(now)?;
    let secret = StaticSecret::random_from_rng(OsRng);
    let shared = secret.diffie_hellman(&PublicKey::from(bytes32(&peer.encryption)?));
    ensure!(
        shared.was_contributory(),
        "invalid recipient encryption key"
    );
    let mut n = [0; 24];
    OsRng.fill_bytes(&mut n);
    let h = Header {
        protocol: 1,
        id: nonce(),
        from: identity.id(),
        to: peer.device.clone(),
        from_session: session.into(),
        to_session: peer.session.clone(),
        issued: now,
        expires: now + 120,
        ephemeral: hex::encode(PublicKey::from(&secret).as_bytes()),
        nonce: hex::encode(n),
    };
    let signed = Signed {
        signature: identity.sign(&signing(&h, &message)),
        message,
    };
    let cipher = XChaCha20Poly1305::new((&key(shared.as_bytes(), &h)?).into());
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&n),
            Payload {
                msg: &serde_json::to_vec(&signed)?,
                aad: &aad(&h),
            },
        )
        .map_err(|_| anyhow::anyhow!("encryption failed"))?;
    Ok(Envelope {
        header: h,
        ciphertext: hex::encode(ciphertext),
    })
}
pub fn open(
    identity: &Identity,
    session: &str,
    peer: &Presence,
    env: &Envelope,
    now: i64,
) -> anyhow::Result<Message> {
    let h = &env.header;
    ensure!(
        env.ciphertext.len() <= MAX_CONTROL_BYTES && h.protocol == 1 && valid_id(&h.id),
        "invalid envelope size/version/id"
    );
    ensure!(
        h.to == identity.id()
            && h.from == peer.device
            && h.to_session == session
            && h.from_session == peer.session,
        "wrong recipient/sender/session"
    );
    ensure!(
        h.issued <= now + 30
            && h.expires > now
            && h.expires <= now + 150
            && h.expires.checked_sub(h.issued) == Some(120),
        "expired or invalid envelope time"
    );
    let shared = identity.shared(&h.ephemeral)?;
    let n: [u8; 24] = hex::decode(&h.nonce)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid nonce"))?;
    let cipher = XChaCha20Poly1305::new((&key(&shared, h)?).into());
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(&n),
            Payload {
                msg: &hex::decode(&env.ciphertext)?,
                aad: &aad(h),
            },
        )
        .map_err(|_| anyhow::anyhow!("authentication failed"))?;
    let s: Signed = serde_json::from_slice(&plaintext)?;
    verify(&peer.signing, &signing(h, &s.message), &s.signature)?;
    Ok(s.message)
}
