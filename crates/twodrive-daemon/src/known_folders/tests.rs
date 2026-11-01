use super::activity::unique_suffix;
use super::scan::baseline_known_folder_path;
use super::state::{FileSnapshot, KnownFolderRoot, KnownFolderUploadJob, PendingKnownFolderUpload};
use super::uploads::process_known_folder_uploads;
use super::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;
use twodrive_backend::CloudBackend;
use twodrive_backend::{DeltaResult, MockBackend};
use twodrive_core::{AppPaths, Config, Database};
use twodrive_core::{FileRecord, MetadataEntry};

#[derive(Debug)]
struct DelayedBackend {
    inner: MockBackend,
    active: AtomicUsize,
    max_active: AtomicUsize,
    slow_release: Option<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>,
}

impl DelayedBackend {
    fn new() -> Self {
        Self {
            inner: MockBackend::new(),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            slow_release: None,
        }
    }
}

impl CloudBackend for DelayedBackend {
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        self.inner.list_all()
    }

    fn list_delta(&self, delta_link: Option<&str>) -> anyhow::Result<DeltaResult> {
        self.inner.list_delta(delta_link)
    }

    fn download(&self, remote_id: &str) -> anyhow::Result<Vec<u8>> {
        self.inner.download(remote_id)
    }

    fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        self.inner.upload(path, content)
    }

    fn upload_file_with_version(
        &self,
        path: &str,
        source_path: &Path,
        remote_id: Option<&str>,
        if_match: Option<&str>,
        on_progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        if path.contains("slow")
            && let Some(release) = &self.slow_release
        {
            release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(15))?;
        }
        thread::sleep(if path.contains("slow") {
            Duration::from_millis(800)
        } else {
            Duration::from_millis(100)
        });
        let result = self.inner.upload_file_with_version(
            path,
            source_path,
            remote_id,
            if_match,
            on_progress,
        );
        self.active.fetch_sub(1, Ordering::SeqCst);
        result
    }

    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        self.inner.create_folder(path)
    }

    fn rename(&self, remote_id: &str, new_path: &str) -> anyhow::Result<MetadataEntry> {
        self.inner.rename(remote_id, new_path)
    }

    fn delete(&self, remote_id: &str) -> anyhow::Result<()> {
        self.inner.delete(remote_id)
    }
}

