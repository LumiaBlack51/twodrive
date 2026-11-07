use super::*;
use crate::model::DeltaResult;
use crate::provider::{Capabilities, Provider};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU8, Ordering},
    },
};
use twodrive_core::FileState;

const OBJECT: &str = "opaque-object-not-a-path";
const REMOTE_PATH: &str = "/generated-中文.txt";
const INITIAL: &[u8] = b"generated initial provider content";
const EDITED: &[u8] = b"generated unsynchronized content survives complete-byte checks";
struct Remote {
    bytes: Vec<u8>,
    version: u64,
}
#[derive(Clone)]
struct MockProvider {
    remote: Arc<Mutex<Remote>>,
    mode: Arc<AtomicU8>,
    conditional: bool,
}
impl MockProvider {
    fn new() -> Self {
        Self {
            remote: Arc::new(Mutex::new(Remote {
                bytes: INITIAL.to_vec(),
                version: 1,
            })),
            mode: Arc::new(AtomicU8::new(0)),
            conditional: true,
        }
    }
    fn metadata(remote: &Remote) -> MetadataEntry {
        MetadataEntry::new_file(
            OBJECT,
            REMOTE_PATH,
            remote.bytes.len() as u64,
            remote.version as i64,
            format!("version-{}", remote.version),
        )
    }
    fn change(&self, bytes: &[u8]) {
        let mut r = self.remote.lock().unwrap();
        r.bytes = bytes.to_vec();
        r.version += 1;
    }
}
impl CloudBackend for MockProvider {
    fn is_conflict_error(&self, error: &anyhow::Error) -> bool {
        error.to_string().contains("fixture conflict")
    }
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        Ok(vec![Self::metadata(&self.remote.lock().unwrap())])
    }
    fn list_delta(&self, _: Option<&str>) -> anyhow::Result<DeltaResult> {
        Ok(DeltaResult {
            entries: self.list_all()?,
            deleted_remote_ids: vec![],
            delta_link: None,
        })
    }
    fn get_metadata(&self, id: &str) -> anyhow::Result<Option<MetadataEntry>> {
        ensure!(id == OBJECT, "opaque ID required, never a path");
        Ok(Some(Self::metadata(&self.remote.lock().unwrap())))
    }
    fn get_metadata_by_path(&self, path: &str) -> anyhow::Result<Option<MetadataEntry>> {
        ensure!(path == REMOTE_PATH, "unexpected fixture path");
        self.get_metadata(OBJECT)
    }
    fn download(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        ensure!(id == OBJECT, "opaque download ID required");
        Ok(self.remote.lock().unwrap().bytes.clone())
    }
    fn download_to(
        &self,
        id: &str,
        writer: &mut dyn Write,
        progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        let bytes = self.download(id)?;
        writer.write_all(&bytes)?;
        progress(bytes.len() as u64)?;
        if self.mode.load(Ordering::SeqCst) == 3 {
            self.mode.store(0, Ordering::SeqCst);
            self.change(INITIAL);
        }
        Ok(bytes.len() as u64)
    }
    fn upload(&self, _: &str, _: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        anyhow::bail!("unconditional upload forbidden")
    }
    fn upload_with_etag(
        &self,
        _: &str,
        _: Vec<u8>,
        _: Option<&str>,
    ) -> anyhow::Result<MetadataEntry> {
        anyhow::bail!("use controlled conditional transfer")
    }
    fn upload_file_with_version(
        &self,
        path: &str,
        source: &Path,
        id: Option<&str>,
        version: Option<&str>,
        progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        let result = self.upload_controlled(path, source, id, version, no_progress())?;
        progress(result.size, result.size)?;
        Ok(result)
    }
    fn create_folder(&self, _: &str) -> anyhow::Result<MetadataEntry> {
        anyhow::bail!("fixture mkdir disabled")
    }
    fn rename(&self, _: &str, _: &str) -> anyhow::Result<MetadataEntry> {
        anyhow::bail!("fixture rename disabled")
    }
    fn delete(&self, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("fixture delete disabled")
    }
}
impl Provider for MockProvider {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            upload: true,
            conditional_upload: self.conditional,
            ..Default::default()
        }
    }
    fn list_directory(&self, _: &str) -> anyhow::Result<Vec<MetadataEntry>> {
        self.list_all()
    }
    fn upload_controlled(
        &self,
        path: &str,
        source: &Path,
        id: Option<&str>,
        version: Option<&str>,
        progress: Progress,
    ) -> anyhow::Result<MetadataEntry> {
        ensure!(
            path == REMOTE_PATH && id == Some(OBJECT),
            "identity/path boundary violated"
        );
        let bytes = fs::read(source)?;
        progress(0, bytes.len() as u64)?;
        if self.mode.load(Ordering::SeqCst) == 1 {
            anyhow::bail!("injected offline before commit");
        }
        let mut remote = self.remote.lock().unwrap();
        ensure!(
            version == Some(Self::metadata(&remote).etag.as_str()),
            "fixture conflict"
        );
        remote.bytes = bytes;
        remote.version += 1;
        let result = Self::metadata(&remote);
        drop(remote);
        progress(result.size, result.size)?;
        if self.mode.load(Ordering::SeqCst) == 2 {
            anyhow::bail!("injected accepted write with lost reply");
        }
        Ok(result)
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    db: Database,
    cache: PathBuf,
    bridge: LocalFileBridge,
}
fn fixture(provider: &MockProvider) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let cache = root.join("cache");
    fs::create_dir(&cache).unwrap();
    let db = Database::new(root.join("fixture.sqlite3"));
    let bridge = LocalFileBridge::open(
        db.clone(),
        &cache,
        Backend::new(provider.clone()),
        "generated-provider-account-1",
    )
    .unwrap();
    Fixture {
        _temp: temp,
        db,
        cache,
        bridge,
    }
}
fn edit(bridge: &LocalFileBridge) {
    let mut file = bridge.edit(OBJECT).unwrap();
    file.set_len(0).unwrap();
    file.write_all(EDITED).unwrap();
    file.finish().unwrap();
}
fn bytes(bridge: &LocalFileBridge) -> Vec<u8> {
    let mut file = bridge.journal.open_read(OBJECT).unwrap();
    let mut bytes = vec![];
    file.read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn actual_engine_download_edit_conditional_upload_and_generation_ack() {
    let provider = MockProvider::new();
    let fixture = fixture(&provider);
    let bridge = fixture.bridge;
    assert_eq!(bridge.hydrate(OBJECT).unwrap(), OBJECT);
    assert_eq!(bytes(&bridge), INITIAL);
    assert!(bridge.engine_release_ready(OBJECT, 0).unwrap());
    edit(&bridge);
    assert_eq!(bridge.status(OBJECT).unwrap().state, FileState::Dirty);
    assert!(!bridge.engine_release_ready(OBJECT, 1).unwrap());
    assert!(bridge.upload(OBJECT, no_progress()).unwrap());
    assert_eq!(provider.download(OBJECT).unwrap(), EDITED);
    assert_eq!(bytes(&bridge), EDITED);
    assert_eq!(bridge.status(OBJECT).unwrap().acknowledged_generation, 1);
    assert!(bridge.engine_release_ready(OBJECT, 1).unwrap());
    assert!(!bridge.engine_release_ready(OBJECT, 0).unwrap());
}

#[test]
fn native_upload_and_delta_echo_preserve_the_local_content_timestamp() {
    let provider = MockProvider::new();
    let f = fixture(&provider);
    f.bridge.hydrate(OBJECT).unwrap();
    edit(&f.bridge);
    let local = f.db.get_by_remote_id(OBJECT).unwrap().unwrap();
    assert!(f.bridge.upload(OBJECT, no_progress()).unwrap());
    let acknowledged = f.db.get_by_remote_id(OBJECT).unwrap().unwrap();
    assert_eq!(
        acknowledged.metadata.modified_unix,
        local.metadata.modified_unix
    );
    let echo = provider.get_metadata(OBJECT).unwrap().unwrap();
    f.db.upsert_metadata(&core_metadata(&echo)).unwrap();
    assert_eq!(
        f.db.get_by_remote_id(OBJECT)
            .unwrap()
            .unwrap()
            .metadata
            .modified_unix,
        local.metadata.modified_unix
    );
}

#[test]
fn native_snapshot_preserves_old_bytes_and_rejects_stale_capture() {
    let provider = MockProvider::new();
    let f = fixture(&provider);
    f.bridge.hydrate(OBJECT).unwrap();
    let old_cache =
        f.db.get_by_remote_id(OBJECT)
            .unwrap()
            .unwrap()
            .cache_path
            .unwrap();
    f.bridge.journal.record_native_edit(OBJECT).unwrap();
    let generation = f.bridge.status(OBJECT).unwrap().generation;
    let captured = f.cache.join("native-snapshot");
    fs::write(&captured, EDITED).unwrap();
    assert!(
        f.bridge
            .journal
            .stage_native_edit(OBJECT, generation - 1, &captured, EDITED.len() as u64)
            .is_err()
    );
    assert!(f.bridge.status(OBJECT).unwrap().native_dirty);
    assert_eq!(fs::read(&old_cache).unwrap(), INITIAL);
    let next = f
        .bridge
        .journal
        .stage_native_edit(OBJECT, generation, &captured, EDITED.len() as u64)
        .unwrap();
    assert_eq!(next, generation + 1);
    assert!(!f.bridge.status(OBJECT).unwrap().sync_confirmed());
    assert_eq!(bytes(&f.bridge), EDITED);
    assert!(f.bridge.upload(OBJECT, no_progress()).unwrap());
    assert_eq!(provider.download(OBJECT).unwrap(), EDITED);
    assert!(f.bridge.status(OBJECT).unwrap().sync_confirmed());
    assert_eq!(fs::read(old_cache).unwrap(), INITIAL);
}

#[test]
fn native_capture_cannot_bypass_a_reader_or_changed_path_binding() {
    let provider = MockProvider::new();
    let f = fixture(&provider);
    f.bridge.hydrate(OBJECT).unwrap();
    let reader = f.bridge.journal.open_read(OBJECT).unwrap();
    f.bridge.journal.record_native_edit(OBJECT).unwrap();
    let generation = f.bridge.status(OBJECT).unwrap().generation;
    let captured = f.cache.join("native-snapshot");
    fs::write(&captured, EDITED).unwrap();
    assert!(
        f.bridge
            .journal
            .stage_native_edit(OBJECT, generation, &captured, EDITED.len() as u64)
            .is_err()
    );
    drop(reader);
    f.db.move_subtree_and_queue(OBJECT, "/renamed.txt").unwrap();
    assert!(
        f.bridge
            .journal
            .stage_native_edit(OBJECT, generation, &captured, EDITED.len() as u64)
            .is_err()
    );
    assert!(f.bridge.status(OBJECT).unwrap().native_dirty);
    assert_eq!(fs::read(captured).unwrap(), EDITED);
}

#[test]
fn native_edit_during_an_upload_does_not_destroy_the_older_receipt() {
    let provider = MockProvider::new();
    let f = fixture(&provider);
    f.bridge.hydrate(OBJECT).unwrap();
    edit(&f.bridge);
    let mut upload = f.bridge.journal.begin_upload(OBJECT, digest_file).unwrap();
    let old = upload.record().clone();
    f.bridge.journal.record_native_edit(OBJECT).unwrap();
    let remote = provider
        .upload_controlled(
            &old.metadata.path,
            old.cache_path.as_ref().unwrap(),
            Some(OBJECT),
            Some(&old.metadata.etag),
            no_progress(),
        )
        .unwrap();
    let digest = digest_file(upload.file()).unwrap();
    assert!(
        upload
            .acknowledge(&core_metadata(&remote), &digest)
            .unwrap()
    );
    // Only the old private snapshot is acknowledged. Native bytes still veto
    // all synchronized/release claims until a new immutable snapshot is captured.
    let state = f.bridge.status(OBJECT).unwrap();
    assert!(state.native_dirty);
    assert!(!state.sync_confirmed());
    let newer = f.cache.join("newer-native-snapshot");
    fs::write(&newer, b"later native edit").unwrap();
    f.bridge
        .journal
        .stage_native_edit(OBJECT, state.generation, &newer, 17)
        .unwrap();
    assert!(f.bridge.upload(OBJECT, no_progress()).unwrap());
    assert_eq!(provider.download(OBJECT).unwrap(), b"later native edit");
}

#[test]
fn later_native_bytes_survive_lost_reply_reconciliation_and_restart() {
    let provider = MockProvider::new();
    let f = fixture(&provider);
    f.bridge.hydrate(OBJECT).unwrap();
    edit(&f.bridge);
    provider.mode.store(2, Ordering::SeqCst);
    assert!(f.bridge.upload(OBJECT, no_progress()).is_err());
    f.bridge.journal.record_native_edit(OBJECT).unwrap();
    drop(f.bridge);
    let bridge = LocalFileBridge::open(
        f.db.clone(),
        &f.cache,
        Backend::new(provider.clone()),
        "generated-provider-account-1",
    )
    .unwrap();
    assert!(bridge.reconcile(OBJECT).unwrap());
    let state = bridge.status(OBJECT).unwrap();
    assert!(state.native_dirty && !state.sync_confirmed());
    let newer = f.cache.join("later-native");
    fs::write(&newer, b"later native edit").unwrap();
    bridge
        .journal
        .stage_native_edit(OBJECT, state.generation, &newer, 17)
        .unwrap();
    provider.mode.store(0, Ordering::SeqCst);
    assert!(bridge.upload(OBJECT, no_progress()).unwrap());
    assert_eq!(provider.download(OBJECT).unwrap(), b"later native edit");
}

#[cfg(all(windows, feature = "native-local-fixture"))]
#[test]
#[ignore = "registers an explicitly supplied NEW generated CFAPI root; no real cloud"]
fn native_read_edit_capture_upload_preserves_complete_bytes() {
    use std::sync::atomic::AtomicBool;
    use twodrive_local_files::native::{HydrationSource, Session};
    struct Source(LocalFileBridge);
    impl HydrationSource for Source {
        fn size(&self, identity: &[u8]) -> Result<usize, String> {
            if identity != OBJECT.as_bytes() {
                return Err("foreign fixture identity".into());
            }
            Ok(INITIAL.len())
        }
        fn read_range(
            &self,
            identity: &[u8],
            offset: usize,
            length: usize,
            cancelled: &AtomicBool,
        ) -> Result<Vec<u8>, String> {
            self.size(identity)?;
            if self
                .0
                .journal
                .database()
                .get_by_remote_id(OBJECT)
                .map_err(|e| e.to_string())?
                .and_then(|r| r.cache_path)
                .is_none()
            {
                self.0
                    .hydrate_cancellable(OBJECT, cancelled)
                    .map_err(|e| e.to_string())?;
            }
            let mut file = self
                .0
                .journal
                .open_read(OBJECT)
                .map_err(|e| e.to_string())?;
            file.seek(SeekFrom::Start(offset as u64))
                .map_err(|e| e.to_string())?;
            let mut bytes = vec![0; length];
            file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
            Ok(bytes)
        }
    }
    let root = PathBuf::from(
        std::env::var_os("TWODRIVE_APPROVED_NATIVE_CAPTURE_ROOT")
            .expect("explicit new root required"),
    );
    assert!(root.is_absolute() && !root.exists());
    let provider = MockProvider::new();
    let f = fixture(&provider);
    let mut session =
        Session::create_with_source(&root, Arc::new(Source(f.bridge.clone()))).unwrap();
    let path = session
        .placeholder_for(
            Path::new("generated-中文.txt"),
            OBJECT.as_bytes(),
            INITIAL.len(),
        )
        .unwrap();
    let edited = vec![b'X'; INITIAL.len()];
    let result = (|| -> anyhow::Result<()> {
        assert_eq!(fs::read(&path)?, INITIAL);
        fs::write(&path, &edited)?;
        let generation = f.bridge.capture_native_edit(&path, OBJECT)?;
        assert_eq!(bytes(&f.bridge), edited);
        assert!(!f.bridge.engine_release_ready(OBJECT, generation)?);
        assert!(f.bridge.upload(OBJECT, no_progress())?);
        assert_eq!(provider.download(OBJECT)?, edited);
        assert_eq!(fs::read(&path)?, edited);
        Ok(())
    })();
    let cleanup = session.finish();
    result.unwrap();
    cleanup.unwrap();
    twodrive_local_files::native::verify_unregistered(&root).unwrap();
    assert_eq!(fs::read(path).unwrap(), edited);
}
#[test]
fn accepted_write_lost_reply_restarts_and_requires_verified_download() {
    let provider = MockProvider::new();
    let f = fixture(&provider);
    f.bridge.hydrate(OBJECT).unwrap();
    edit(&f.bridge);
    let local_modified =
        f.db.get_by_remote_id(OBJECT)
            .unwrap()
            .unwrap()
            .metadata
            .modified_unix;
    provider.mode.store(2, Ordering::SeqCst);
    assert!(f.bridge.upload(OBJECT, no_progress()).is_err());
    assert_eq!(provider.download(OBJECT).unwrap(), EDITED);
    assert!(f.bridge.status(OBJECT).unwrap().uncertain_upload);
    assert!(f.bridge.upload(OBJECT, no_progress()).is_err());
    drop(f.bridge);
    let bridge = LocalFileBridge::open(
        f.db.clone(),
        &f.cache,
        Backend::new(provider.clone()),
        "generated-provider-account-1",
    )
    .unwrap();
    assert!(bridge.status(OBJECT).unwrap().uncertain_upload);
    assert!(bridge.reconcile(OBJECT).unwrap());
    assert_eq!(
        f.db.get_by_remote_id(OBJECT)
            .unwrap()
            .unwrap()
            .metadata
            .modified_unix,
        local_modified
    );
    assert_eq!(bridge.status(OBJECT).unwrap().state, FileState::Cached);
    assert!(bridge.engine_release_ready(OBJECT, 1).unwrap());
    assert_eq!(bytes(&bridge), EDITED);
}
#[test]
fn offline_retry_waits_for_unchanged_remote_version_proof() {
    let provider = MockProvider::new();
    let f = fixture(&provider);
    f.bridge.hydrate(OBJECT).unwrap();
    edit(&f.bridge);
    provider.mode.store(1, Ordering::SeqCst);
    assert!(f.bridge.upload(OBJECT, no_progress()).is_err());
    assert!(f.bridge.status(OBJECT).unwrap().uncertain_upload);
    provider.mode.store(0, Ordering::SeqCst);
    assert!(!f.bridge.reconcile(OBJECT).unwrap());
    assert_eq!(f.bridge.status(OBJECT).unwrap().state, FileState::Dirty);
    assert!(f.bridge.upload(OBJECT, no_progress()).unwrap());
    assert_eq!(bytes(&f.bridge), EDITED);
}
#[test]
fn remote_conflict_preserves_local_bytes_and_blocks_release() {
    let provider = MockProvider::new();
    let f = fixture(&provider);
    f.bridge.hydrate(OBJECT).unwrap();
    edit(&f.bridge);
    provider.change(b"different generated remote content");
    assert!(f.bridge.upload(OBJECT, no_progress()).is_err());
    assert_eq!(f.bridge.status(OBJECT).unwrap().state, FileState::Conflict);
    assert_eq!(bytes(&f.bridge), EDITED);
    assert!(!f.bridge.engine_release_ready(OBJECT, 1).unwrap());
}
#[test]
fn provider_version_change_during_hydration_is_not_published() {
    let provider = MockProvider::new();
    let f = fixture(&provider);
    provider.mode.store(3, Ordering::SeqCst);
    assert!(f.bridge.hydrate(OBJECT).is_err());
    let record = f.db.get_by_cloud_remote_id(OBJECT).unwrap().unwrap();
    assert_eq!(record.state, FileState::OnlineOnly);
    assert!(record.cache_path.is_none());
    assert!(f.bridge.status(OBJECT).is_err());
    f.bridge.hydrate(OBJECT).unwrap();
    assert_eq!(bytes(&f.bridge), INITIAL);
}
#[test]
fn external_byte_change_invalidates_stale_cached_generation() {
    let provider = MockProvider::new();
    let f = fixture(&provider);
    f.bridge.hydrate(OBJECT).unwrap();
    let record = f.db.get_by_remote_id(OBJECT).unwrap().unwrap();
    fs::write(record.cache_path.unwrap(), EDITED).unwrap();
    assert!(!f.bridge.engine_release_ready(OBJECT, 0).unwrap());
    let state = f.bridge.status(OBJECT).unwrap();
    assert_eq!(state.state, FileState::Dirty);
    assert_eq!(state.generation, 1);
    assert!(state.dirty_intent);
    assert_eq!(bytes(&f.bridge), EDITED);
}
#[test]
fn missing_conditional_capability_never_starts_upload() {
    let mut provider = MockProvider::new();
    provider.conditional = false;
    let f = fixture(&provider);
    f.bridge.hydrate(OBJECT).unwrap();
    edit(&f.bridge);
    assert!(f.bridge.upload(OBJECT, no_progress()).is_err());
    assert!(f.bridge.status(OBJECT).unwrap().upload_generation.is_none());
    assert_eq!(provider.download(OBJECT).unwrap(), INITIAL);
}

#[test]
fn cancellation_after_provider_commit_is_reconciled_without_losing_bytes() {
    let provider = MockProvider::new();
    let f = fixture(&provider);
    f.bridge.hydrate(OBJECT).unwrap();
    edit(&f.bridge);
    let progress: Progress = Arc::new(|done, _| {
        ensure!(done == 0, "generated cancellation after commit");
        Ok(())
    });
    assert!(f.bridge.upload(OBJECT, progress).is_err());
    assert_eq!(provider.download(OBJECT).unwrap(), EDITED);
    assert!(f.bridge.status(OBJECT).unwrap().uncertain_upload);
    assert!(!f.bridge.engine_release_ready(OBJECT, 1).unwrap());
    assert!(f.bridge.reconcile(OBJECT).unwrap());
    assert_eq!(bytes(&f.bridge), EDITED);
}

#[test]
fn different_provider_identity_or_cache_root_cannot_recover_existing_journal() {
    let provider = MockProvider::new();
    let f = fixture(&provider);
    f.bridge.hydrate(OBJECT).unwrap();
    edit(&f.bridge);
    let epoch = f.db.get_state_value("local_cache_epoch").unwrap();
    drop(f.bridge);
    assert!(
        LocalFileBridge::open(
            f.db.clone(),
            &f.cache,
            Backend::new(MockProvider::new()),
            "generated-other-account"
        )
        .is_err()
    );
    let other = f.cache.with_file_name("other-generated-cache");
    fs::create_dir(&other).unwrap();
    assert!(
        LocalFileBridge::open(
            f.db.clone(),
            &other,
            Backend::new(provider.clone()),
            "generated-provider-account-1"
        )
        .is_err()
    );
    assert_eq!(
        f.db.get_state_value("local_cache_epoch").unwrap(),
        epoch,
        "wrong binding must not reset recovery or advance ownership"
    );
    let bridge = LocalFileBridge::open(
        f.db,
        &f.cache,
        Backend::new(provider),
        "generated-provider-account-1",
    )
    .unwrap();
    assert_eq!(bridge.status(OBJECT).unwrap().state, FileState::Dirty);
    assert_eq!(bytes(&bridge), EDITED);
}

#[test]
fn rejected_rehydration_must_preserve_owned_cache_metadata_and_journal() {
    let provider = MockProvider::new();
    let f = fixture(&provider);
    f.bridge.hydrate(OBJECT).unwrap();
    let before = f.db.get_by_remote_id(OBJECT).unwrap().unwrap();
    let generation = f.bridge.status(OBJECT).unwrap().generation;
    provider.change(b"generated newer version with a different size");
    assert!(f.bridge.hydrate(OBJECT).is_err());
    let after = f.db.get_by_remote_id(OBJECT).unwrap().unwrap();
    assert_eq!(
        after.metadata.etag, before.metadata.etag,
        "rejected hydration must not retag an owned cache"
    );
    assert_eq!(after.metadata.size, before.metadata.size);
    assert_eq!(after.metadata.path, before.metadata.path);
    assert_eq!(after.cache_path, before.cache_path);
    assert_eq!(f.bridge.status(OBJECT).unwrap().generation, generation);
    assert_eq!(bytes(&f.bridge), INITIAL);
}
