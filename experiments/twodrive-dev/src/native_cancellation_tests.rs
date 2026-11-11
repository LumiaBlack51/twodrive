//! Explicit, disposable Windows roots only. No user profile or live provider.
use super::*;
use crate::{
    local_files::tests::MockProvider, model::CloudBackend, native_control::NativeControl,
    native_runtime_status::NativeRuntimeStatus, provider::Backend,
};
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use twodrive_core::IndexedItem;

const COUNT: usize = 2048;
fn fixture() -> (
    tempfile::TempDir,
    NativeCoordinator,
    MockProvider,
    Database,
    PathBuf,
) {
    let root = PathBuf::from(
        std::env::var_os("TWODRIVE_APPROVED_CANCELLATION_ROOT")
            .expect("explicit NEW generated root"),
    );
    assert!(
        root.is_absolute()
            && !root.exists()
            && root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("twodrive-cfapi-fixture-office-")
    );
    let private = tempfile::tempdir().unwrap();
    let cache = private.path().join("cache");
    std::fs::create_dir(&cache).unwrap();
    let plan = NamespacePlan::build(
        &[IndexedItem {
            id: "root".into(),
            parent: None,
            name: "root".into(),
            kind: "folder".into(),
            size: 0,
            etag: "root-v1".into(),
            modified: None,
            deleted: false,
        }],
        "root",
        100,
        twodrive_local_files::names::ordinal_ignorecase_cmp,
    )
    .unwrap();
    let provider = MockProvider::empty();
    let db = Database::new(private.path().join("native.sqlite3"));
    let bridge = LocalFileBridge::open_bound(
        db.clone(),
        &cache,
        Backend::new(provider.clone()),
        "generated-cancellation",
    )
    .unwrap();
    let coordinator = NativeCoordinator::create_new(
        bridge,
        &plan,
        &root,
        b"twodrive-dev-native-v1:generated-office-only",
        &root.with_extension("receipt.json"),
    )
    .unwrap();
    (private, coordinator, provider, db, root)
}
fn resume(
    old: NativeCoordinator,
    private: &Path,
    provider: MockProvider,
    db: Database,
) -> NativeCoordinator {
    let root = old.root().to_owned();
    let receipt = old.receipt().clone();
    drop(old);
    let bridge = LocalFileBridge::open_bound(
        db,
        &private.join("cache"),
        Backend::new(provider),
        "generated-cancellation",
    )
    .unwrap();
    let value = NativeCoordinator::resume(bridge, &root, &receipt).unwrap();
    assert_eq!(value.receipt().file_id, receipt.file_id);
    value
}
fn cleanup(mut coordinator: NativeCoordinator, checkpoint: PathBuf) {
    let root = coordinator.root().to_owned();
    let mut entries = coordinator.owner.inventory().unwrap();
    entries.sort_by_key(|e| std::cmp::Reverse(e.relative_path.components().count()));
    assert_eq!(entries.len(), COUNT);
    for entry in entries {
        assert!(entry.directory && entry.relative_path.components().count() == 1);
        std::fs::remove_dir(coordinator.root().join(entry.relative_path)).unwrap();
    }
    coordinator.retire_empty_generated_fixture().unwrap();
    drop(coordinator);
    std::fs::remove_dir(root).unwrap();
    std::fs::remove_file(checkpoint).unwrap();
}