pub(super) fn test_paths(name: &str) -> (PathBuf, AppPaths) {
    let root = std::env::temp_dir().join(format!(
        "twodrive-daemon-{name}-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let paths = AppPaths {
        config_dir: root.join("config"),
        config_path: root.join("config/config.toml"),
        data_dir: root.join("data"),
        cache_dir: root.join("data/cache"),
        db_path: root.join("data/test.sqlite3"),
        mount_dir: root.join("mount"),
        token_path: root.join("config/tokens.json"),
    };
    (root, paths)
}

#[test]
fn restart_retries_changed_backup_without_caching_or_propagating_deletion() {
    let (root, paths) = test_paths("retry-changed-backup");
    paths.ensure().unwrap();
    let db = Database::new(paths.db_path.clone());
    db.init().unwrap();
    let backend = twodrive_backend::MockBackend::new();
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    let file = source.join("photo.bin");
    fs::write(&file, b"old").unwrap();
    let mut state = KnownFolderState::default();
    state.pending.insert(
        file.to_string_lossy().into_owned(),
        PendingKnownFolderUpload {
            remote_path: "/Pictures/photo.bin".into(),
            snapshot: FileSnapshot::from_metadata(&fs::metadata(&file).unwrap()),
        },
    );
    state.save(&paths.data_dir).unwrap();
    fs::write(&file, b"new content after interruption").unwrap();
    let mut state = KnownFolderState::load(&paths.data_dir).unwrap();
    retry_known_folder_uploads(&paths, &backend, &db, &Config::default(), &mut state).unwrap();
    assert!(state.pending.is_empty());
    let record = db.get_by_path("/Pictures/photo.bin").unwrap().unwrap();
    assert_eq!(
        backend
            .download(record.cloud_remote_id.as_deref().unwrap())
            .unwrap(),
        b"new content after interruption"
    );
    assert!(record.cache_path.is_none());
    assert_eq!(fs::read_dir(&paths.cache_dir).unwrap().count(), 0);
    fs::remove_file(&file).unwrap();
    sync_known_folder_root(
        &paths,
        &backend,
        &db,
        &Config::default(),
        &KnownFolderRoot {
            local: source,
            remote: "/Pictures".into(),
        },
        &mut state,
    )
    .unwrap();
    assert!(
        backend
            .get_metadata_by_path("/Pictures/photo.bin")
            .unwrap()
            .is_some()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn known_folder_state_persists_pending_uploads() {
    let (root, paths) = test_paths("pending-state");
    let mut state = KnownFolderState::default();
    state.pending.insert(
        "/local/a.txt".to_string(),
        PendingKnownFolderUpload {
            remote_path: "/remote/a.txt".to_string(),
            snapshot: FileSnapshot {
                size: 12,
                modified_unix: 34,
                modified_nanos: 0,
            },
        },
    );
    state.save(&paths.data_dir).unwrap();

    let loaded = KnownFolderState::load(&paths.data_dir).unwrap();
    let pending = loaded.pending.get("/local/a.txt").unwrap();
    assert_eq!(pending.remote_path, "/remote/a.txt");
    assert_eq!(pending.snapshot.size, 12);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn mount_source_is_not_uploaded_into_itself() {
    let (root, paths) = test_paths("mount-source");
    paths.ensure().unwrap();
    fs::write(
        paths.mount_dir.join("photo.bin"),
        b"do not upload a mounted copy",
    )
    .unwrap();
    let db = Database::new(paths.db_path.clone());
    db.init().unwrap();
    let backend = MockBackend::new();
    let mut state = KnownFolderState::default();
    sync_known_folder_root(
        &paths,
        &backend,
        &db,
        &Config::default(),
        &KnownFolderRoot {
            local: paths.mount_dir.clone(),
            remote: "/Pictures".into(),
        },
        &mut state,
    )
    .unwrap();
    assert!(
        backend
            .get_metadata_by_path("/Pictures/photo.bin")
            .unwrap()
            .is_none()
    );
    assert!(state.files.is_empty());
    assert!(state.pending.is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn missing_pending_known_folder_file_is_pruned() {
    let (root, paths) = test_paths("missing-pending");
    let missing = root.join("no-longer-present.txt");
    let key = missing.to_string_lossy().into_owned();
    let mut state = KnownFolderState::default();
    state.files.insert(
        key.clone(),
        FileSnapshot {
            size: 5,
            modified_unix: 10,
            modified_nanos: 0,
        },
    );
    state.pending.insert(
        key.clone(),
        PendingKnownFolderUpload {
            remote_path: "/Known/no-longer-present.txt".to_string(),
            snapshot: FileSnapshot {
                size: 5,
                modified_unix: 10,
                modified_nanos: 0,
            },
        },
    );

    assert_eq!(state.prune_missing_pending(), 1);
    assert!(!state.pending.contains_key(&key));
    assert!(!state.files.contains_key(&key));
    fs::remove_dir_all(paths.data_dir.parent().unwrap()).unwrap_or(());
}

#[test]
fn first_start_baseline_preserves_tracked_snapshots_and_adds_history() {
    let (root, _paths) = test_paths("baseline");
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    let tracked = source.join("tracked.txt");
    let historical = source.join("historical.txt");
    fs::write(&tracked, b"changed since last upload").unwrap();
    fs::write(&historical, b"old local history").unwrap();

    let mut config = Config::default();
    config.known_folders.exclude_suffixes = vec![".tmp".to_string()];
    let mut state = KnownFolderState::default();
    let old_snapshot = FileSnapshot {
        size: 3,
        modified_unix: 4,
        modified_nanos: 0,
    };
    state
        .files
        .insert(tracked.to_string_lossy().into_owned(), old_snapshot.clone());

    baseline_known_folder_path(&config, &source, &mut state).unwrap();

    assert_eq!(
        state.files.get(&tracked.to_string_lossy().into_owned()),
        Some(&old_snapshot)
    );
    assert_eq!(
        state.files.get(&historical.to_string_lossy().into_owned()),
        Some(&FileSnapshot::from_metadata(
            &fs::metadata(&historical).unwrap()
        ))
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn known_folder_small_files_upload_concurrently() {
    let (root, paths) = test_paths("parallel");
    paths.ensure().unwrap();
    let db = Database::new(paths.db_path.clone());
    db.init().unwrap();
    let backend = DelayedBackend::new();
    let mut config = Config::default();
    config.power.ac_upload_concurrency = 4;
    let mut state = KnownFolderState::default();
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir).unwrap();
    let mut jobs = Vec::new();
    for index in 0..4 {
        let local_path = source_dir.join(format!("small-{index}.txt"));
        fs::write(&local_path, format!("small-{index}")).unwrap();
        jobs.push(KnownFolderUploadJob {
            snapshot: FileSnapshot::from_metadata(&fs::metadata(&local_path).unwrap()),
            local_path,
            remote_path: format!("/Parallel/small-{index}.txt"),
        });
    }

    process_known_folder_uploads(&paths, &backend, &db, &config, jobs, &mut state).unwrap();

    assert!(state.pending.is_empty());
    assert_eq!(state.files.len(), 4);
    assert!(backend.max_active.load(Ordering::SeqCst) >= 2);
    for index in 0..4 {
        let record: FileRecord = db
            .get_by_path(&format!("/Parallel/small-{index}.txt"))
            .unwrap()
            .unwrap();
        assert_eq!(record.metadata.size, format!("small-{index}").len() as u64);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn completed_known_folder_job_is_persisted_while_slower_job_runs() {
    let (root, paths) = test_paths("incremental-results");
    paths.ensure().unwrap();
    let db = Database::new(paths.db_path.clone());
    db.init().unwrap();
    let mut config = Config::default();
    config.power.ac_upload_concurrency = 2;
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir).unwrap();
    let fast_path = source_dir.join("fast.txt");
    let slow_path = source_dir.join("slow.txt");
    fs::write(&fast_path, b"fast").unwrap();
    fs::write(&slow_path, b"slow").unwrap();
    let jobs = vec![
        KnownFolderUploadJob {
            snapshot: FileSnapshot::from_metadata(&fs::metadata(&fast_path).unwrap()),
            local_path: fast_path.clone(),
            remote_path: "/Parallel/fast.txt".to_string(),
        },
        KnownFolderUploadJob {
            snapshot: FileSnapshot::from_metadata(&fs::metadata(&slow_path).unwrap()),
            local_path: slow_path.clone(),
            remote_path: "/Parallel/slow.txt".to_string(),
        },
    ];
    let worker_paths = paths.clone();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        // Deliberately exceed the old 500ms polling window to model a busy CI runner.
        thread::sleep(Duration::from_millis(600));
        let mut backend = DelayedBackend::new();
        backend.slow_release = Some(std::sync::Mutex::new(release_rx));
        let mut state = KnownFolderState::default();
        process_known_folder_uploads(&worker_paths, &backend, &db, &config, jobs, &mut state)
            .map(|()| state)
    });

    let fast_key = fast_path.to_string_lossy().into_owned();
    let slow_key = slow_path.to_string_lossy().into_owned();
    let mut observed_incremental_commit = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if let Ok(state) = KnownFolderState::load(&paths.data_dir)
            && state.files.contains_key(&fast_key)
            && !state.pending.contains_key(&fast_key)
            && state.pending.contains_key(&slow_key)
        {
            observed_incremental_commit = true;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }

    // Release and join even when the observation failed; never leave a detached upload thread.
    let _ = release_tx.send(());
    let final_state = worker.join().unwrap().unwrap();
    assert!(
        observed_incremental_commit,
        "the fast result should be durable before the slow upload finishes"
    );
    assert!(final_state.pending.is_empty());
    assert_eq!(final_state.files.len(), 2);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn dependency_subtree_is_excluded_from_scan_events_and_old_queue() {
    let (root, paths) = test_paths("excluded-dependencies");
    paths.ensure().unwrap();
    let source = root.join("source");
    let dependency = source.join("app/node_modules/pkg/index.js");
    fs::create_dir_all(dependency.parent().unwrap()).unwrap();
    fs::write(&dependency, b"dependency").unwrap();
    let source_file = source.join("app/index.js");
    fs::write(&source_file, b"source").unwrap();
    let config = Config::default();
    let mapping = KnownFolderRoot {
        local: source,
        remote: "/Code".into(),
    };
    let mut state = KnownFolderState::default();
    let mut jobs = Vec::new();
    super::scan::collect_local_path(&config, &mapping, &mapping.local, &state, &mut jobs).unwrap();
    assert_eq!(
        jobs.len(),
        1,
        "recursive scan must prune dependency subtrees"
    );
    assert_eq!(jobs[0].local_path, source_file);
    let event = notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::File))
        .add_path(dependency.clone());
    let mut pending = HashSet::new();
    assert!(!queue_addition_event(
        &event,
        &[mapping],
        &config,
        &mut pending
    ));
    let snapshot = FileSnapshot::from_metadata(&fs::metadata(&dependency).unwrap());
    state.pending.insert(
        dependency.to_string_lossy().into_owned(),
        PendingKnownFolderUpload {
            remote_path: "/Code/app/node_modules/pkg/index.js".into(),
            snapshot,
        },
    );
    state.save(&paths.data_dir).unwrap();
    let db = Database::new(paths.db_path.clone());
    db.init().unwrap();
    let backend = MockBackend::new();
    retry_known_folder_uploads(&paths, &backend, &db, &config, &mut state).unwrap();
    assert!(
        backend
            .get_metadata_by_path("/Code/app/node_modules/pkg/index.js")
            .unwrap()
            .is_none(),
        "legacy queued exclusions must not upload"
    );
    assert!(dependency.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn hidden_ancestor_event_cannot_bypass_scan_filter() {
    let mapping = KnownFolderRoot {
        local: "/source".into(),
        remote: "/Code".into(),
    };
    let event = notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::File))
        .add_path("/source/app/.git/objects/abc".into());
    assert!(!queue_addition_event(
        &event,
        &[mapping],
        &Config::default(),
        &mut HashSet::new()
    ));
}

#[test]
fn event_burst_coalesces_and_has_a_fixed_pending_bound() {
    let mappings = [KnownFolderRoot {
        local: "/source".into(),
        remote: "/Code".into(),
    }];
    let mut pending = HashSet::new();
    for index in 0..4096 {
        let event = notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::File))
            .add_path(format!("/source/app/{index}.txt").into());
        queue_addition_event(&event, &mappings, &Config::default(), &mut pending);
        assert!(pending.len() <= 1024, "pending events must be bounded");
    }
    let event = notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::Folder))
        .add_path("/source".into());
    queue_addition_event(&event, &mappings, &Config::default(), &mut pending);
    assert_eq!(pending, HashSet::from([PathBuf::from("/source")]));
}

#[derive(Debug)]
struct ExistingDirectoryBackend {
    inner: MockBackend,
    creates: AtomicUsize,
    lookups: AtomicUsize,
}

impl CloudBackend for ExistingDirectoryBackend {
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        self.inner.list_all()
    }
    fn download(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        self.inner.download(id)
    }
    fn upload(&self, path: &str, data: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        self.inner.upload(path, data)
    }
    fn create_folder(&self, _path: &str) -> anyhow::Result<MetadataEntry> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("HTTP 409 code=nameAlreadyExists");
    }
    fn get_metadata_by_path(&self, path: &str) -> anyhow::Result<Option<MetadataEntry>> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        self.inner.get_metadata_by_path(path)
    }
    fn rename(&self, id: &str, path: &str) -> anyhow::Result<MetadataEntry> {
        self.inner.rename(id, path)
    }
    fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.inner.delete(id)
    }
}

