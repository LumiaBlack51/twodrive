use super::*;
use twodrive_core::NameExclusions;

fn active(root: &Path) -> Vec<serde_json::Value> {
    fs::read(root.join("activity.json"))
        .ok()
        .and_then(|data| serde_json::from_slice::<serde_json::Value>(&data).ok())
        .and_then(|data| data["active"].as_array().cloned())
        .unwrap_or_default()
}

fn wait_saved(test: &TestFs, path: &str, bytes: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let record = test.fs.db.get_by_path(path).unwrap().unwrap();
        if record.state == FileState::Cached
            && record
                .cloud_remote_id
                .as_ref()
                .is_some_and(|id| test.fs.backend.download(id).unwrap() == bytes)
        {
            return;
        }
        assert!(Instant::now() < deadline, "save did not finish: {record:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn office_owner_lifecycle_is_local_durable_exclusive_and_readable_after_unlink() {
    for extension in ["docx", "xlsx", "pptx"] {
        let mut test = TestFs::new(&format!("owner-{extension}"));
        let name = format!("~$document.{extension}");
        let (ino, fh, _) = test
            .fs
            .create_upload(ROOT_INO, OsStr::new(&name), libc::O_RDWR | libc::O_EXCL)
            .unwrap();
        assert!(
            active(&test.root).is_empty(),
            "creating owner is not an upload"
        );
        assert!(
            test.fs
                .create_upload(ROOT_INO, OsStr::new(&name), libc::O_RDWR | libc::O_EXCL)
                .is_err()
        );
        let cache = test.fs.write_handles[&fh].cache_path.clone();
        write_slice(&cache, 0, b"office owner").unwrap();
        test.fs.sync_handle(fh, false).unwrap();
        test.fs.queue_upload_handle(fh).unwrap();
        test.fs.write_handles.remove(&fh);
        let record = test
            .fs
            .db
            .get_by_path(&format!("/{name}"))
            .unwrap()
            .unwrap();
        assert_eq!(record.state, FileState::LocalOnly);
        assert_eq!(record.metadata.size, 12);
        assert_eq!(record.cloud_remote_id, None);
        recover_interrupted_writes(&test.fs.db).unwrap();
        assert_eq!(
            recover_dirty_uploads(&test.fs.db, test.fs.backend.as_ref()).unwrap(),
            0
        );
        assert!(!test.fs.db.release_record(&record).unwrap());
        assert_eq!(test.fs.db.prune_cache(0).unwrap(), 0);
        let read = test.fs.open_read_handle(ino, cache.clone()).unwrap();
        assert_eq!(
            test.fs.read_open_handle(read, 0, 100).unwrap(),
            b"office owner"
        );
        assert!(
            test.fs
                .unlink_record_with_open_handles(ino, &record)
                .unwrap()
        );
        assert!(
            test.fs
                .db
                .get_by_path(&format!("/{name}"))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            test.fs.read_open_handle(read, 0, 100).unwrap(),
            b"office owner"
        );
        test.fs.release_read_handle(read).unwrap();
        assert!(!cache.exists());
        assert!(test.fs.db.pending_deletes().unwrap().is_empty());
        assert!(test.fs.db.pending_metadata_operations().unwrap().is_empty());
        assert!(
            test.fs
                .upload_pool
                .sender
                .pending
                .lock()
                .unwrap()
                .is_empty()
        );
        assert!(
            test.fs
                .backend
                .get_metadata_by_path(&format!("/{name}"))
                .unwrap()
                .is_none()
        );
        assert!(active(&test.root).is_empty());
    }
}

#[test]
fn excluded_temp_promotes_to_normal_name_without_uploading_open_partial_bytes() {
    let mut test = TestFs::new("owner-promote-open");
    let (_, first, _) = test
        .fs
        .create_upload(ROOT_INO, OsStr::new("~$save.pptx"), libc::O_RDWR)
        .unwrap();
    let record = test.fs.db.get_by_path("/~$save.pptx").unwrap().unwrap();
    let (_, second, _) = test.fs.create_overwrite_upload(record, false).unwrap();
    test.fs.begin_handle_write(second).unwrap();
    let cache = test.fs.write_handles[&first].cache_path.clone();
    write_slice(&cache, 0, b"partial").unwrap();
    test.fs
        .rename_record(
            ROOT_INO,
            OsStr::new("~$save.pptx"),
            ROOT_INO,
            OsStr::new("saved.pptx"),
        )
        .unwrap();
    assert_eq!(
        test.fs
            .db
            .get_by_path("/saved.pptx")
            .unwrap()
            .unwrap()
            .state,
        FileState::Writing
    );
    test.fs.queue_upload_handle(first).unwrap();
    test.fs.write_handles.remove(&first);
    assert_eq!(
        recover_dirty_uploads(&test.fs.db, test.fs.backend.as_ref()).unwrap(),
        0
    );
    assert!(
        test.fs
            .backend
            .get_metadata_by_path("/saved.pptx")
            .unwrap()
            .is_none()
    );
    write_slice(&cache, 0, b"complete document").unwrap();
    test.fs.queue_upload_handle(second).unwrap();
    test.fs.write_handles.remove(&second);
    wait_saved(&test, "/saved.pptx", b"complete document");
}

#[test]
fn excluded_closed_temp_can_atomically_replace_a_synced_document() {
    let mut test = TestFs::new("owner-replace");
    test.fs.refresh_child_records("/");
    let (_, fh, _) = test
        .fs
        .create_upload(ROOT_INO, OsStr::new("~$save.docx"), libc::O_RDWR)
        .unwrap();
    write_slice(&test.fs.write_handles[&fh].cache_path, 0, b"saved document").unwrap();
    test.fs.queue_upload_handle(fh).unwrap();
    test.fs.write_handles.remove(&fh);
    test.fs
        .rename_record(
            ROOT_INO,
            OsStr::new("~$save.docx"),
            ROOT_INO,
            OsStr::new("README-cloud.txt"),
        )
        .unwrap();
    wait_saved(&test, "/README-cloud.txt", b"saved document");
    assert!(
        test.fs
            .backend
            .get_metadata_by_path("/~$save.docx")
            .unwrap()
            .is_none()
    );
}

#[test]
fn regex_exclusion_survives_restart_and_removing_rule_restores_upload() {
    let root = test_root("regex-restart");
    fs::create_dir_all(&root).unwrap();
    let rules = NameExclusions::new(vec![r"(?i)\.scratch$".into()]).unwrap();
    let db = Database::new(root.join("test.sqlite3")).with_name_exclusions(rules.clone());
    db.init().unwrap();
    let mut filesystem =
        TwoDriveFs::new(db.clone(), root.join("cache"), MockBackend::new()).unwrap();
    let (_, fh, _) = filesystem
        .create_upload(ROOT_INO, OsStr::new("notes.SCRATCH"), libc::O_RDWR)
        .unwrap();
    write_slice(
        &filesystem.write_handles[&fh].cache_path,
        0,
        b"unsynced durable bytes",
    )
    .unwrap();
    filesystem.sync_handle(fh, false).unwrap();
    // Simulate interruption without a normal FUSE release.
    drop(filesystem);
    drop(db);
    let db = Database::new(root.join("test.sqlite3")).with_name_exclusions(rules);
    db.init().unwrap();
    recover_interrupted_writes(&db).unwrap();
    let record = db.get_by_path("/notes.SCRATCH").unwrap().unwrap();
    assert_eq!(record.state, FileState::LocalOnly);
    assert_eq!(record.metadata.size, 22);
    assert_eq!(
        fs::read(record.cache_path.as_ref().unwrap()).unwrap(),
        b"unsynced durable bytes"
    );
    let (sender, receiver) = mpsc::channel();
    let queue = UploadQueue {
        sender,
        pending: Arc::new(Mutex::new(HashMap::new())),
    };
    crate::recovery::enqueue_recovery(&db, &queue).unwrap();
    assert!(receiver.try_recv().is_err());
    let db = Database::new(root.join("test.sqlite3"));
    db.init().unwrap();
    recover_interrupted_writes(&db).unwrap();
    assert_eq!(
        db.get_by_path("/notes.SCRATCH").unwrap().unwrap().state,
        FileState::Dirty
    );
    let backend = MockBackend::new();
    assert_eq!(recover_dirty_uploads(&db, &backend).unwrap(), 1);
    assert_eq!(
        backend
            .download(
                db.get_by_path("/notes.SCRATCH")
                    .unwrap()
                    .unwrap()
                    .cloud_remote_id
                    .as_deref()
                    .unwrap()
            )
            .unwrap(),
        b"unsynced durable bytes"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_owner_dirty_retry_migrates_without_discarding_cache_or_other_work() {
    let mut test = TestFs::new("owner-migrate");
    let (_, fh, _) = test
        .fs
        .create_upload(ROOT_INO, OsStr::new("~$legacy.xlsx"), libc::O_RDWR)
        .unwrap();
    let cache = test.fs.write_handles[&fh].cache_path.clone();
    write_slice(&cache, 0, b"old owner").unwrap();
    let record = test.fs.db.get_by_path("/~$legacy.xlsx").unwrap().unwrap();
    test.fs.write_handles.remove(&fh);
    test.fs
        .db
        .mark_state(&record.metadata.remote_id, FileState::Dirty)
        .unwrap();
    recover_interrupted_writes(&test.fs.db).unwrap();
    let after = test.fs.db.get_by_path("/~$legacy.xlsx").unwrap().unwrap();
    assert_eq!(after.state, FileState::LocalOnly);
    assert_eq!(after.metadata.size, 9);
    assert_eq!(fs::read(&cache).unwrap(), b"old owner");
    assert!(
        test.fs
            .db
            .get_by_path("/README-cloud.txt")
            .unwrap()
            .is_some()
    );
    assert_eq!(
        recover_dirty_uploads(&test.fs.db, test.fs.backend.as_ref()).unwrap(),
        0
    );
}

#[test]
fn local_writes_and_waiting_preflight_have_no_transfer_activity() {
    let mut test = TestFs::new("local-write-activity");
    let (_, fh, _) = test
        .fs
        .create_upload(ROOT_INO, OsStr::new("normal.txt"), libc::O_RDWR)
        .unwrap();
    write_slice(&test.fs.write_handles[&fh].cache_path, 0, b"local bytes").unwrap();
    test.fs.sync_handle(fh, false).unwrap();
    assert!(active(&test.root).is_empty());
    let parent = test
        .fs
        .db
        .create_local_directory("local-upload-parent", "/waiting")
        .unwrap();
    let parent_ino = test.fs.inodes.insert_or_update(parent);
    let (_, waiting, _) = test
        .fs
        .create_upload(parent_ino, OsStr::new("queued.txt"), libc::O_RDWR)
        .unwrap();
    test.fs.queue_upload_handle(waiting).unwrap();
    test.fs.write_handles.remove(&waiting);
    assert_eq!(
        recover_dirty_uploads(&test.fs.db, test.fs.backend.as_ref()).unwrap(),
        0
    );
    assert!(active(&test.root).is_empty());
    // A real network transfer still gets an activity, tested with a gated backend below.
    let root = test_root("transfer-activity");
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("test.sqlite3"));
    db.init().unwrap();
    let (backend, release) = ConcurrentBackend::with_blocked_first_upload();
    let mut filesystem = TwoDriveFs::new(db, root.join("cache"), backend).unwrap();
    let (_, fh, _) = filesystem
        .create_upload(ROOT_INO, OsStr::new("network.txt"), libc::O_RDWR)
        .unwrap();
    write_slice(
        &filesystem.write_handles[&fh].cache_path,
        0,
        b"network bytes",
    )
    .unwrap();
    assert!(active(&root).is_empty());
    filesystem.queue_upload_handle(fh).unwrap();
    filesystem.write_handles.remove(&fh);
    filesystem.backend.wait_for_upload();
    let activity = active(&root);
    release.send(()).unwrap();
    assert_eq!(activity.len(), 1);
    assert_eq!(activity[0]["kind"], "upload");
    assert_eq!(activity[0]["bytes_done"], 0);
    drop(filesystem);
    assert!(active(&root).is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires /dev/fuse; isolated mock mount only"]
fn mounted_office_owner_and_regex_files_stay_local_and_promote_on_rename() {
    use std::io::Seek;
    use std::os::fd::AsRawFd;
    let root = test_root("mounted-owner-regex");
    let mount = root.join("mount");
    fs::create_dir_all(&mount).unwrap();
    let db = Database::new(root.join("test.sqlite3"))
        .with_name_exclusions(NameExclusions::new(vec![r"(?i)\.scratch$".into()]).unwrap());
    db.init().unwrap();
    let backend = MockBackend::new();
    sync_metadata(&db, &backend).unwrap();
    let filesystem = TwoDriveFs::new(db.clone(), root.join("cache"), backend).unwrap();
    let backend = Arc::clone(&filesystem.backend);
    let session = fuser::spawn_mount2(filesystem, &mount, &[]).unwrap();
    for name in [
        "~$report.docx",
        "~$report.xlsx",
        "~$report.pptx",
        "notes.SCRATCH",
    ] {
        let path = mount.join(name);
        let mut owner = fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        owner.write_all(b"owner bytes").unwrap();
        owner.sync_all().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"owner bytes");
        assert_eq!(
            fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        let sibling = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        // SAFETY: both descriptors are live; the kernel enforces local flock semantics.
        assert_eq!(
            unsafe { libc::flock(owner.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert_eq!(
            unsafe { libc::flock(sibling.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            -1
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::EWOULDBLOCK)
        );
        assert!(active(&root).is_empty());
        assert!(
            backend
                .get_metadata_by_path(&format!("/{name}"))
                .unwrap()
                .is_none()
        );
        fs::remove_file(&path).unwrap();
        owner.rewind().unwrap();
        let mut content = Vec::new();
        owner.read_to_end(&mut content).unwrap();
        assert_eq!(content, b"owner bytes");
        drop(sibling);
        drop(owner);
    }
    let temp = mount.join("saved.SCRATCH");
    fs::write(&temp, b"final document").unwrap();
    // FUSE release is asynchronous; rename must also work before it finishes.
    fs::rename(&temp, mount.join("README-cloud.txt")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let record = db.get_by_path("/README-cloud.txt").unwrap().unwrap();
        if record.state == FileState::Cached
            && backend
                .download(record.cloud_remote_id.as_deref().unwrap())
                .unwrap()
                == b"final document"
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "promoted save did not upload: {record:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(db.pending_deletes().unwrap().is_empty());
    assert!(
        backend
            .get_metadata_by_path("/saved.SCRATCH")
            .unwrap()
            .is_none()
    );
    session.join();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cached_cloud_file_can_be_excluded_then_renamed_back_without_losing_edits() {
    let mut test = TestFs::new("owner-cloud-demotion");
    let original = test
        .fs
        .db
        .get_by_path("/README-cloud.txt")
        .unwrap()
        .unwrap();
    let old_bytes = test
        .fs
        .backend
        .download(original.cloud_remote_id.as_deref().unwrap())
        .unwrap();
    test.fs
        .rename_record(
            ROOT_INO,
            OsStr::new("README-cloud.txt"),
            ROOT_INO,
            OsStr::new("~$document.docx"),
        )
        .unwrap();
    let local = test.fs.db.get_by_path("/~$document.docx").unwrap().unwrap();
    assert_eq!(local.state, FileState::LocalOnly);
    let (_, fh, _) = test.fs.create_overwrite_upload(local, true).unwrap();
    write_slice(
        &test.fs.write_handles[&fh].cache_path,
        0,
        b"local edited bytes",
    )
    .unwrap();
    test.fs.queue_upload_handle(fh).unwrap();
    test.fs.write_handles.remove(&fh);
    assert_eq!(
        test.fs
            .backend
            .download(original.cloud_remote_id.as_deref().unwrap())
            .unwrap(),
        old_bytes
    );
    assert_eq!(
        recover_dirty_uploads(&test.fs.db, test.fs.backend.as_ref()).unwrap(),
        0
    );
    test.fs
        .rename_record(
            ROOT_INO,
            OsStr::new("~$document.docx"),
            ROOT_INO,
            OsStr::new("saved.docx"),
        )
        .unwrap();
    wait_saved(&test, "/saved.docx", b"local edited bytes");
}

#[test]
fn missing_local_only_cache_never_falls_back_to_old_cloud_bytes() {
    let mut test = TestFs::new("missing-local-only");
    let (ino, fh, _) = test
        .fs
        .create_upload(ROOT_INO, OsStr::new("~$missing.pptx"), libc::O_RDWR)
        .unwrap();
    let cache = test.fs.write_handles[&fh].cache_path.clone();
    test.fs.queue_upload_handle(fh).unwrap();
    test.fs.write_handles.remove(&fh);
    let record = test.fs.record_for_ino(ino).unwrap();
    fs::remove_file(&cache).unwrap();
    assert!(
        test.fs
            .ensure_cached(&record)
            .unwrap_err()
            .to_string()
            .contains("local-only content is unavailable")
    );
    assert_eq!(
        test.fs
            .db
            .get_by_remote_id(&record.metadata.remote_id)
            .unwrap()
            .unwrap()
            .state,
        FileState::LocalOnly
    );
    assert!(active(&test.root).is_empty());
}

#[test]
fn in_flight_upload_renamed_to_excluded_then_unlinked_keeps_its_tombstone() {
    let root = test_root("exclude-during-upload");
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("db"));
    db.init().unwrap();
    let (backend, release) = ConcurrentBackend::with_blocked_first_upload();
    let mut filesystem = TwoDriveFs::new(db.clone(), root.join("cache"), backend).unwrap();
    let (ino, fh, _) = filesystem
        .create_upload(ROOT_INO, OsStr::new("normal.txt"), libc::O_RDWR)
        .unwrap();
    write_slice(
        &filesystem.write_handles[&fh].cache_path,
        0,
        b"in flight bytes",
    )
    .unwrap();
    filesystem.queue_upload_handle(fh).unwrap();
    filesystem.write_handles.remove(&fh);
    filesystem.backend.wait_for_upload();
    filesystem
        .rename_record(
            ROOT_INO,
            OsStr::new("normal.txt"),
            ROOT_INO,
            OsStr::new("~$owner.docx"),
        )
        .unwrap();
    let record = db.get_by_path("/~$owner.docx").unwrap().unwrap();
    let deleted = filesystem.delete_record(ino, &record);
    // Always release the gated worker before evaluating assertions.
    release.send(()).unwrap();
    deleted.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        recover_pending_deletes(&db, filesystem.backend.as_ref()).unwrap();
        if db.pending_deletes().unwrap().is_empty()
            && filesystem.backend.active_uploads.load(Ordering::SeqCst) == 0
        {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        filesystem
            .backend
            .get_metadata_by_path("/normal.txt")
            .unwrap()
            .is_none()
    );
    assert!(
        filesystem
            .backend
            .get_metadata_by_path("/~$owner.docx")
            .unwrap()
            .is_none()
    );
    drop(filesystem);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn excluded_backup_does_not_block_new_original_save_across_retries_and_restart() {
    let root = test_root("excluded-backup-save");
    fs::create_dir_all(&root).unwrap();
    let rules = NameExclusions::new(vec![r"\.bak$".into()]).unwrap();
    let db = Database::new(root.join("db")).with_name_exclusions(rules.clone());
    db.init().unwrap();
    let backend = MockBackend::new();
    sync_metadata(&db, &backend).unwrap();
    let filesystem = TwoDriveFs::new(db.clone(), root.join("cache"), backend).unwrap();
    let backend = Arc::clone(&filesystem.backend);
    let mut test = TestFs {
        fs: filesystem,
        root,
    };
    let original = db.get_by_path("/README-cloud.txt").unwrap().unwrap();
    let original_bytes = backend
        .download(original.cloud_remote_id.as_deref().unwrap())
        .unwrap();
    test.fs
        .rename_record(
            ROOT_INO,
            OsStr::new("README-cloud.txt"),
            ROOT_INO,
            OsStr::new("README-cloud.txt.bak"),
        )
        .unwrap();
    let backup_before = db.get_by_path("/README-cloud.txt.bak").unwrap().unwrap();
    let (_, backup_fh, _) = test
        .fs
        .create_overwrite_upload(backup_before.clone(), false)
        .unwrap();
    let (_, backup_promote_fh, _) = test
        .fs
        .create_overwrite_upload(backup_before, false)
        .unwrap();
    let (_, fh, _) = test
        .fs
        .create_upload(ROOT_INO, OsStr::new("README-cloud.txt"), libc::O_RDWR)
        .unwrap();
    let saved = b"complete newly saved document with excluded backup";
    write_slice(&test.fs.write_handles[&fh].cache_path, 0, saved).unwrap();
    test.fs.queue_upload_handle(fh).unwrap();
    test.fs.write_handles.remove(&fh);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !test
        .fs
        .upload_pool
        .sender
        .pending
        .lock()
        .unwrap()
        .is_empty()
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    recover_dirty_uploads(&db, backend.as_ref()).unwrap();
    // Reopen persistence and execute the same startup/periodic recovery paths.
    let reopened = Database::new(test.root.join("db")).with_name_exclusions(rules);
    reopened.init().unwrap();
    recover_interrupted_writes(&reopened).unwrap();
    recover_dirty_uploads(&reopened, backend.as_ref()).unwrap();
    let uploaded = backend
        .get_metadata_by_path("/README-cloud.txt")
        .unwrap()
        .unwrap();
    assert_eq!(
        backend.download(&uploaded.remote_id).unwrap(),
        saved,
        "excluded backup must not monopolize the original cloud identity"
    );
    let backup = reopened
        .get_by_path("/README-cloud.txt.bak")
        .unwrap()
        .unwrap();
    assert_eq!(backup.state, FileState::LocalOnly);
    assert_eq!(
        fs::read(backup.cache_path.unwrap()).unwrap(),
        original_bytes
    );
    assert!(backup.cloud_remote_id.is_none());
    assert!(
        backend
            .get_metadata_by_path("/README-cloud.txt.bak")
            .unwrap()
            .is_none()
    );
    // Detaching the cloud binding must not revoke an already opened backup's
    // local write permission or send its later bytes to the new document.
    test.fs.begin_handle_write(backup_fh).unwrap();
    write_slice(
        &test.fs.write_handles[&backup_fh].cache_path,
        0,
        b"edited backup",
    )
    .unwrap();
    test.fs.queue_upload_handle(backup_fh).unwrap();
    test.fs.write_handles.remove(&backup_fh);
    assert_eq!(backend.download(&uploaded.remote_id).unwrap(), saved);
    test.fs
        .rename_record(
            ROOT_INO,
            OsStr::new("README-cloud.txt.bak"),
            ROOT_INO,
            OsStr::new("promoted-backup.txt"),
        )
        .unwrap();
    test.fs.begin_handle_write(backup_promote_fh).unwrap();
    write_slice(
        &test.fs.write_handles[&backup_promote_fh].cache_path,
        0,
        b"new promoted backup",
    )
    .unwrap();
    test.fs.queue_upload_handle(backup_promote_fh).unwrap();
    test.fs.write_handles.remove(&backup_promote_fh);
    let promoted_bytes = fs::read(
        test.fs
            .db
            .get_by_path("/promoted-backup.txt")
            .unwrap()
            .unwrap()
            .cache_path
            .unwrap(),
    )
    .unwrap();
    wait_saved(&test, "/promoted-backup.txt", &promoted_bytes);
    assert_eq!(backend.download(&uploaded.remote_id).unwrap(), saved);
    assert_eq!(
        reopened
            .get_by_path("/README-cloud.txt")
            .unwrap()
            .unwrap()
            .state,
        FileState::Cached
    );
}

struct BackupVersionBackend {
    inner: MockBackend,
    external_change: std::sync::atomic::AtomicBool,
    attempted_etags: Mutex<Vec<String>>,
    fail_next: std::sync::atomic::AtomicBool,
    fail_entered: std::sync::atomic::AtomicBool,
    failure_gate: Mutex<Option<mpsc::Receiver<()>>>,
}
impl CloudBackend for BackupVersionBackend {
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        let mut entries = self.inner.list_all()?;
        if self.external_change.load(Ordering::SeqCst) {
            for entry in &mut entries {
                if entry.path == "/report.txt" {
                    entry.etag = "etag-external".into();
                }
            }
        }
        Ok(entries)
    }
    fn download(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        self.inner.download(id)
    }
    fn upload(&self, path: &str, bytes: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        self.upload_with_etag(path, bytes, None)
    }
    fn upload_with_etag(
        &self,
        path: &str,
        bytes: Vec<u8>,
        etag: Option<&str>,
    ) -> anyhow::Result<MetadataEntry> {
        if path == "/report.txt" && self.fail_next.swap(false, Ordering::SeqCst) {
            self.fail_entered.store(true, Ordering::SeqCst);
            if let Some(gate) = self.failure_gate.lock().unwrap().take() {
                gate.recv_timeout(Duration::from_secs(15))?;
            }
            anyhow::bail!("Graph request failed with HTTP 503 Service Unavailable");
        }
        if path == "/report.txt"
            && self.external_change.load(Ordering::SeqCst)
            && let Some(etag) = etag
        {
            self.attempted_etags.lock().unwrap().push(etag.into());
            if etag != "etag-external" {
                anyhow::bail!("Graph request failed with HTTP 412 Precondition Failed");
            }
        }
        self.inner.upload(path, bytes)
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
}

#[test]
fn excluded_backup_handoff_retains_old_etag_and_preserves_external_edit_as_conflict() {
    let root = test_root("excluded-backup-external-edit");
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("db"))
        .with_name_exclusions(NameExclusions::new(vec![r"\.bak$".into()]).unwrap());
    db.init().unwrap();
    let backend = BackupVersionBackend {
        inner: MockBackend::new(),
        external_change: std::sync::atomic::AtomicBool::new(false),
        attempted_etags: Mutex::new(Vec::new()),
        fail_next: std::sync::atomic::AtomicBool::new(false),
        fail_entered: std::sync::atomic::AtomicBool::new(false),
        failure_gate: Mutex::new(None),
    };
    backend
        .inner
        .upload("/report.txt", b"original before backup".to_vec())
        .unwrap();
    sync_metadata(&db, &backend).unwrap();
    let baseline = db
        .get_by_path("/report.txt")
        .unwrap()
        .unwrap()
        .metadata
        .etag;
    let mut filesystem = TwoDriveFs::new(db.clone(), root.join("cache"), backend).unwrap();
    filesystem
        .rename_record(
            ROOT_INO,
            OsStr::new("report.txt"),
            ROOT_INO,
            OsStr::new("report.txt.bak"),
        )
        .unwrap();
    filesystem
        .backend
        .inner
        .upload("/report.txt", b"external cloud winner".to_vec())
        .unwrap();
    filesystem
        .backend
        .external_change
        .store(true, Ordering::SeqCst);
    let (_, fh, _) = filesystem
        .create_upload(ROOT_INO, OsStr::new("report.txt"), libc::O_RDWR)
        .unwrap();
    write_slice(
        &filesystem.write_handles[&fh].cache_path,
        0,
        b"local saved document",
    )
    .unwrap();
    filesystem.queue_upload_handle(fh).unwrap();
    filesystem.write_handles.remove(&fh);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !filesystem
        .upload_pool
        .sender
        .pending
        .lock()
        .unwrap()
        .is_empty()
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let attempts = filesystem.backend.attempted_etags.lock().unwrap();
    assert_eq!(&*attempts, &[baseline]);
    drop(attempts);
    let remote = filesystem
        .backend
        .get_metadata_by_path("/report.txt")
        .unwrap()
        .unwrap();
    assert_eq!(
        filesystem.backend.download(&remote.remote_id).unwrap(),
        b"external cloud winner"
    );
    let conflict = filesystem
        .backend
        .list_all()
        .unwrap()
        .into_iter()
        .find(|entry| entry.name.contains("TwoDrive conflict"))
        .expect("local edit must be preserved as a conflict copy");
    assert_eq!(
        filesystem.backend.download(&conflict.remote_id).unwrap(),
        b"local saved document"
    );
    let backup = db.get_by_path("/report.txt.bak").unwrap().unwrap();
    assert_eq!(backup.state, FileState::LocalOnly);
    assert!(backup.cloud_remote_id.is_none());
    assert_eq!(
        fs::read(backup.cache_path.unwrap()).unwrap(),
        b"original before backup"
    );
    drop(filesystem);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_known_upload_before_or_during_excluded_backup_rename_does_not_block_save() {
    for scenario in ["before", "during", "legacy_restart"] {
        let rename_during_failure = scenario == "during";
        let root = test_root("excluded-failed-backup");
        fs::create_dir_all(&root).unwrap();
        let rules = NameExclusions::new(vec![r"\.bak$".into()]).unwrap();
        let db = Database::new(root.join("db")).with_name_exclusions(rules.clone());
        db.init().unwrap();
        let (release, gate) = mpsc::channel();
        let backend = BackupVersionBackend {
            inner: MockBackend::new(),
            external_change: std::sync::atomic::AtomicBool::new(false),
            attempted_etags: Mutex::new(Vec::new()),
            fail_next: std::sync::atomic::AtomicBool::new(true),
            fail_entered: std::sync::atomic::AtomicBool::new(false),
            failure_gate: Mutex::new(if rename_during_failure {
                Some(gate)
            } else {
                None
            }),
        };
        backend
            .inner
            .upload("/report.txt", b"original cloud bytes".to_vec())
            .unwrap();
        sync_metadata(&db, &backend).unwrap();
        let mut filesystem = TwoDriveFs::new(db.clone(), root.join("cache"), backend).unwrap();
        let original = db.get_by_path("/report.txt").unwrap().unwrap();
        let (_, fh, _) = filesystem.create_overwrite_upload(original, true).unwrap();
        write_slice(
            &filesystem.write_handles[&fh].cache_path,
            0,
            b"unsynced earlier edit in backup",
        )
        .unwrap();
        filesystem.queue_upload_handle(fh).unwrap();
        filesystem.write_handles.remove(&fh);
        let deadline = Instant::now() + Duration::from_secs(10);
        if rename_during_failure {
            while !filesystem.backend.fail_entered.load(Ordering::SeqCst) {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(10));
            }
        } else {
            while !filesystem
                .upload_pool
                .sender
                .pending
                .lock()
                .unwrap()
                .is_empty()
            {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(10));
            }
        }
        if scenario == "legacy_restart" {
            // Old versions kept this exact known-target claim after an error.
            let dirty = db.get_by_path("/report.txt").unwrap().unwrap();
            assert!(db.begin_upload(&dirty).unwrap());
            db.mark_state(&dirty.metadata.remote_id, FileState::Dirty)
                .unwrap();
        }
        filesystem
            .rename_record(
                ROOT_INO,
                OsStr::new("report.txt"),
                ROOT_INO,
                OsStr::new("report.txt.bak"),
            )
            .unwrap();
        let (_, new_fh, _) = filesystem
            .create_upload(ROOT_INO, OsStr::new("report.txt"), libc::O_RDWR)
            .unwrap();
        write_slice(
            &filesystem.write_handles[&new_fh].cache_path,
            0,
            b"new complete saved document",
        )
        .unwrap();
        filesystem.queue_upload_handle(new_fh).unwrap();
        filesystem.write_handles.remove(&new_fh);
        if rename_during_failure {
            release.send(()).unwrap();
        }
        while !filesystem
            .upload_pool
            .sender
            .pending
            .lock()
            .unwrap()
            .is_empty()
        {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        recover_dirty_uploads(&db, filesystem.backend.as_ref()).unwrap();
        let reopened = Database::new(root.join("db")).with_name_exclusions(rules);
        reopened.init().unwrap();
        recover_interrupted_writes(&reopened).unwrap();
        recover_dirty_uploads(&reopened, filesystem.backend.as_ref()).unwrap();
        let remote = filesystem
            .backend
            .get_metadata_by_path("/report.txt")
            .unwrap()
            .unwrap();
        assert_eq!(
            filesystem.backend.download(&remote.remote_id).unwrap(),
            b"new complete saved document"
        );
        let backup = reopened.get_by_path("/report.txt.bak").unwrap().unwrap();
        assert_eq!(backup.state, FileState::LocalOnly);
        assert!(backup.cloud_remote_id.is_none());
        assert_eq!(
            fs::read(backup.cache_path.unwrap()).unwrap(),
            b"unsynced earlier edit in backup"
        );
        assert!(
            filesystem
                .backend
                .get_metadata_by_path("/report.txt.bak")
                .unwrap()
                .is_none()
        );
        drop(filesystem);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn startup_retirement_preserves_unknown_create_claim_and_its_delete_uncertainty() {
    let mut test = TestFs::new("unknown-create-claim");
    let (_, fh, _) = test
        .fs
        .create_upload(ROOT_INO, OsStr::new("unbound.txt"), libc::O_RDWR)
        .unwrap();
    let cache = test.fs.write_handles[&fh].cache_path.clone();
    write_slice(&cache, 0, b"uncertain create bytes").unwrap();
    let record = test.fs.db.get_by_path("/unbound.txt").unwrap().unwrap();
    test.fs
        .db
        .mark_dirty_with_size(&record.metadata.remote_id, 22)
        .unwrap();
    let dirty = test.fs.db.get_by_path("/unbound.txt").unwrap().unwrap();
    assert!(test.fs.db.begin_upload(&dirty).unwrap());
    test.fs.write_handles.remove(&fh);
    recover_interrupted_writes(&test.fs.db).unwrap();
    let current = test.fs.db.get_by_path("/unbound.txt").unwrap().unwrap();
    let tombstone = test.fs.db.queue_pending_delete(&current).unwrap();
    assert_eq!(tombstone.cloud_remote_id, None);
    assert_eq!(tombstone.upload_path.as_deref(), Some("/unbound.txt"));
    assert_eq!(
        tombstone.upload_cache_path.as_deref(),
        Some(cache.as_path())
    );
    assert_eq!(fs::read(cache).unwrap(), b"uncertain create bytes");
}
