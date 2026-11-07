use super::*;
use std::sync::mpsc::Receiver;
use std::time::Instant;

pub(super) fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "deletion did not settle within the test deadline"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[derive(Debug)]
struct GatedUploadBackend {
    inner: MockBackend,
    started: Sender<()>,
    release: Mutex<Receiver<()>>,
    uploads: AtomicUsize,
    reject_first: bool,
    completed: Mutex<Vec<MetadataEntry>>,
}

impl CloudBackend for GatedUploadBackend {
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        let completed = self.completed.lock().unwrap();
        Ok(self
            .inner
            .list_all()?
            .into_iter()
            .map(|entry| {
                completed
                    .iter()
                    .rev()
                    .find(|uploaded| uploaded.remote_id == entry.remote_id)
                    .cloned()
                    .unwrap_or(entry)
            })
            .collect())
    }
    fn download(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        self.inner.download(id)
    }
    fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        self.inner.upload(path, content)
    }
    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        self.inner.create_folder(path)
    }
    fn rename(&self, id: &str, path: &str) -> anyhow::Result<MetadataEntry> {
        self.inner.rename(id, path)
    }
    fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.inner.delete(id)
    }
    fn is_conflict_error(&self, _: &anyhow::Error) -> bool {
        self.reject_first
    }
    fn upload_file_with_version(
        &self,
        path: &str,
        source: &Path,
        id: Option<&str>,
        etag: Option<&str>,
        progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        let generation = self.uploads.fetch_add(1, Ordering::SeqCst);
        if generation == 0 {
            self.started.send(())?;
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(20))?;
            if self.reject_first {
                anyhow::bail!("injected version conflict");
            }
        }
        // MockBackend normally derives IDs from paths. Give these generations
        // distinct cloud IDs so delayed deletion of the old one is observable.
        let storage_path = format!("/test-upload-generation-{generation}");
        let stored =
            self.inner
                .upload_file_with_version(&storage_path, source, id, etag, progress)?;
        let entry = MetadataEntry::new_file(
            stored.remote_id,
            path,
            stored.size,
            stored.modified_unix,
            stored.etag,
        );
        self.completed.lock().unwrap().push(entry.clone());
        Ok(entry)
    }
}

