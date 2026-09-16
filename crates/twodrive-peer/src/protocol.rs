use crate::{
    crypto::{self, Envelope},
    identity::{Identity, Presence, nonce, valid_id},
};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Message {
    Hello {
        challenge: String,
    },
    HelloAck {
        challenge: String,
    },
    Ping {
        nonce: String,
    },
    Pong {
        nonce: String,
    },
    StatusRequest,
    Status {
        version: String,
        platform: String,
        selected_roots: usize,
    },
    UpdateAvailable {
        tag: String,
    },
}
#[derive(Default)]
struct Peer {
    presence: Option<Presence>,
    challenge: String,
    authenticated: bool,
}
pub struct Engine {
    pub identity: Identity,
    pub session: String,
    trusted: BTreeSet<String>,
    peers: BTreeMap<String, Peer>,
    seen: BTreeMap<String, i64>,
    pings: BTreeMap<String, (String, i64)>,
    pub selected_roots: usize,
}
impl Engine {
    pub fn new(identity: Identity, trusted: BTreeSet<String>) -> Self {
        Self {
            identity,
            session: nonce(),
            trusted,
            peers: BTreeMap::new(),
            seen: BTreeMap::new(),
            pings: BTreeMap::new(),
            selected_roots: 0,
        }
    }
    pub fn trust(&mut self, trusted: BTreeSet<String>) {
        self.peers.retain(|id, _| trusted.contains(id));
        self.trusted = trusted;
    }
    pub fn discover(&mut self, presence: Presence, now: i64) -> anyhow::Result<bool> {
        presence.verify(now)?;
        ensure!(presence.device != self.identity.id(), "self presence");
        if !self.trusted.contains(&presence.device) {
            return Ok(false);
        }
        ensure!(
            self.peers.len() < 64 || self.peers.contains_key(&presence.device),
            "peer limit reached"
        );
        let peer = self.peers.entry(presence.device.clone()).or_default();
        if peer
            .presence
            .as_ref()
            .is_none_or(|p| p.session != presence.session || p.encryption != presence.encryption)
        {
            peer.authenticated = false;
            peer.challenge = nonce();
        }
        peer.presence = Some(presence);
        Ok(true)
    }
    pub fn peer_ids(&self) -> Vec<String> {
        self.peers.keys().cloned().collect()
    }
    pub fn authenticated(&self, id: &str, now: i64) -> bool {
        self.peers.get(id).is_some_and(|p| {
            p.authenticated && p.presence.as_ref().is_some_and(|p| p.verify(now).is_ok())
        })
    }
    pub fn hello(&mut self, id: &str, now: i64) -> anyhow::Result<Envelope> {
        let challenge = self
            .peers
            .get(id)
            .context("unknown peer")?
            .challenge
            .clone();
        self.wrap(id, Message::Hello { challenge }, now)
    }
    fn wrap(&self, id: &str, message: Message, now: i64) -> anyhow::Result<Envelope> {
        let peer = self
            .peers
            .get(id)
            .and_then(|p| p.presence.as_ref())
            .context("peer not discovered")?;
        crypto::seal(&self.identity, &self.session, peer, message, now)
    }
    pub fn send(&mut self, id: &str, message: Message, now: i64) -> anyhow::Result<Envelope> {
        ensure!(self.authenticated(id, now), "peer handshake not complete");
        ensure!(
            !matches!(message, Message::Hello { .. } | Message::HelloAck { .. }),
            "reserved handshake message"
        );
        if let Message::Ping { nonce } = &message {
            self.pings.retain(|_, (_, expires)| *expires > now);
            ensure!(
                valid_id(nonce) && self.pings.len() < 256,
                "invalid ping/too many outstanding pings"
            );
            self.pings.insert(nonce.clone(), (id.into(), now + 120));
        }
        self.wrap(id, message, now)
    }
    pub fn receive(
        &mut self,
        env: &Envelope,
        now: i64,
    ) -> anyhow::Result<(Message, Option<Envelope>)> {
        let id = &env.header.from;
        let peer = self
            .peers
            .get(id)
            .and_then(|p| p.presence.as_ref())
            .context("untrusted sender")?;
        peer.verify(now)?;
        let message = crypto::open(&self.identity, &self.session, peer, env, now)?;
        self.seen.retain(|_, expiry| *expiry > now);
        ensure!(
            !self.seen.contains_key(&env.header.id),
            "replayed control message"
        );
        ensure!(self.seen.len() < 2048, "control replay window full");
        let response = match &message {
            Message::Hello { challenge } => {
                ensure!(valid_id(challenge), "invalid challenge");
                Some(Message::HelloAck {
                    challenge: challenge.clone(),
                })
            }
            Message::HelloAck { challenge } => {
                let peer = self.peers.get_mut(id).unwrap();
                ensure!(
                    !peer.challenge.is_empty() && *challenge == peer.challenge,
                    "handshake challenge mismatch"
                );
                peer.authenticated = true;
                None
            }
            _ => {
                ensure!(
                    self.authenticated(id, now),
                    "control received before handshake"
                );
                match &message {
                    Message::Ping { nonce } => {
                        ensure!(valid_id(nonce), "invalid ping");
                        Some(Message::Pong {
                            nonce: nonce.clone(),
                        })
                    }
                    Message::Pong { nonce } => {
                        ensure!(
                            self.pings
                                .get(nonce)
                                .is_some_and(|(p, expiry)| p == id && *expiry > now),
                            "unsolicited pong"
                        );
                        self.pings.remove(nonce);
                        None
                    }
                    Message::StatusRequest => Some(Message::Status {
                        version: crate::VERSION.into(),
                        platform: crate::identity::platform().into(),
                        selected_roots: self.selected_roots,
                    }),
                    Message::Status {
                        version,
                        platform,
                        selected_roots,
                    } => {
                        ensure!(
                            version.len() <= 40 && platform.len() <= 40 && *selected_roots <= 1024,
                            "invalid status"
                        );
                        None
                    }
                    Message::UpdateAvailable { tag } => {
                        crate::update::validate_tag(tag)?;
                        None
                    }
                    _ => unreachable!(),
                }
            }
        };
        self.seen.insert(env.header.id.clone(), env.header.expires);
        Ok((
            message,
            response.map(|m| self.wrap(id, m, now)).transpose()?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pair() -> (Engine, Engine) {
        let a = Identity::generate();
        let b = Identity::generate();
        let ai = a.id();
        let bi = b.id();
        (
            Engine::new(a, BTreeSet::from([bi])),
            Engine::new(b, BTreeSet::from([ai])),
        )
    }
    fn discover(a: &mut Engine, b: &mut Engine) {
        a.discover(b.identity.presence(&b.session, 1000), 1000)
            .unwrap();
        b.discover(a.identity.presence(&a.session, 1000), 1000)
            .unwrap();
    }
    fn handshake(a: &mut Engine, b: &mut Engine) {
        let hello = a.hello(&b.identity.id(), 1000).unwrap();
        let (_, ack) = b.receive(&hello, 1000).unwrap();
        a.receive(&ack.unwrap(), 1000).unwrap();
    }
    #[test]
    fn mutual_handshake_bidirectional_control_replay_and_restart() {
        let (mut a, mut b) = pair();
        discover(&mut a, &mut b);
        assert!(
            a.send(&b.identity.id(), Message::StatusRequest, 1000)
                .is_err()
        );
        handshake(&mut a, &mut b);
        handshake(&mut b, &mut a);
        for _ in 0..2 {
            let ping = a
                .send(&b.identity.id(), Message::Ping { nonce: nonce() }, 1001)
                .unwrap();
            let (_, pong) = b.receive(&ping, 1001).unwrap();
            assert!(matches!(
                a.receive(&pong.unwrap(), 1001).unwrap().0,
                Message::Pong { .. }
            ));
            assert!(b.receive(&ping, 1001).is_err());
            b.session = nonce();
            assert!(b.receive(&ping, 1001).is_err());
            discover(&mut a, &mut b);
            handshake(&mut a, &mut b);
            handshake(&mut b, &mut a);
            std::mem::swap(&mut a, &mut b);
        }
    }
    #[test]
    fn tamper_unknown_sender_expiry_version_and_wrong_session_rejected() {
        let (mut a, mut b) = pair();
        discover(&mut a, &mut b);
        let original = a.hello(&b.identity.id(), 1000).unwrap();
        let mut bad = original.clone();
        bad.header.protocol = 2;
        assert!(b.receive(&bad, 1000).is_err());
        let mut bad = original.clone();
        bad.header.to = nonce();
        assert!(b.receive(&bad, 1000).is_err());
        let mut bad = original.clone();
        bad.header.id = nonce();
        assert!(b.receive(&bad, 1000).is_err());
        let mut bad = original.clone();
        let replacement = if bad.ciphertext.starts_with('0') {
            "1"
        } else {
            "0"
        };
        bad.ciphertext.replace_range(..1, replacement);
        assert_ne!(bad.ciphertext, original.ciphertext);
        assert!(b.receive(&bad, 1000).is_err());
        assert!(b.receive(&original, 1121).is_err());
        let c = Identity::generate();
        assert!(!b.discover(c.presence(&nonce(), 1000), 1000).unwrap());
        let mut presence = a.identity.presence(&a.session, 1000);
        presence.encryption = nonce();
        assert!(b.discover(presence, 1000).is_err());
    }
    #[test]
    fn identity_persists_and_low_order_keys_fail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity");
        let a = Identity::load_or_create(&path).unwrap();
        assert_eq!(a.id(), Identity::load_or_create(&path).unwrap().id());
        assert!(a.shared(&"00".repeat(32)).is_err());
    }
    #[test]
    fn cloud_presence_rejects_terminal_controls_and_unknown_platform() {
        let id = Identity::generate();
        let mut p = id.presence(&nonce(), 1000);
        p.platform = "\x1b[2J".into();
        assert!(p.verify(1000).is_err());
        p.platform = "windows-x86_64".into();
        p.version = "\nforged output".into();
        assert!(p.verify(1000).is_err());
    }
}
