use crate::{recover_dirty_uploads, sync_delta_metadata};
use std::{
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use twodrive_backend::{
    CloudBackend, DeltaResult, MockBackend,
    diagnostics::{RequestFailure, RequestOperation},
};
use twodrive_core::{Database, FileState, MetadataEntry};

const LOCAL: &str = "local-upload-edited";
const CLOUD: &str = "file-readme";
const ORIGINAL: &str = "/README-cloud.txt";
const EDIT: &[u8] = b"durable local edit after remote deletion";
type OnAck = Box<dyn FnOnce(MetadataEntry) + Send>;

struct Backend {
    inner: MockBackend,
    bound_attempts: AtomicUsize,
    copies: AtomicUsize,
    operation: RequestOperation,
    reject_bound: bool,
    fail_metadata: bool,
    fail_copy: bool,
    rename_requests: Mutex<Vec<String>>,
    on_bound: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    on_copy: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    on_ack: Mutex<Option<OnAck>>,
}
impl Backend {
    fn new() -> Self {
        Self {
            inner: MockBackend::new(),
            bound_attempts: AtomicUsize::new(0),
            copies: AtomicUsize::new(0),
            operation: RequestOperation::Graph,
            reject_bound: true,
            fail_metadata: false,
            fail_copy: false,
            rename_requests: Mutex::new(vec![]),
            on_bound: Mutex::new(None),
            on_copy: Mutex::new(None),
            on_ack: Mutex::new(None),
        }
    }
}
impl CloudBackend for Backend {
    fn get_metadata(&self, id: &str) -> anyhow::Result<Option<MetadataEntry>> {
        if self.fail_metadata {
            return Err(RequestFailure::new(503, RequestOperation::Graph).into());
        }
        self.inner.get_metadata(id)
    }
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        self.inner.list_all()
    }
    fn list_delta(&self, _: Option<&str>) -> anyhow::Result<DeltaResult> {
        Ok(DeltaResult {
            entries: self.list_all()?,
            deleted_remote_ids: vec![CLOUD.into()],
            delta_link: Some("mock-next".into()),
        })
    }
    fn download(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        self.inner.download(id)
    }
    fn upload(&self, path: &str, bytes: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        self.inner.upload(path, bytes)
    }
    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        self.inner.create_folder(path)
    }
    fn rename(&self, id: &str, path: &str) -> anyhow::Result<MetadataEntry> {
        if path.contains(':') {
            anyhow::bail!("unsupported OneDrive file name");
        }
        self.rename_requests.lock().unwrap().push(path.into());
        self.inner.rename(id, path)
    }
    fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.inner.delete(id)
    }
    fn upload_file_with_version(
        &self,
        path: &str,
        source: &Path,
        id: Option<&str>,
        _: Option<&str>,
        progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        if path.contains(':') {
            anyhow::bail!("unsupported OneDrive file name");
        }
        if id == Some(CLOUD) && self.reject_bound {
            self.bound_attempts.fetch_add(1, Ordering::SeqCst);
            if let Some(callback) = self.on_bound.lock().unwrap().take() {
                callback();
            }
            return Err(RequestFailure::new(404, self.operation).into());
        }
        self.copies.fetch_add(1, Ordering::SeqCst);
        if self.fail_copy {
            return Err(RequestFailure::new(503, RequestOperation::Graph).into());
        }
        if let Some(callback) = self.on_copy.lock().unwrap().take() {
            callback();
        }
        let uploaded = self
            .inner
            .upload_file_with_version(path, source, id, None, progress)?;
        let callback = self.on_ack.lock().unwrap().take();
        if let Some(callback) = callback {
            callback(uploaded.clone());
        }
        Ok(uploaded)
    }
}