// Release a blocked worker even if an assertion fails, before the filesystem's
// Drop joins its workers. Every barrier also has a bounded timeout.
struct ReleaseUpload(Sender<()>);
impl Drop for ReleaseUpload {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

#[test]
fn closed_dirty_file_unlinks_without_uploading() {
    let mut test = TestFs::new("closed-dirty-delete");
    let (ino, fh, _) = test
        .fs
        .create_upload(ROOT_INO, OsStr::new("index.lock"), libc::O_CREAT)
        .unwrap();
    let handle = &test.fs.write_handles[&fh];
    let local_id = handle.record_remote_id.clone();
    let cache = handle.cache_path.clone();
    fs::write(&cache, b"temporary lock").unwrap();
    // A closed generation queued for recovery, before any worker has claimed it.
    test.fs.db.mark_dirty_with_size(&local_id, 14).unwrap();
    test.fs.write_handles.remove(&fh);
    let record = test.fs.db.get_by_remote_id(&local_id).unwrap().unwrap();
    assert!(
        !test
            .fs
            .unlink_record_with_open_handles(ino, &record)
            .unwrap()
    );
    test.fs.delete_record(ino, &record).unwrap();
    assert!(test.fs.db.get_by_path("/index.lock").unwrap().is_none());
    assert!(test.fs.inodes.ino_for_path("/index.lock").is_none());
    assert!(
        !recover_dirty_record(
            &test.fs.db,
            test.fs.backend.as_ref(),
            record,
            &mut |_, _| Ok(())
        )
        .unwrap()
    );
    wait_for(|| test.fs.db.pending_deletes().unwrap().is_empty());
    assert!(!cache.exists());
    assert!(
        test.fs
            .backend
            .get_metadata_by_path("/index.lock")
            .unwrap()
            .is_none()
    );
}

#[test]
fn closed_dirty_cloud_file_deletes_without_uploading_edits() {
    let root = test_root("closed-cloud-delete");
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("db"));
    db.init().unwrap();
    let backend = FailingBackend::new(true, false);
    sync_metadata(&db, &backend).unwrap();
    let mut fs = TwoDriveFs::new(db, root.join("cache"), backend).unwrap();
    let old = fs.db.get_by_path("/README-cloud.txt").unwrap().unwrap();
    let (ino, fh, _) = fs.create_overwrite_upload(old, false).unwrap();
    let cache = fs.write_handles[&fh].cache_path.clone();
    std::fs::write(&cache, b"discarded edits").unwrap();
    let id = fs.write_handles[&fh].record_remote_id.clone();
    fs.db.mark_dirty_with_size(&id, 15).unwrap();
    fs.write_handles.remove(&fh);
    let record = fs.db.get_by_remote_id(&id).unwrap().unwrap();
    fs.delete_record(ino, &record).unwrap();
    wait_for(|| fs.db.pending_deletes().unwrap().is_empty());
    assert!(
        fs.backend
            .inner
            .get_metadata("file-readme")
            .unwrap()
            .is_none()
    );
    assert!(!cache.exists());
    drop(fs);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn unlink_during_upload_and_same_name_recreation_keep_new_content() {
    let root = test_root("upload-delete-recreate");
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("db"));
    db.init().unwrap();
    let (started, ready) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let backend = GatedUploadBackend {
        inner: MockBackend::new(),
        started,
        release: Mutex::new(released),
        uploads: AtomicUsize::new(0),
        reject_first: false,
        completed: Mutex::new(Vec::new()),
    };
    sync_metadata(&db, &backend).unwrap();
    let mut fs =
        TwoDriveFs::new_with_upload_concurrency(db, root.join("cache"), backend, 2).unwrap();
    let unblock = ReleaseUpload(release);
    let (ino, fh, _) = fs
        .create_upload(ROOT_INO, OsStr::new("config.lock"), libc::O_CREAT)
        .unwrap();
    let old_cache = fs.write_handles[&fh].cache_path.clone();
    let old_id = fs.write_handles[&fh].record_remote_id.clone();
    std::fs::write(&old_cache, b"old lock").unwrap();
    fs.queue_upload_handle(fh).unwrap();
    fs.write_handles.remove(&fh);
    ready.recv_timeout(Duration::from_secs(10)).unwrap();
    let record = fs.db.get_by_remote_id(&old_id).unwrap().unwrap();
    assert_eq!(record.state, FileState::Uploading);
    let start = Instant::now();
    fs.delete_record(ino, &record).unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "unlink waited for the gated network transfer"
    );
    assert!(fs.db.get_by_path("/config.lock").unwrap().is_none());
    assert!(old_cache.exists());
    let (_, new_fh, _) = fs
        .create_upload(ROOT_INO, OsStr::new("config.lock"), libc::O_CREAT)
        .unwrap();
    let new_id = fs.write_handles[&new_fh].record_remote_id.clone();
    let new_cache = fs.write_handles[&new_fh].cache_path.clone();
    assert_ne!(old_id, new_id);
    assert_ne!(old_cache, new_cache);
    std::fs::write(&new_cache, b"new generation").unwrap();
    fs.queue_upload_handle(new_fh).unwrap();
    fs.write_handles.remove(&new_fh);
    let new = fs.db.get_by_remote_id(&new_id).unwrap().unwrap();
    assert!(!recover_dirty_record(&fs.db, fs.backend.as_ref(), new, &mut |_, _| Ok(())).unwrap());
    assert_eq!(fs.backend.uploads.load(Ordering::SeqCst), 1);
    // A stale cloud echo must not resurrect the old generation over the new row.
    fs.db
        .upsert_metadata_batch(&[MetadataEntry::new_file(
            "late-old",
            "/config.lock",
            8,
            1,
            "old",
        )])
        .unwrap();
    assert_eq!(
        fs.db
            .get_by_path("/config.lock")
            .unwrap()
            .unwrap()
            .metadata
            .remote_id,
        new_id
    );
    drop(unblock);
    wait_for(|| fs.db.pending_deletes().unwrap().is_empty());
    fs.upload_pool.enqueue(new_id.clone()).unwrap();
    wait_for(|| fs.db.get_by_remote_id(&new_id).unwrap().unwrap().state == FileState::Cached);
    let saved = fs.db.get_by_remote_id(&new_id).unwrap().unwrap();
    let completed = fs.backend.completed.lock().unwrap().clone();
    assert_eq!(completed.len(), 2);
    assert_ne!(completed[0].remote_id, completed[1].remote_id);
    assert!(
        fs.backend
            .get_metadata(&completed[0].remote_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fs.backend
            .download(saved.cloud_remote_id.as_deref().unwrap())
            .unwrap(),
        b"new generation"
    );
    assert!(!old_cache.exists());
    assert!(new_cache.exists());
    drop(completed);
    sync_delta_metadata(&fs.db, fs.backend.as_ref()).unwrap();
    assert_eq!(
        fs.db
            .get_by_path("/config.lock")
            .unwrap()
            .unwrap()
            .metadata
            .remote_id,
        new_id
    );
    drop(fs);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn deleted_upload_restart_recovers_before_and_after_lost_acknowledgement() {
    for cloud_accepted in [false, true] {
        let root = test_root(if cloud_accepted {
            "delete-accepted-restart"
        } else {
            "delete-claimed-restart"
        });
        fs::create_dir_all(&root).unwrap();
        let db = Database::new(root.join("db"));
        db.init().unwrap();
        let backend = MockBackend::new();
        let cache = root.join("old.cache");
        fs::write(&cache, b"old durable generation").unwrap();
        db.create_local_file("local-upload-old", "/lock", &cache)
            .unwrap();
        db.mark_dirty_with_size("local-upload-old", 22).unwrap();
        let old = db.get_by_remote_id("local-upload-old").unwrap().unwrap();
        assert!(db.begin_upload(&old).unwrap());
        if cloud_accepted {
            backend
                .upload_file_with_version("/lock", &cache, None, None, &mut |_, _| Ok(()))
                .unwrap();
        }
        db.queue_pending_delete(&old).unwrap();
        drop(db);
        let reopened = Database::new(root.join("db"));
        reopened.init().unwrap();
        reopened.init().unwrap();
        assert!(reopened.get_by_path("/lock").unwrap().is_none());
        assert!(cache.exists());
        let new_cache = root.join("new.cache");
        fs::write(&new_cache, b"new durable generation").unwrap();
        reopened
            .create_local_file("local-upload-new", "/lock", &new_cache)
            .unwrap();
        reopened
            .mark_dirty_with_size("local-upload-new", 22)
            .unwrap();
        let new = reopened.get_by_path("/lock").unwrap().unwrap();
        assert!(!recover_dirty_record(&reopened, &backend, new, &mut |_, _| Ok(())).unwrap());
        assert_eq!(recover_pending_deletes(&reopened, &backend).unwrap(), 1);
        assert!(backend.get_metadata_by_path("/lock").unwrap().is_none());
        assert!(!cache.exists());
        let new = reopened.get_by_path("/lock").unwrap().unwrap();
        assert!(recover_dirty_record(&reopened, &backend, new, &mut |_, _| Ok(())).unwrap());
        let saved = reopened.get_by_path("/lock").unwrap().unwrap();
        assert_eq!(
            backend
                .download(saved.cloud_remote_id.as_deref().unwrap())
                .unwrap(),
            b"new durable generation"
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn failed_delete_keeps_bound_upload_and_cache_for_restart_retry() {
    let root = test_root("delete-upload-outage");
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("db"));
    db.init().unwrap();
    let mut backend = FailingBackend::new(false, true);
    let cache = root.join("cache");
    fs::write(&cache, b"durable").unwrap();
    db.create_local_file("local-upload-outage", "/outage.lock", &cache)
        .unwrap();
    db.mark_dirty_with_size("local-upload-outage", 7).unwrap();
    let old = db.get_by_path("/outage.lock").unwrap().unwrap();
    assert!(db.begin_upload(&old).unwrap());
    db.queue_pending_delete(&old).unwrap();
    assert_eq!(recover_pending_deletes(&db, &backend).unwrap(), 0);
    assert!(backend.get_metadata_by_path("/locked").unwrap().is_none());
    let pending = db.pending_deletes().unwrap();
    assert!(pending[0].cloud_remote_id.is_some());
    assert!(pending[0].upload_path.is_none());
    assert!(cache.exists());
    sync_metadata(&db, &backend).unwrap();
    assert!(db.get_by_path("/outage.lock").unwrap().is_none());
    let reopened = Database::new(db.path());
    reopened.init().unwrap();
    backend.fail_delete = false;
    backend.fail_upload = true; // Retry must not upload it again.
    reopened.retry_pending_deletes().unwrap();
    assert_eq!(recover_pending_deletes(&reopened, &backend).unwrap(), 1);
    assert!(!cache.exists());
    assert!(
        backend
            .inner
            .get_metadata_by_path("/outage.lock")
            .unwrap()
            .is_none()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cloud_upload_conflict_after_unlink_does_not_create_a_conflict_copy() {
    let root = test_root("delete-during-conflict");
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("db"));
    db.init().unwrap();
    let (started, ready) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let backend = GatedUploadBackend {
        inner: MockBackend::new(),
        started,
        release: Mutex::new(released),
        uploads: AtomicUsize::new(0),
        reject_first: true,
        completed: Mutex::new(Vec::new()),
    };
    sync_metadata(&db, &backend).unwrap();
    let mut fs =
        TwoDriveFs::new_with_upload_concurrency(db, root.join("cache"), backend, 2).unwrap();
    let unblock = ReleaseUpload(release);
    let record = fs.db.get_by_path("/README-cloud.txt").unwrap().unwrap();
    let (ino, fh, _) = fs.create_overwrite_upload(record, true).unwrap();
    let cache = fs.write_handles[&fh].cache_path.clone();
    std::fs::write(&cache, b"deleted edit").unwrap();
    fs.queue_upload_handle(fh).unwrap();
    fs.write_handles.remove(&fh);
    ready.recv_timeout(Duration::from_secs(10)).unwrap();
    let record = fs.db.get_by_path("/README-cloud.txt").unwrap().unwrap();
    fs.delete_record(ino, &record).unwrap();
    drop(unblock);
    wait_for(|| fs.db.pending_deletes().unwrap().is_empty());
    assert_eq!(fs.backend.uploads.load(Ordering::SeqCst), 1);
    assert!(fs.backend.get_metadata("file-readme").unwrap().is_none());
    assert!(
        !fs.backend
            .list_all()
            .unwrap()
            .iter()
            .any(|entry| entry.name.contains("TwoDrive conflict"))
    );
    assert!(fs.db.get_by_path("/README-cloud.txt").unwrap().is_none());
    assert!(!cache.exists());
    drop(fs);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires /dev/fuse, fusermount3 and git; uses an isolated temporary mount"]
fn mounted_git_lock_files_can_close_delete_and_recreate_while_cloud_is_offline() {
    let root = test_root("mounted-git-unlink");
    let mount = root.join("mount");
    fs::create_dir_all(&mount).unwrap();
    let db = Database::new(root.join("db"));
    db.init().unwrap();
    let backend = FailingBackend::new(true, true);
    sync_metadata(&db, &backend).unwrap();
    let fs = TwoDriveFs::new(db, root.join("cache"), backend).unwrap();
    let session = fuser::spawn_mount2(fs, &mount, &[]).unwrap();
    let repo = mount.join("git-test");
    fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.name", "TwoDrive test"]);
    git(&["config", "user.email", "twodrive-test@example.invalid"]);
    fs::write(repo.join("file.txt"), b"durable local content").unwrap();
    git(&["add", "file.txt"]);
    git(&["commit", "-q", "-m", "local test"]);
    assert!(git(&["status", "--porcelain"]).is_empty());
    for name in ["HEAD.lock", "config.lock", "index.lock", "maintenance.lock"] {
        let path = repo.join(".git").join(name);
        for _ in 0..5 {
            let mut lock = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .unwrap();
            lock.write_all(b"short-lived lock").unwrap();
            lock.sync_all().unwrap();
            drop(lock);
            fs::remove_file(&path).unwrap();
            assert!(!path.exists());
        }
    }
    assert_eq!(
        fs::read(repo.join("file.txt")).unwrap(),
        b"durable local content"
    );
    assert!(git(&["status", "--porcelain"]).is_empty());
    session.join();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unlink_during_conflict_copy_upload_hides_and_cleans_both_cloud_objects() {
    let root = test_root("unlink-conflict-transfer");
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("db"));
    db.init().unwrap();
    let (started, ready) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let backend = GatedUploadBackend {
        inner: MockBackend::new(),
        started,
        release: Mutex::new(released),
        uploads: AtomicUsize::new(0),
        reject_first: false,
        completed: Mutex::new(Vec::new()),
    };
    sync_metadata(&db, &backend).unwrap();
    let mut fs =
        TwoDriveFs::new_with_upload_concurrency(db, root.join("cache"), backend, 2).unwrap();
    let unblock = ReleaseUpload(release);
    let old = fs.db.get_by_path("/README-cloud.txt").unwrap().unwrap();
    let ino = fs.inodes.ino_for_path("/README-cloud.txt").unwrap();
    let cache = fs.ensure_cached(&old).unwrap();
    std::fs::write(&cache, b"conflicting edit").unwrap();
    fs.db
        .mark_state(&old.metadata.remote_id, FileState::Conflict)
        .unwrap();
    fs.upload_pool
        .enqueue(old.metadata.remote_id.clone())
        .unwrap();
    ready.recv_timeout(Duration::from_secs(10)).unwrap();
    let record = fs.db.get_by_path("/README-cloud.txt").unwrap().unwrap();
    fs.delete_record(ino, &record).unwrap();
    drop(unblock);
    // A cloud ID returned for the conflict copy is queued separately from the
    // original. Explicit scans also model restart recovery of both entries.
    wait_for(|| !fs.backend.completed.lock().unwrap().is_empty());
    for _ in 0..2 {
        recover_pending_deletes(&fs.db, fs.backend.as_ref()).unwrap();
    }
    wait_for(|| fs.db.pending_deletes().unwrap().is_empty());
    sync_metadata(&fs.db, fs.backend.as_ref()).unwrap();
    assert!(fs.db.get_by_path("/README-cloud.txt").unwrap().is_none());
    assert!(
        !fs.backend
            .list_all()
            .unwrap()
            .iter()
            .any(|entry| entry.name.contains("TwoDrive conflict"))
    );
    assert!(fs.backend.get_metadata("file-readme").unwrap().is_none());
    assert!(!cache.exists());
    drop(fs);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn deleted_hydration_cannot_publish_cache_after_unlink() {
    let root = test_root("unlink-hydrating");
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("db"));
    db.init().unwrap();
    let entry = MetadataEntry::new_file("stream", "/stream.bin", 1_048_576, 0, "etag");
    db.upsert_metadata(&entry).unwrap();
    let record = db.get_by_remote_id("stream").unwrap().unwrap();
    let (started, ready) = mpsc::channel();
    let backend = StreamingBackend {
        inner: MockBackend::new(),
        started,
        bytes_sent: AtomicUsize::new(0),
    };
    let cache = root.join("cache");
    thread::scope(|scope| {
        let download = scope.spawn(|| hydrate_record(&db, &cache, &backend, &record));
        ready.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(
            db.get_by_remote_id("stream").unwrap().unwrap().state,
            FileState::Hydrating
        );
        db.queue_pending_delete(&record).unwrap();
        assert!(download.join().unwrap().is_err());
    });
    assert!(db.get_by_path("/stream.bin").unwrap().is_none());
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 0);
    assert_eq!(recover_pending_deletes(&db, &backend).unwrap(), 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn deletion_recovery_respects_a_retained_upload_cache_lock() {
    let root = test_root("unlink-transfer-cache-lock");
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("db"));
    db.init().unwrap();
    let backend = MockBackend::new();
    let cache = root.join("cache");
    fs::write(&cache, b"in flight").unwrap();
    db.create_local_file("local-upload-locked", "/locked", &cache)
        .unwrap();
    db.mark_dirty_with_size("local-upload-locked", 9).unwrap();
    let record = db.get_by_path("/locked").unwrap().unwrap();
    assert!(db.begin_upload(&record).unwrap());
    db.queue_pending_delete(&record).unwrap();
    let transfer_guard = std::fs::File::open(&cache).unwrap();
    transfer_guard.lock_shared().unwrap();
    // No request may be made before the retained transfer/descriptor ends.
    assert_eq!(recover_pending_deletes(&db, &backend).unwrap(), 0);
    assert!(db.pending_deletes().unwrap()[0].cloud_remote_id.is_none());
    assert!(cache.exists());
    let uploaded = MetadataEntry::new_file("accepted-old", "/locked", 9, 1, "etag");
    db.complete_deleted_upload("local-upload-locked", &uploaded)
        .unwrap();
    drop(transfer_guard);
    assert_eq!(recover_pending_deletes(&db, &backend).unwrap(), 1);
    assert!(!cache.exists());
    fs::remove_dir_all(root).unwrap();
}
