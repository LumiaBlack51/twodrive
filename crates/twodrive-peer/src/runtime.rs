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
        let Ok(bytes) = store.get("devices", &object.name) else {
            continue;
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
            Err(_) => eprintln!(
                "Peer poll failed; retrying in 5 seconds (credentials and remote error bodies omitted)."
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
        let incoming = (|| -> anyhow::Result<_> {
            ensure!(object.size <= MAX_CONTROL_BYTES as u64, "oversized object");
            let bytes = store.get(&bucket, &object.name)?;
            let env: Envelope = serde_json::from_slice(&bytes)?;
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
        let mut child = cmd.spawn().context("unable to start peer worker")?;
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