#[test]
fn existing_remote_directory_is_resolved_once_and_file_is_rejected() {
    let (root, paths) = test_paths("existing-remote-directory");
    paths.ensure().unwrap();
    let db = Database::new(paths.db_path.clone());
    db.init().unwrap();
    let backend = ExistingDirectoryBackend {
        inner: MockBackend::new(),
        creates: AtomicUsize::new(0),
        lookups: AtomicUsize::new(0),
    };
    backend.inner.create_folder("/Code").unwrap();
    for _ in 0..8 {
        super::uploads::ensure_remote_dir(&backend, &db, "/Code").unwrap();
    }
    assert_eq!(backend.lookups.load(Ordering::SeqCst), 1);
    assert!(backend.creates.load(Ordering::SeqCst) <= 1);
    backend.inner.upload("/occupied", b"file".to_vec()).unwrap();
    assert!(super::uploads::ensure_remote_dir(&backend, &db, "/occupied/child").is_err());
    assert!(db.get_by_path("/occupied/child").unwrap().is_none());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_state_migration_and_incremental_completion_preserve_unrelated_rows() {
    let (root, paths) = test_paths("state-migration");
    paths.ensure().unwrap();
    let mut legacy = KnownFolderState {
        baseline_initialized: true,
        ..KnownFolderState::default()
    };
    for index in 0..1000 {
        legacy.files.insert(
            format!("/source/old-{index}"),
            FileSnapshot {
                size: 10,
                modified_unix: 20,
                modified_nanos: 0,
            },
        );
    }
    let local = "/source/new".to_string();
    let snapshot = FileSnapshot {
        size: 30,
        modified_unix: 40,
        modified_nanos: 0,
    };
    legacy.pending.insert(
        local.clone(),
        PendingKnownFolderUpload {
            remote_path: "/remote/new".into(),
            snapshot: snapshot.clone(),
        },
    );
    let original = serde_json::to_vec(&legacy).unwrap();
    fs::write(paths.data_dir.join("known-folders-state.json"), &original).unwrap();
    let mut state = KnownFolderState::load(&paths.data_dir).unwrap();
    assert!(state.baseline_initialized);
    assert_eq!(state.files.len(), 1000);
    let before = state.storage.as_ref().unwrap().total_changes();
    state
        .complete_upload(local.clone(), snapshot.clone())
        .unwrap();
    let changed = state.storage.as_ref().unwrap().total_changes() - before;
    assert_eq!(
        changed, 2,
        "completion must change only its snapshot and queue row"
    );
    assert_eq!(
        fs::read(paths.data_dir.join("known-folders-state.json")).unwrap(),
        original
    );
    drop(state);
    let state = KnownFolderState::load(&paths.data_dir).unwrap();
    assert_eq!(state.files.get(&local), Some(&snapshot));
    assert!(state.pending.is_empty());
    assert_eq!(state.files.len(), 1001);
    drop(state);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rate_limited_uploads_remain_durable_and_retry_latest_source() {
    let (root, paths) = test_paths("deferred-upload-rate");
    paths.ensure().unwrap();
    let db = Database::new(paths.db_path.clone());
    db.init().unwrap();
    let backend = MockBackend::new();
    let mut config = Config::default();
    config.known_folders.upload_rate_window = "1s".into();
    config.known_folders.max_project_uploads_per_window = 2;
    let mut state = KnownFolderState::default();
    config.known_folders.upload_rate_roots = vec![root.to_string_lossy().into_owned()];
    for generation in 0..3 {
        let mut jobs = Vec::new();
        for index in 0..4 {
            let local = root.join(format!("{index}.txt"));
            fs::write(&local, vec![b'a'; 8 + generation]).unwrap();
            jobs.push(KnownFolderUploadJob {
                snapshot: FileSnapshot::from_metadata(&fs::metadata(&local).unwrap()),
                local_path: local,
                remote_path: format!("/Downloads/project/{index}.txt"),
            });
        }
        process_known_folder_uploads(&paths, &backend, &db, &config, jobs, &mut state).unwrap();
        if generation < 2 {
            assert!(
                state.pending.is_empty(),
                "first uploads and ordinary first edits must remain unrestricted"
            );
        }
    }
    assert_eq!(state.files.len(), 4);
    assert_eq!(state.pending.len(), 2);
    assert_eq!(
        KnownFolderState::load(&paths.data_dir)
            .unwrap()
            .pending
            .len(),
        2
    );
    let (local, remote) = state
        .pending
        .iter()
        .next()
        .map(|(local, p)| (local.clone(), p.remote_path.clone()))
        .unwrap();
    fs::write(&local, b"latest contents while deferred").unwrap();
    thread::sleep(Duration::from_millis(1100));
    retry_known_folder_uploads(&paths, &backend, &db, &config, &mut state).unwrap();
    assert!(state.pending.is_empty());
    let uploaded = backend.get_metadata_by_path(&remote).unwrap().unwrap();
    assert_eq!(
        backend.download(&uploaded.remote_id).unwrap(),
        b"latest contents while deferred"
    );
    drop(state);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn same_second_same_size_changes_have_distinct_snapshots() {
    let (root, paths) = test_paths("subsecond-save");
    paths.ensure().unwrap();
    let local = root.join("code.txt");
    let mut snapshots = Vec::new();
    for nanos in [100_000_000, 200_000_000] {
        fs::write(&local, b"same size").unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&local)
            .unwrap()
            .set_times(
                fs::FileTimes::new()
                    .set_modified(std::time::UNIX_EPOCH + Duration::new(1_790_000_000, nanos)),
            )
            .unwrap();
        snapshots.push(FileSnapshot::from_metadata(&fs::metadata(&local).unwrap()));
    }
    assert_eq!(
        (snapshots[0].size, snapshots[0].modified_unix),
        (snapshots[1].size, snapshots[1].modified_unix)
    );
    assert_ne!(snapshots[0], snapshots[1]);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn watcher_ingress_drops_exclusions_and_marks_both_overflow_kinds() {
    use super::events::{MAX_PENDING_PATHS, enqueue_watch_event};
    use std::sync::atomic::AtomicBool;
    let config = Config::default();
    let roots = [KnownFolderRoot {
        local: "/source".into(),
        remote: "/Code".into(),
    }];
    let (tx, rx) = std::sync::mpsc::sync_channel(MAX_PENDING_PATHS);
    let overflow = AtomicBool::new(false);
    let create = |path: PathBuf| {
        notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::File))
            .add_path(path)
    };
    enqueue_watch_event(
        &tx,
        &overflow,
        &roots,
        &config,
        Ok(create("/source/node_modules/pkg/index.js".into())),
    );
    assert!(rx.try_recv().is_err());
    for index in 0..MAX_PENDING_PATHS + 1 {
        enqueue_watch_event(
            &tx,
            &overflow,
            &roots,
            &config,
            Ok(create(format!("/source/{index}").into())),
        );
    }
    assert!(overflow.swap(false, Ordering::AcqRel));
    assert_eq!(rx.try_iter().count(), MAX_PENDING_PATHS);
    enqueue_watch_event(
        &tx,
        &overflow,
        &roots,
        &config,
        Ok(notify::Event::new(notify::EventKind::Other).set_flag(notify::event::Flag::Rescan)),
    );
    assert!(overflow.load(Ordering::Acquire));
    let save = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Data(
        notify::event::DataChange::Content,
    )))
    .add_path("/source/code.rs".into());
    assert!(queue_addition_event(
        &save,
        &roots,
        &config,
        &mut HashSet::new()
    ));
}
