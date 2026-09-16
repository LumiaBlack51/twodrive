use twodrive_windows::{engine::Engine, ipc, protocol::*};
fn call(engine: &Engine, id: &str, command: Command) -> Reply {
    engine.handle(Request {
        version: VERSION,
        id: id.into(),
        command,
    })
}
#[test]
fn pause_blocks_real_scheduler_and_upload_confirmations() {
    let root = tempfile::tempdir().unwrap();
    let engine = Engine::open(root.path(), true).unwrap();
    assert!(call(&engine, "pause", Command::SetPaused { paused: true }).ok);
    let imported = call(
        &engine,
        "import",
        Command::MockImport {
            name: "regression.txt".into(),
            content: "synthetic bytes".into(),
        },
    );
    assert!(imported.ok);
    let item = imported
        .snapshot
        .files
        .iter()
        .find(|f| f.name == "regression.txt")
        .unwrap();
    let id = item.id.clone();
    assert_eq!(item.state, "dirty");
    assert!(!engine.run_one().unwrap());
    assert_eq!(engine.snapshot().unwrap().queued, 1);
    assert!(
        !call(
            &engine,
            "release-dirty",
            Command::Release { id: id.clone() }
        )
        .ok
    );
    assert!(
        std::fs::read_dir(root.path().join("cache"))
            .unwrap()
            .next()
            .is_some()
    );
    assert!(call(&engine, "resume", Command::SetPaused { paused: false }).ok);
    assert!(engine.run_one().unwrap());
    let snap = engine.snapshot().unwrap();
    assert_eq!(snap.recent[0].outcome, "completed");
    assert_eq!(snap.recent[0].done, 15);
    assert!(
        call(
            &engine,
            "release-clean",
            Command::Release { id: id.clone() }
        )
        .ok
    );
    assert!(call(&engine, "download", Command::Download { id: id.clone() }).ok);
    assert!(engine.run_one().unwrap());
    assert_eq!(
        std::fs::read(root.path().join("cache").join(id)).unwrap(),
        b"synthetic bytes"
    );
}
#[test]
fn listing_does_not_hydrate_and_default_has_no_mock_data() {
    let root = tempfile::tempdir().unwrap();
    let engine = Engine::open(root.path(), true).unwrap();
    for _ in 0..10 {
        assert_eq!(engine.snapshot().unwrap().files.len(), 4);
    }
    assert_eq!(
        std::fs::read_dir(root.path().join("cache"))
            .unwrap()
            .count(),
        0
    );
    let production = tempfile::tempdir().unwrap();
    let engine = Engine::open(production.path(), false).unwrap();
    let snap = engine.snapshot().unwrap();
    assert_eq!(snap.status, "signed_out");
    assert!(snap.files.is_empty());
    assert!(
        !call(
            &engine,
            "mock",
            Command::MockImport {
                name: "a".into(),
                content: "b".into()
            }
        )
        .ok
    );
    assert!(Engine::open(production.path(), true).is_err());
}
#[test]
fn version_idempotency_and_persistence() {
    let root = tempfile::tempdir().unwrap();
    let engine = Engine::open(root.path(), true).unwrap();
    let rejected = engine.handle(Request {
        version: 999,
        id: "bad-version".into(),
        command: Command::SetPaused { paused: true },
    });
    assert!(!rejected.ok);
    assert!(!rejected.snapshot.paused);
    let first = call(&engine, "stable", Command::SetPaused { paused: true });
    let replay = call(&engine, "stable", Command::SetPaused { paused: true });
    assert_eq!(first.snapshot.revision, replay.snapshot.revision);
    assert!(!call(&engine, "stable", Command::SetPaused { paused: false }).ok);
    drop(engine);
    assert!(
        Engine::open(root.path(), true)
            .unwrap()
            .snapshot()
            .unwrap()
            .paused
    );
}
#[test]
fn engine_and_tray_locks_are_shared_between_editions() {
    let root = tempfile::tempdir().unwrap();
    let engine = ipc::lock(root.path(), "engine").unwrap();
    let tray = ipc::lock(root.path(), "tray").unwrap();
    assert!(ipc::lock(root.path(), "engine").is_err());
    assert!(ipc::lock(root.path(), "tray").is_err());
    drop(engine);
    drop(tray);
    assert!(ipc::lock(root.path(), "engine").is_ok());
}
#[tokio::test]
async fn framed_protocol_rejects_oversized_and_truncated_frames() {
    use tokio::io::AsyncWriteExt;
    let (mut sender, mut receiver) = tokio::io::duplex(16);
    sender.write_u32_le(MAX_FRAME as u32 + 1).await.unwrap();
    assert!(ipc::read_frame(&mut receiver).await.is_err());
    sender.write_u32_le(8).await.unwrap();
    sender.write_all(b"x").await.unwrap();
    drop(sender);
    assert!(ipc::read_frame(&mut receiver).await.is_err());
}