#[test]
#[ignore = "explicit NEW generated CFAPI root; 2048 local objects, heartbeat, stop during receipt and restart"]
fn large_local_application_stops_between_receipts_and_resumes_without_duplicates() {
    let (private, mut coordinator, provider, db, root) = fixture();
    for i in 0..COUNT {
        std::fs::create_dir(coordinator.root().join(format!("local-{i:04}"))).unwrap();
    }
    let control = NativeControl::default();
    coordinator.set_control(control.clone());
    let status = NativeRuntimeStatus::start(
        crate::state::State::new(private.path().join("status")).unwrap(),
        control.clone(),
        Duration::from_millis(100),
        || false,
    )
    .unwrap();
    status
        .publish(serde_json::json!({"phase":"working","stage":"local"}))
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let stop = control.clone();
    provider.on_directory_create(Arc::new(move || {
        let n = observed.fetch_add(1, Ordering::SeqCst) + 1;
        // A slow provider receipt must not freeze heartbeat, nor be interrupted
        // before the journal records that receipt and its native acknowledgement.
        if n == 1 {
            std::thread::sleep(Duration::from_millis(350));
        }
        if n == 8 {
            stop.request_stop();
        }
    }));
    let started = Instant::now();
    let result = coordinator.synchronize_once();
    let stopped = result.is_err();
    let stop_elapsed = started.elapsed();
    let first_calls = calls.load(Ordering::SeqCst);
    let journal = coordinator.bridge.journal();
    let publications = journal
        .native_projections(coordinator.binding().unwrap())
        .unwrap();
    assert_eq!(publications.len(), first_calls);
    assert!(publications.iter().all(|p| p.state == "published"));
    let report: serde_json::Value = serde_json::from_slice(
        &std::fs::read(private.path().join("status/native-status.json")).unwrap(),
    )
    .unwrap();
    let heartbeat = report["heartbeat"].as_u64().unwrap();
    assert_eq!(report["phase"], "working");
    coordinator.disconnect().unwrap();
    status
        .finish(serde_json::json!({"phase":"stopped"}))
        .unwrap();
    let mut coordinator = resume(coordinator, private.path(), provider.clone(), db);
    let cold_setup_rejected = coordinator.verify_desktop_start(false).is_err();
    let ready_restart_accepted = coordinator.verify_desktop_start(true).is_ok();
    // Aborting a traversal returns no partial set and cannot authorize deletion.
    let mut checked = 0;
    assert!(
        coordinator
            .owner
            .inventory_controlled(&mut || {
                checked += 1;
                if checked == 128 {
                    Err("generated stop".into())
                } else {
                    Ok(())
                }
            })
            .is_err()
    );
    let progress = coordinator.synchronize_once().unwrap();
    assert_eq!(progress.created, COUNT - first_calls, "{progress:?}");
    assert_eq!(progress.deferred, 0, "{progress:?}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        COUNT,
        "restart duplicated a completed mutation"
    );
    let publications = coordinator
        .bridge
        .journal()
        .native_projections(coordinator.binding().unwrap())
        .unwrap();
    assert_eq!(publications.len(), COUNT);
    for p in &publications {
        assert_eq!(p.state, "published");
        assert_eq!(
            provider.get_metadata(&p.cloud_id).unwrap().unwrap().path,
            p.path
        );
    }
    assert!(coordinator.synchronize_once().unwrap().idle);
    cleanup(coordinator, root.with_extension("receipt.json"));
    assert!(stopped, "batch ignored stop: {result:?}");
    assert_eq!(
        first_calls, 8,
        "stop must prevent the next provider mutation"
    );
    assert!(stop_elapsed < Duration::from_secs(10));
    assert!(
        cold_setup_rejected,
        "first setup must reject unprocessed ordinary additions"
    );
    assert!(
        ready_restart_accepted,
        "a retained ready root must resume its pending work"
    );
    assert!(heartbeat >= 4, "{report}");
}

#[test]
#[ignore = "explicit NEW generated CFAPI root; 2048 remote publications, in-flight stop and restart"]
fn large_remote_application_stops_after_durable_publications_and_resumes_same_root() {
    let (private, mut coordinator, provider, db, root) = fixture();
    let control = NativeControl::default();
    coordinator.set_control(control.clone());
    let metadata = (0..COUNT)
        .map(|i| {
            MetadataEntry::new_dir(format!("remote-{i:04}"), format!("/remote-{i:04}"), 1, "v1")
        })
        .collect::<Vec<_>>();
    let journal = coordinator.bridge.journal().clone();
    let binding = coordinator.binding().unwrap().to_owned();
    let stop = control.clone();
    let observer = std::thread::spawn(move || {
        let started = Instant::now();
        loop {
            let n = journal
                .native_projections(&binding)
                .unwrap()
                .iter()
                .filter(|p| p.state == "published")
                .count();
            if n >= 32 {
                stop.request_stop();
                return n;
            }
            assert!(
                started.elapsed() < Duration::from_secs(15),
                "no durable progress before stop"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    let started = Instant::now();
    let result = coordinator.apply_remote_metadata(&metadata);
    let observed = observer.join().unwrap();
    let stopped = result.is_err();
    let stop_elapsed = started.elapsed();
    let published = coordinator
        .bridge
        .journal()
        .native_projections(coordinator.binding().unwrap())
        .unwrap();
    let first_published = published.len();
    assert!(published.iter().all(|p| p.state == "published"));
    let old = published
        .iter()
        .map(|p| (p.local_id.clone(), p.native_file_id.clone()))
        .collect::<HashMap<_, _>>();
    coordinator.disconnect().unwrap();
    let mut coordinator = resume(coordinator, private.path(), provider, db);
    let progress = coordinator.apply_remote_metadata(&metadata).unwrap();
    assert_eq!(progress.created, COUNT - published.len(), "{progress:?}");
    assert_eq!(progress.deferred, 0, "{progress:?}");
    let publications = coordinator
        .bridge
        .journal()
        .native_projections(coordinator.binding().unwrap())
        .unwrap();
    assert_eq!(publications.len(), COUNT);
    for p in &publications {
        if let Some(id) = old.get(&p.local_id) {
            assert_eq!(&p.native_file_id, id);
        }
    }
    assert_eq!(
        coordinator
            .apply_remote_metadata(&metadata)
            .unwrap()
            .created,
        0
    );
    coordinator.verify_bootstrap_inventory().unwrap();
    cleanup(coordinator, root.with_extension("receipt.json"));
    assert!(
        stopped,
        "batch ignored stop after {observed} objects: {result:?}"
    );
    assert!(stop_elapsed < Duration::from_secs(15));
    assert!((32..COUNT).contains(&first_published));
}
