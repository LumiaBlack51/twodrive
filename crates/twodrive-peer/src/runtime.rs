use crate::{
    crypto::Envelope,
    identity::{Identity, Presence, nonce, valid_id},
    local,
    protocol::{Engine, Message},
    update,
};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use twodrive_backend::{
    GraphBackend,
    control::{ControlStore, MAX_CONTROL_BYTES},
};
use twodrive_core::now_unix;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Queued {
    pub to: String,
    pub message: Message,
    pub expires: i64,
}
pub fn trusted(home: &Path) -> anyhow::Result<BTreeSet<String>> {
    let p = home.join("trusted.json");
    if p.exists() {
        local::load(&p)
    } else {
        Ok(BTreeSet::new())
    }
}
pub fn identity(home: &Path) -> anyhow::Result<Identity> {
    Identity::load_or_create(&home.join("identity.dat"))
}
pub fn trust(home: &Path, fingerprint: &str, remove: bool) -> anyhow::Result<()> {
    ensure!(
        valid_id(fingerprint),
        "supply the complete 64-character fingerprint displayed on the other device"
    );
    let _lock = local::lock(home, "settings.lock")?;
    let mut ids = trusted(home)?;
    if remove {
        ids.remove(fingerprint);
    } else {
        ensure!(ids.len() < 64, "too many peers");
        ids.insert(fingerprint.into());
    }
    local::save(&home.join("trusted.json"), &ids)
}
pub fn queue(home: &Path, to: &str, message: Message) -> anyhow::Result<()> {
    ensure!(
        valid_id(to) && trusted(home)?.contains(to),
        "recipient is not locally trusted"
    );
    ensure!(
        fs::read_dir(home.join("outbox"))?.count() < 256,
        "local outbox full"
    );
    local::save(
        &home.join("outbox").join(format!("{}.json", nonce())),
        &Queued {
            to: to.into(),
            message,
            expires: now_unix() + 300,
        },
    )
}
pub fn discover(store: &dyn ControlStore, now: i64) -> anyhow::Result<Vec<Presence>> {
    let mut peers = Vec::new();
    for object in store.list("devices")? {
        if object.size > MAX_CONTROL_BYTES as u64 {
            continue;
        }
        let bytes = match store.get("devices", &object.name) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!(
                    "Discovery read failed: {}",
                    twodrive_backend::control::safe_diagnostic(&error)
                );
                continue;
            }
        };
        let Ok(p) = serde_json::from_slice::<Presence>(&bytes) else {
            continue;
        };
        if object.name == format!("{}.json", p.device) && p.verify(now).is_ok() {
            peers.push(p);
        }
    }
    Ok(peers)
}
fn put(store: &dyn ControlStore, env: &Envelope) -> anyhow::Result<()> {
    store.put(
        &format!("in-{}", env.header.to),
        &format!("{}.json", env.header.id),
        &serde_json::to_vec(env)?,
    )
}
#[derive(Default, Serialize)]
struct Counters {
    received: u64,
    pings: u64,
    pongs: u64,
    rejected: u64,
}
// Returns 75 only for a verified staged update; supervisor performs activation.
pub fn worker(home: &Path, auto_update: bool, ready: Option<&Path>) -> anyhow::Result<i32> {
    let _lock = local::lock(home, "worker.lock")?;
    let mut engine = Engine::new(identity(home)?, trusted(home)?);
    let roots = home.join("selected-roots.json");
    if roots.exists() {
        engine.selected_roots = local::load::<Vec<PathBuf>>(&roots)?.len();
    }
    let graph = GraphBackend::from_paths(&local::app_paths(home))?;
    // Initialize everything local before acknowledging startup. Network outages aren't bad installs.
    if let Some(path) = ready {
        local::atomic(path, b"ready")?;
    }
    let mut counters = Counters::default();
    let mut last_publish = 0;
    let mut last_hello = BTreeMap::<String, i64>::new();
    let mut update_attempts = BTreeMap::<String, i64>::new();
    println!(
        "Peer running: {} (Ctrl+C stops this isolated process)",
        engine.identity.id()
    );
    loop {
        let result = tick(
            &graph,
            home,
            &mut engine,
            &mut counters,
            &mut last_publish,
            &mut last_hello,
            &mut update_attempts,
            auto_update,
        );
        match result {
            Ok(true) => return Ok(75),
            Ok(false) => (),
            Err(error) => eprintln!(
                "Peer poll failed; retrying in 5 seconds: {}",
                twodrive_backend::control::safe_diagnostic(&error)
            ),
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}
#[allow(clippy::too_many_arguments)]
fn tick(
    store: &dyn ControlStore,
    home: &Path,
    engine: &mut Engine,
    counters: &mut Counters,
    last_publish: &mut i64,
    last_hello: &mut BTreeMap<String, i64>,
    update_attempts: &mut BTreeMap<String, i64>,
    auto_update: bool,
) -> anyhow::Result<bool> {
    let now = now_unix();
    engine.trust(trusted(home)?);
    if now - *last_publish >= 30 {
        store.put(
            "devices",
            &format!("{}.json", engine.identity.id()),
            &serde_json::to_vec(&engine.identity.presence(&engine.session, now))?,
        )?;
        *last_publish = now;
    }
    let discovered = discover(store, now)?;
    for presence in &discovered {
        if presence.device != engine.identity.id() {
            let _ = engine.discover(presence.clone(), now);
        }
    }
    for id in engine.peer_ids() {
        if !engine.authenticated(&id, now) && now - *last_hello.get(&id).unwrap_or(&0) >= 20 {
            if let Ok(hello) = engine.hello(&id, now) {
                put(store, &hello)?;
            }
            last_hello.insert(id, now);
        }
    }
    let bucket = format!("in-{}", engine.identity.id());
    for object in store.list(&bucket)? {
        // A transport failure is not an invalid envelope. Preserve it for retry.
        let bytes = if object.size <= MAX_CONTROL_BYTES as u64 {
            Some(store.get(&bucket, &object.name)?)
        } else {
            None
        };
        let incoming = (|| -> anyhow::Result<_> {
            let bytes = bytes.as_ref().context("oversized object")?;
            let env: Envelope = serde_json::from_slice(bytes)?;
            ensure!(
                object.name == format!("{}.json", env.header.id),
                "mailbox name mismatch"
            );
            let (message, response) = engine.receive(&env, now)?;
            Ok((env.header.from, message, response))
        })();
        match incoming {
            Ok((from, message, response)) => {
                if let Some(response) = response {
                    put(store, &response)?;
                }
                counters.received += 1;
                match &message {
                    Message::HelloAck { .. } => println!("Authenticated {from}"),
                    Message::Ping { .. } => {
                        counters.pings += 1;
                        println!("PING from {from}; PONG sent");
                    }
                    Message::Pong { .. } => {
                        counters.pongs += 1;
                        println!("PONG from {from}: round trip verified");
                    }
                    Message::Status { .. } => {
                        println!("STATUS from {from}: {}", serde_json::to_string(&message)?)
                    }
                    Message::UpdateAvailable { tag } => {
                        println!("Release notification: {tag}");
                        if auto_update && now - *update_attempts.get(tag).unwrap_or(&0) > 3600 {
                            update_attempts.retain(|_, t| now - *t < 3600);
                            ensure!(
                                update_attempts.len() < 64,
                                "update notification rate limit reached"
                            );
                            update_attempts.insert(tag.clone(), now);
                            if update::download_and_stage(home, tag).is_ok() {
                                store.delete(&bucket, &object.name)?;
                                return Ok(true);
                            }
                            eprintln!(
                                "Update rejected or unavailable; current executable retained."
                            );
                        }
                    }
                    _ => (),
                }
            }
            Err(_) => counters.rejected += 1,
        }
        // Only this protocol's dedicated recipient bucket. Poison objects cannot block the next poll.
        store.delete(&bucket, &object.name)?;
    }
    for entry in fs::read_dir(home.join("outbox"))?.take(256) {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let q: Queued = local::load(&entry.path())?;
        if q.expires <= now {
            fs::remove_file(entry.path())?;
            continue;
        }
        if engine.authenticated(&q.to, now) {
            put(store, &engine.send(&q.to, q.message, now)?)?;
            fs::remove_file(entry.path())?;
        }
    }
    let peers: Vec<_> = discovered.iter().filter(|p| p.device != engine.identity.id()).map(|p| serde_json::json!({"device":p.device,"trusted":trusted(home).is_ok_and(|t|t.contains(&p.device)),"authenticated":engine.authenticated(&p.device,now),"expires":p.expires})).collect();
    local::save(
        &home.join("status.json"),
        &serde_json::json!({"version":crate::VERSION,"device":engine.identity.id(),"updated":now,"peers":peers,"counters":counters}),
    )?;
    Ok(false)
}
pub fn supervise(home: &Path, auto_update: bool) -> anyhow::Result<()> {
    use std::process::{Command, Stdio};
    use std::time::Instant;
    let _lock = local::lock(home, "supervisor.lock")?;
    let bootstrap = std::env::current_exe()?;
    loop {
        if update::activate_pending(home).is_err() {
            eprintln!("Candidate update rejected; previous version retained.");
        }
        let exe = match update::active_executable(home, &bootstrap) {
            Ok(path) => path,
            Err(_) => {
                update::rollback(home)?;
                update::active_executable(home, &bootstrap)?
            }
        };
        let ready = home.join(format!("ready-{}", nonce()));
        let mut cmd = Command::new(&exe);
        cmd.arg("--state-dir")
            .arg(home)
            .arg("worker")
            .arg("--ready")
            .arg(&ready)
            .stdin(Stdio::null());
        if auto_update {
            cmd.arg("--auto-update");
        }
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(_) if exe != bootstrap => {
                update::rollback(home)?;
                continue;
            }
            Err(err) => return Err(err).context("unable to start peer worker"),
        };
        let start = Instant::now();
        let startup = loop {
            if ready.exists() {
                break true;
            }
            if child.try_wait()?.is_some() {
                break false;
            }
            if start.elapsed() > Duration::from_secs(30) {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let _ = fs::remove_file(&ready);
        if !startup {
            if exe != bootstrap {
                update::rollback(home)?;
                continue;
            }
            anyhow::bail!("peer startup failed; run login before run");
        }
        let status = child.wait()?;
        if status.code() == Some(75) {
            continue;
        }
        if !status.success() && start.elapsed() < Duration::from_secs(30) && exe != bootstrap {
            update::rollback(home)?;
            continue;
        }
        ensure!(status.success(), "peer worker stopped");
        return Ok(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use twodrive_backend::control::ControlObject;
    type Objects = BTreeMap<(String, String), Vec<u8>>;
    #[derive(Default)]
    struct Memory(RefCell<Objects>);
    impl ControlStore for Memory {
        fn list(&self, bucket: &str) -> anyhow::Result<Vec<ControlObject>> {
            Ok(self
                .0
                .borrow()
                .iter()
                .filter(|((b, _), _)| b == bucket)
                .map(|((_, name), v)| ControlObject {
                    name: name.clone(),
                    size: v.len() as u64,
                })
                .collect())
        }
        fn get(&self, bucket: &str, name: &str) -> anyhow::Result<Vec<u8>> {
            self.0
                .borrow()
                .get(&(bucket.into(), name.into()))
                .cloned()
                .context("missing")
        }
        fn put(&self, bucket: &str, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
            self.0
                .borrow_mut()
                .insert((bucket.into(), name.into()), bytes.to_vec());
            Ok(())
        }
        fn delete(&self, bucket: &str, name: &str) -> anyhow::Result<()> {
            self.0.borrow_mut().remove(&(bucket.into(), name.into()));
            Ok(())
        }
    }
    struct Harness {
        home: tempfile::TempDir,
        engine: Engine,
        counters: Counters,
        published: i64,
        hello: BTreeMap<String, i64>,
        updates: BTreeMap<String, i64>,
    }
    impl Harness {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            local::prepare(home.path()).unwrap();
            let engine = Engine::new(identity(home.path()).unwrap(), BTreeSet::new());
            Self {
                home,
                engine,
                counters: Counters::default(),
                published: 0,
                hello: BTreeMap::new(),
                updates: BTreeMap::new(),
            }
        }
        fn tick(&mut self, store: &Memory) {
            tick(
                store,
                self.home.path(),
                &mut self.engine,
                &mut self.counters,
                &mut self.published,
                &mut self.hello,
                &mut self.updates,
                false,
            )
            .unwrap();
        }
    }
    #[test]
    fn failed_mailbox_read_is_not_deleted_or_counted_as_rejected() {
        struct FailingRead {
            bucket: String,
            deleted: std::cell::Cell<bool>,
        }
        impl ControlStore for FailingRead {
            fn list(&self, bucket: &str) -> anyhow::Result<Vec<ControlObject>> {
                Ok(if bucket == self.bucket {
                    vec![ControlObject {
                        name: "test.json".into(),
                        size: 8,
                    }]
                } else {
                    vec![]
                })
            }
            fn get(&self, _: &str, _: &str) -> anyhow::Result<Vec<u8>> {
                anyhow::bail!("synthetic transport outage")
            }
            fn put(&self, _: &str, _: &str, _: &[u8]) -> anyhow::Result<()> {
                Ok(())
            }
            fn delete(&self, _: &str, _: &str) -> anyhow::Result<()> {
                self.deleted.set(true);
                Ok(())
            }
        }
        let mut h = Harness::new();
        let store = FailingRead {
            bucket: format!("in-{}", h.engine.identity.id()),
            deleted: std::cell::Cell::new(false),
        };
        let result = tick(
            &store,
            h.home.path(),
            &mut h.engine,
            &mut h.counters,
            &mut h.published,
            &mut h.hello,
            &mut h.updates,
            false,
        );
        assert!(
            !store.deleted.get(),
            "a failed fetch must remain in the mailbox"
        );
        assert!(result.is_err());
        assert_eq!(h.counters.rejected, 0);
    }
    #[test]
    fn two_isolated_peers_discover_pair_and_exchange_via_opaque_cloud_store() {
        let store = Memory::default();
        let mut a = Harness::new();
        let mut b = Harness::new();
        a.tick(&store);
        b.tick(&store);
        assert_eq!(discover(&store, now_unix()).unwrap().len(), 2);
        let ai = a.engine.identity.id();
        let bi = b.engine.identity.id();
        assert!(!a.engine.authenticated(&bi, now_unix()));
        trust(a.home.path(), &bi, false).unwrap();
        trust(b.home.path(), &ai, false).unwrap();
        for _ in 0..4 {
            a.tick(&store);
            b.tick(&store);
        }
        assert!(a.engine.authenticated(&bi, now_unix()) && b.engine.authenticated(&ai, now_unix()));
        queue(a.home.path(), &bi, Message::Ping { nonce: nonce() }).unwrap();
        queue(b.home.path(), &ai, Message::Ping { nonce: nonce() }).unwrap();
        for _ in 0..4 {
            a.tick(&store);
            b.tick(&store);
        }
        assert_eq!(
            (
                a.counters.pings,
                a.counters.pongs,
                b.counters.pings,
                b.counters.pongs
            ),
            (1, 1, 1, 1)
        );
        let path = a.home.path().join("private-directory");
        fs::create_dir(&path).unwrap();
        local::select_root(a.home.path(), &path).unwrap();
        for data in store.0.borrow().values() {
            let text = String::from_utf8_lossy(data);
            assert!(
                !text.contains("private-directory")
                    && !text.contains("access_token")
                    && !text.contains("identity.dat")
            );
        }
        // Revocation takes effect while running; poisoned cloud input cannot keep the mailbox blocked.
        trust(a.home.path(), &bi, true).unwrap();
        store
            .put(&format!("in-{ai}"), "poison.json", b"{not-json}")
            .unwrap();
        a.tick(&store);
        assert!(!a.engine.authenticated(&bi, now_unix()));
        assert_eq!(a.counters.rejected, 1);
        assert!(store.list(&format!("in-{ai}")).unwrap().is_empty());
    }
    #[test]
    fn locks_and_state_directory_guard_prevent_runtime_collision() {
        let unrelated = tempfile::tempdir().unwrap();
        fs::write(unrelated.path().join("unrelated.txt"), b"untouched").unwrap();
        assert!(local::prepare(unrelated.path()).is_err());
        assert!(!unrelated.path().join("peer-state-v1").exists());
        let home = tempfile::tempdir().unwrap();
        local::prepare(home.path()).unwrap();
        let guard = local::lock(home.path(), "worker.lock").unwrap();
        assert!(local::lock(home.path(), "worker.lock").is_err());
        drop(guard);
        assert!(local::lock(home.path(), "worker.lock").is_ok());
        fs::write(home.path().join("twodrive.sqlite3"), b"untouched").unwrap();
        assert!(local::prepare(home.path()).is_err());
        assert_eq!(
            fs::read(home.path().join("twodrive.sqlite3")).unwrap(),
            b"untouched"
        );
    }
}