struct Test {
    root: PathBuf,
    db: Database,
    cache: PathBuf,
    backend: Backend,
}
impl Test {
    fn unbound(name: &str) -> Self {
        let test = Self::new(name);
        test.db.remove_by_remote_id(LOCAL).unwrap();
        test.db
            .create_local_file(LOCAL, ORIGINAL, &test.cache)
            .unwrap();
        test.db
            .mark_dirty_with_size(LOCAL, EDIT.len() as u64)
            .unwrap();
        test.backend.delete(CLOUD).unwrap();
        test
    }
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "td-missing-upload-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let db = Database::new(root.join("db"));
        db.init().unwrap();
        let cache = root.join("edit");
        std::fs::write(&cache, EDIT).unwrap();
        db.create_local_file(LOCAL, ORIGINAL, &cache).unwrap();
        db.bind_cloud_identity(LOCAL, CLOUD, "old-etag").unwrap();
        db.mark_dirty_with_size(LOCAL, EDIT.len() as u64).unwrap();
        Self {
            root,
            db,
            cache,
            backend: Backend::new(),
        }
    }
    fn conflict(&self) -> twodrive_core::FileRecord {
        self.db
            .all_records()
            .unwrap()
            .into_iter()
            .find(|r| r.metadata.name.contains("TwoDrive conflict"))
            .expect("local edit must have a conflict copy")
    }
    fn check_copy(&self) {
        let copy = self.conflict();
        assert_eq!(copy.state, FileState::Cached);
        assert_eq!(std::fs::read(copy.cache_path.unwrap()).unwrap(), EDIT);
        assert_eq!(
            self.backend
                .download(copy.cloud_remote_id.as_deref().unwrap())
                .unwrap(),
            EDIT
        );
    }
}
impl Drop for Test {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn missing_upload_target_after_delta_preserves_edits_once_across_restart() {
    let test = Test::new("deleted");
    test.backend.delete(CLOUD).unwrap();
    sync_delta_metadata(&test.db, &test.backend).unwrap();
    assert_eq!(
        test.db.get_by_remote_id(LOCAL).unwrap().unwrap().state,
        FileState::Dirty
    );
    assert_eq!(recover_dirty_uploads(&test.db, &test.backend).unwrap(), 1);
    test.check_copy();
    assert!(test.db.get_by_path(ORIGINAL).unwrap().is_none());
    assert!(
        test.backend
            .get_metadata_by_path(ORIGINAL)
            .unwrap()
            .is_none()
    );
    let reopened = Database::new(test.db.path());
    reopened.init().unwrap();
    assert_eq!(recover_dirty_uploads(&reopened, &test.backend).unwrap(), 0);
    assert_eq!(test.backend.bound_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(test.backend.copies.load(Ordering::SeqCst), 1);
}

#[test]
fn missing_upload_target_does_not_overwrite_a_replacement_at_the_same_path() {
    let test = Test::new("replaced");
    test.backend.delete(CLOUD).unwrap();
    let replacement = test
        .backend
        .upload(ORIGINAL, b"new cloud owner".to_vec())
        .unwrap();
    sync_delta_metadata(&test.db, &test.backend).unwrap();
    assert_eq!(recover_dirty_uploads(&test.db, &test.backend).unwrap(), 1);
    test.check_copy();
    assert_eq!(
        test.backend.download(&replacement.remote_id).unwrap(),
        b"new cloud owner"
    );
    sync_delta_metadata(&test.db, &test.backend).unwrap();
    assert_eq!(
        test.db
            .get_by_path(ORIGINAL)
            .unwrap()
            .unwrap()
            .cloud_remote_id
            .as_deref(),
        Some(replacement.remote_id.as_str())
    );
}

#[test]
fn conflict_recovery_with_a_deleted_original_does_not_upload_forever() {
    let test = Test::new("conflict");
    test.backend.delete(CLOUD).unwrap();
    test.db.mark_state(LOCAL, FileState::Conflict).unwrap();
    assert_eq!(recover_dirty_uploads(&test.db, &test.backend).unwrap(), 1);
    test.check_copy();
    assert_eq!(recover_dirty_uploads(&test.db, &test.backend).unwrap(), 0);
    assert_eq!(test.backend.bound_attempts.load(Ordering::SeqCst), 0);
    assert_eq!(test.backend.copies.load(Ordering::SeqCst), 1);
}

#[test]
fn missing_conflict_completion_keeps_a_newer_local_generation() {
    let test = Test::new("new-generation");
    test.backend.delete(CLOUD).unwrap();
    let db = test.db.clone();
    let new_cache = test.root.join("new-edit");
    std::fs::write(&new_cache, b"newer local edit").unwrap();
    let new_path = new_cache.clone();
    *test.backend.on_copy.lock().unwrap() = Some(Box::new(move || {
        db.mark_cached(LOCAL, &new_path).unwrap();
        db.mark_dirty_with_size(LOCAL, 16).unwrap();
    }));
    assert_eq!(recover_dirty_uploads(&test.db, &test.backend).unwrap(), 1);
    test.check_copy();
    let current = test.db.get_by_remote_id(LOCAL).unwrap().unwrap();
    assert_eq!(current.state, FileState::Dirty);
    assert_eq!(current.cache_path.as_deref(), Some(new_cache.as_path()));
    assert_eq!(
        std::fs::read(current.cache_path.unwrap()).unwrap(),
        b"newer local edit"
    );
    assert_eq!(std::fs::read(&test.cache).unwrap(), EDIT);
}

#[test]
fn upload_404_does_not_imply_deletion_when_the_bound_item_still_exists() {
    let test = Test::new("still-exists");
    assert_eq!(recover_dirty_uploads(&test.db, &test.backend).unwrap(), 0);
    assert_eq!(test.backend.copies.load(Ordering::SeqCst), 0);
    assert_eq!(
        test.db.get_by_remote_id(LOCAL).unwrap().unwrap().state,
        FileState::Dirty
    );
}

#[test]
fn oauth_404_is_not_treated_as_a_deleted_upload_target() {
    let mut test = Test::new("auth");
    test.backend.delete(CLOUD).unwrap();
    test.backend.operation = RequestOperation::TokenRefresh;
    assert_eq!(recover_dirty_uploads(&test.db, &test.backend).unwrap(), 0);
    assert_eq!(test.backend.copies.load(Ordering::SeqCst), 0);
    assert_eq!(
        test.db.get_by_remote_id(LOCAL).unwrap().unwrap().state,
        FileState::Dirty
    );
}

#[test]
fn missing_upload_failure_does_not_change_a_reopened_writer_into_a_conflict() {
    let test = Test::new("writer-reopened");
    test.backend.delete(CLOUD).unwrap();
    let db = test.db.clone();
    *test.backend.on_bound.lock().unwrap() = Some(Box::new(move || {
        db.mark_state(LOCAL, FileState::Writing).unwrap();
    }));
    assert_eq!(recover_dirty_uploads(&test.db, &test.backend).unwrap(), 0);
    assert_eq!(
        test.db.get_by_remote_id(LOCAL).unwrap().unwrap().state,
        FileState::Writing
    );
    assert_eq!(test.backend.copies.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read(&test.cache).unwrap(), EDIT);
}

#[test]
fn unlink_during_missing_conflict_upload_keeps_the_copy_in_deletion_recovery() {
    let test = Test::new("unlink-copy");
    test.backend.delete(CLOUD).unwrap();
    let db = test.db.clone();
    *test.backend.on_copy.lock().unwrap() = Some(Box::new(move || {
        let record = db.get_by_remote_id(LOCAL).unwrap().unwrap();
        db.queue_pending_delete(&record).unwrap();
    }));
    assert_eq!(recover_dirty_uploads(&test.db, &test.backend).unwrap(), 1);
    assert!(test.db.get_by_remote_id(LOCAL).unwrap().is_none());
    assert!(
        test.db
            .all_records()
            .unwrap()
            .iter()
            .all(|r| !r.metadata.name.contains("TwoDrive conflict"))
    );
    crate::recover_pending_deletes(&test.db, &test.backend).unwrap();
    assert!(test.db.pending_deletes().unwrap().is_empty());
    assert!(
        test.backend
            .list_all()
            .unwrap()
            .iter()
            .all(|r| !r.name.contains("TwoDrive conflict"))
    );
}

#[test]
fn late_upload_ack_after_rename_and_delta_alias_must_not_delete_the_active_owner() {
    for renamed in ["/Draft: renamed.txt", "/Draft renamed.txt"] {
        let test = Test::new("late-ack-rename");
        test.db.remove_by_remote_id(LOCAL).unwrap();
        test.db
            .create_local_file(LOCAL, ORIGINAL, &test.cache)
            .unwrap();
        test.db
            .mark_dirty_with_size(LOCAL, EDIT.len() as u64)
            .unwrap();
        test.backend.delete(CLOUD).unwrap();
        let db = test.db.clone();
        *test.backend.on_ack.lock().unwrap() = Some(Box::new(move |uploaded| {
            // The PUT has created an object, but its acknowledgement is delayed.
            // Rename before binding, then import the cloud echo at the old name.
            db.move_subtree_and_queue(LOCAL, renamed).unwrap();
            db.upsert_metadata(&uploaded).unwrap();
            // Rename back over that alias: the destination keeps the cloud ID,
            // while the original in-flight local identity ceases to exist.
            db.replace_file_locally(LOCAL, &uploaded.remote_id, ORIGINAL)
                .unwrap();
        }));
        assert!(recover_dirty_uploads(&test.db, &test.backend).is_err());
        let active = test.db.get_by_path(ORIGINAL).unwrap().unwrap();
        assert_eq!(active.state, FileState::Dirty);
        crate::recover_pending_deletes(&test.db, &test.backend).unwrap();
        assert!(
            test.backend
                .get_metadata(active.cloud_remote_id.as_deref().unwrap())
                .unwrap()
                .is_some(),
            "late acknowledgement cleanup deleted the active cloud owner"
        );
        assert!(
            test.db.pending_deletes().unwrap().is_empty(),
            "late acknowledgement queued deletion of the active cloud owner"
        );
        assert_eq!(
            test.backend
                .download(active.cloud_remote_id.as_deref().unwrap())
                .unwrap(),
            EDIT
        );
        assert_eq!(recover_dirty_uploads(&test.db, &test.backend).unwrap(), 1);
        assert_eq!(
            test.db.get_by_path(ORIGINAL).unwrap().unwrap().state,
            FileState::Cached
        );
    }
}

#[test]
fn queued_orphan_cleanup_is_rechecked_if_a_live_record_claims_its_cloud_id() {
    let test = Test::new("cleanup-owner");
    test.db
        .bind_cloud_identity(LOCAL, "other-cloud-id", "etag")
        .unwrap();
    test.db.queue_remote_delete(CLOUD, ORIGINAL).unwrap();
    test.db.bind_cloud_identity(LOCAL, CLOUD, "etag").unwrap();
    let reopened = Database::new(test.db.path());
    reopened.init().unwrap();
    crate::recover_pending_deletes(&reopened, &test.backend).unwrap();
    assert!(
        test.backend.get_metadata(CLOUD).unwrap().is_some(),
        "stale cleanup deleted a cloud ID that has a live local owner"
    );
    assert!(test.db.pending_deletes().unwrap().is_empty());
    assert_eq!(
        test.db
            .get_by_remote_id(LOCAL)
            .unwrap()
            .unwrap()
            .cloud_remote_id
            .as_deref(),
        Some(CLOUD)
    );
}

#[test]
fn an_unuploaded_colon_name_can_be_repaired_after_restart_without_losing_content() {
    let test = Test::unbound("invalid-new-name");
    test.db
        .move_subtree_and_queue(LOCAL, "/Draft: proposal.docx")
        .unwrap();
    assert_eq!(recover_dirty_uploads(&test.db, &test.backend).unwrap(), 0);
    assert_eq!(
        test.db.get_by_remote_id(LOCAL).unwrap().unwrap().state,
        FileState::Error
    );
    assert_eq!(std::fs::read(&test.cache).unwrap(), EDIT);
    let reopened = Database::new(test.db.path());
    reopened.init().unwrap();
    reopened
        .move_subtree_and_queue(LOCAL, "/Draft proposal.docx")
        .unwrap();
    assert_eq!(
        reopened.get_by_remote_id(LOCAL).unwrap().unwrap().state,
        FileState::Dirty
    );
    assert_eq!(recover_dirty_uploads(&reopened, &test.backend).unwrap(), 1);
    let record = reopened.get_by_remote_id(LOCAL).unwrap().unwrap();
    assert_eq!(record.state, FileState::Cached);
    assert_eq!(
        test.backend
            .download(record.cloud_remote_id.as_deref().unwrap())
            .unwrap(),
        EDIT
    );
    assert!(reopened.pending_deletes().unwrap().is_empty());
}

#[test]
fn renames_during_initial_upload_replay_only_the_latest_valid_destination() {
    for intermediate in [
        "/Draft: proposal.docx",
        "/Draft proposal.docx",
        "/Draft： proposal.docx",
    ] {
        let test = Test::unbound("rename-before-initial-ack");
        let db = test.db.clone();
        *test.backend.on_ack.lock().unwrap() = Some(Box::new(move |_| {
            db.move_subtree_and_queue(LOCAL, intermediate).unwrap();
            db.move_subtree_and_queue(LOCAL, "/final proposal.docx")
                .unwrap();
        }));
        assert_eq!(recover_dirty_uploads(&test.db, &test.backend).unwrap(), 1);
        let reopened = Database::new(test.db.path());
        reopened.init().unwrap();
        assert_eq!(
            crate::recover_pending_metadata_operations(&reopened, &test.backend).unwrap(),
            1
        );
        assert_eq!(recover_dirty_uploads(&reopened, &test.backend).unwrap(), 1);
        let current = reopened.get_by_remote_id(LOCAL).unwrap().unwrap();
        assert_eq!(current.metadata.path, "/final proposal.docx");
        assert_eq!(current.state, FileState::Cached);
        assert_eq!(
            test.backend
                .download(current.cloud_remote_id.as_deref().unwrap())
                .unwrap(),
            EDIT
        );
        assert_eq!(
            *test.backend.rename_requests.lock().unwrap(),
            ["/final proposal.docx"]
        );
        assert!(reopened.pending_deletes().unwrap().is_empty());
        assert!(reopened.pending_metadata_operations().unwrap().is_empty());
    }
}

#[test]
fn atomic_save_after_invalid_then_valid_rename_finishes_the_cloud_move() {
    let mut test = Test::new("rename-save-move");
    test.backend.reject_bound = false;
    test.db
        .move_subtree_and_queue(LOCAL, "/Draft: proposal.docx")
        .unwrap();
    assert_eq!(
        crate::recover_pending_metadata_operations(&test.db, &test.backend).unwrap(),
        0
    );
    test.db
        .move_subtree_and_queue(LOCAL, "/final proposal.docx")
        .unwrap();
    let new_cache = test.root.join("saved");
    std::fs::write(&new_cache, b"newer saved bytes").unwrap();
    test.db
        .create_local_file("local-upload-save", "/.save-temp", &new_cache)
        .unwrap();
    test.db
        .mark_dirty_with_size("local-upload-save", 17)
        .unwrap();
    test.db
        .replace_file_locally("local-upload-save", LOCAL, "/final proposal.docx")
        .unwrap();
    assert_eq!(
        crate::recover_pending_metadata_operations(&test.db, &test.backend).unwrap(),
        1
    );
    assert_eq!(recover_dirty_uploads(&test.db, &test.backend).unwrap(), 1);
    let current = test.db.get_by_remote_id(LOCAL).unwrap().unwrap();
    assert_eq!(current.state, FileState::Cached);
    assert_eq!(
        test.backend
            .download(current.cloud_remote_id.as_deref().unwrap())
            .unwrap(),
        b"newer saved bytes"
    );
    assert!(
        test.backend
            .get_metadata_by_path(ORIGINAL)
            .unwrap()
            .is_none()
    );
    assert!(test.db.pending_metadata_operations().unwrap().is_empty());
}

#[test]
fn failed_missing_target_lookup_keeps_the_upload_and_does_not_infer_deletion() {
    let mut test = Test::new("lookup-outage");
    test.backend.delete(CLOUD).unwrap();
    test.backend.fail_metadata = true;
    assert!(recover_dirty_uploads(&test.db, &test.backend).is_err());
    let kept = test.db.get_by_remote_id(LOCAL).unwrap().unwrap();
    assert_eq!(kept.state, FileState::Uploading);
    assert_eq!(std::fs::read(kept.cache_path.unwrap()).unwrap(), EDIT);
    assert_eq!(test.backend.copies.load(Ordering::SeqCst), 0);
}

#[test]
fn failed_missing_target_copy_is_durable_and_can_finish_after_restart() {
    let mut test = Test::new("copy-outage");
    test.backend.delete(CLOUD).unwrap();
    test.backend.fail_copy = true;
    assert_eq!(recover_dirty_uploads(&test.db, &test.backend).unwrap(), 0);
    assert_eq!(
        test.db.get_by_remote_id(LOCAL).unwrap().unwrap().state,
        FileState::Conflict
    );
    assert_eq!(std::fs::read(&test.cache).unwrap(), EDIT);
    let reopened = Database::new(test.db.path());
    reopened.init().unwrap();
    test.backend.fail_copy = false;
    assert_eq!(recover_dirty_uploads(&reopened, &test.backend).unwrap(), 1);
    test.check_copy();
    assert_eq!(recover_dirty_uploads(&reopened, &test.backend).unwrap(), 0);
}

#[cfg(unix)]
#[test]
#[ignore = "requires /dev/fuse and fusermount3; isolated mock mount only"]
fn mounted_save_after_remote_deletion_preserves_edits_and_open_readers() {
    use std::{
        io::Read,
        sync::Arc,
        time::{Duration, Instant},
    };
    let test = Test::new("mounted");
    let mount = test.root.join("mount");
    std::fs::create_dir(&mount).unwrap();
    let filesystem =
        crate::TwoDriveFs::new(test.db.clone(), test.root.join("cache"), Backend::new()).unwrap();
    let backend = Arc::clone(&filesystem.backend);
    backend.delete(CLOUD).unwrap();
    let session = fuser::spawn_mount2(filesystem, &mount, &[]).unwrap();
    let source = mount.join("README-cloud.txt");
    let mut old_reader = std::fs::File::open(&source).unwrap();
    let new_edit = b"saved through the actual FUSE mount";
    std::fs::write(&source, new_edit).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let copy =
        loop {
            if let Some(copy) = test.db.all_records().unwrap().into_iter().find(|r| {
                r.state == FileState::Cached && r.metadata.name.contains("TwoDrive conflict")
            }) {
                break copy;
            }
            assert!(
                Instant::now() < deadline,
                "mounted save did not finish missing-target recovery"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
    assert_eq!(
        std::fs::read(mount.join(&copy.metadata.name)).unwrap(),
        new_edit
    );
    assert_eq!(
        backend
            .download(copy.cloud_remote_id.as_deref().unwrap())
            .unwrap(),
        new_edit
    );
    let mut old_bytes = Vec::new();
    old_reader.read_to_end(&mut old_bytes).unwrap();
    // A normal in-place write is visible to an existing descriptor. Retiring
    // the old database row must not invalidate that still-open descriptor.
    assert_eq!(old_bytes, new_edit);
    assert!(backend.get_metadata_by_path(ORIGINAL).unwrap().is_none());
    assert_eq!(
        recover_dirty_uploads(&test.db, backend.as_ref()).unwrap(),
        0
    );
    drop(old_reader);
    session.join();
}
