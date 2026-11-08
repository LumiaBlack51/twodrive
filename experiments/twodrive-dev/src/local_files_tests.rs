use super::*;
use crate::model::DeltaResult;
use crate::provider::{Capabilities, Provider};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
};
use twodrive_core::FileState;

const OBJECT: &str = "opaque-object-not-a-path";
const REMOTE_PATH: &str = "/generated-中文.txt";
const INITIAL: &[u8] = b"generated initial provider content";
const EDITED: &[u8] = b"generated unsynchronized content survives complete-byte checks";
struct Remote {
    present: bool,
    bytes: Vec<u8>,
    version: u64,
}
#[derive(Clone)]
struct MockProvider {
    remote: Arc<Mutex<Remote>>,
    mode: Arc<AtomicU8>,
    conditional: bool,
    upload_calls: Arc<AtomicUsize>,
}
impl MockProvider {
    fn new() -> Self {
        Self {
            remote: Arc::new(Mutex::new(Remote {
                present: true,
                bytes: INITIAL.to_vec(),
                version: 1,
            })),
            mode: Arc::new(AtomicU8::new(0)),
            conditional: true,
            upload_calls: Arc::new(AtomicUsize::new(0)),
        }
    }
    fn empty() -> Self {
        let value = Self::new();
        value.remote.lock().unwrap().present = false;
        value
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
        let remote = self.remote.lock().unwrap();
        Ok(remote.present.then(|| Self::metadata(&remote)))
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
        self.upload_calls.fetch_add(1, Ordering::SeqCst);
        ensure!(
            path == REMOTE_PATH && (id == Some(OBJECT) || id.is_none()),
            "identity/path boundary violated"
        );
        let bytes = fs::read(source)?;
        progress(0, bytes.len() as u64)?;
        if self.mode.load(Ordering::SeqCst) == 1 {
            anyhow::bail!("injected offline before commit");
        }
        let mut remote = self.remote.lock().unwrap();
        if id.is_none() {
            ensure!(
                !remote.present && version.is_none(),
                "fixture conflict: create-only target exists"
            );
        } else {
            ensure!(
                remote.present && version == Some(Self::metadata(&remote).etag.as_str()),
                "fixture conflict"
            );
        }
        remote.present = true;
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
    db: Database,
    cache: PathBuf,
    bridge: LocalFileBridge,
    // Fields drop in declaration order. Release Windows no-delete-sharing
    // owner/directory handles before TempDir attempts its cleanup.
    _temp: tempfile::TempDir,
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

#[test]
fn fixture_releases_native_owner_before_temporary_directory_cleanup() {
    let provider = MockProvider::new();
    let f = fixture(&provider);
    let root = f._temp.path().to_path_buf();
    drop(f);
    assert!(
        !root.exists(),
        "generated fixture was not cleaned: {}",
        root.display()
    );
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
        for content in [
            b"short".to_vec(),
            Vec::new(),
            vec![b'G'; 131_079],
            edited.clone(),
        ] {
            fs::write(&path, &content)?;
            let source_modified =
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_042);
            std::fs::File::options()
                .write(true)
                .open(&path)?
                .set_times(std::fs::FileTimes::new().set_modified(source_modified))?;
            let generation = f.bridge.capture_native_edit(&path, OBJECT)?;
            let captured = f.db.get_by_remote_id(OBJECT)?.unwrap();
            assert_eq!(captured.metadata.modified_unix, 1_700_000_042);
            assert_eq!(captured.metadata.size, content.len() as u64);
            assert_eq!(bytes(&f.bridge), content);
            assert!(!f.bridge.engine_release_ready(OBJECT, generation)?);
            assert!(!f.bridge.acknowledge_native(&path, OBJECT)?);
            assert!(f.bridge.upload(OBJECT, no_progress())?);
            assert_eq!(provider.download(OBJECT)?, content);
            assert_eq!(fs::read(&path)?, content);
            if content == b"short" {
                // Same-size later edit: an old cloud receipt is insufficient.
                fs::write(&path, b"later")?;
                assert!(!f.bridge.acknowledge_native(&path, OBJECT)?);
                assert!(f.bridge.status(OBJECT)?.native_dirty);
                assert_eq!(fs::read(&path)?, b"later");
                fs::write(&path, &content)?;
                f.bridge.capture_native_edit(&path, OBJECT)?;
                assert!(f.bridge.upload(OBJECT, no_progress())?);
            }
            f.db.mark_pinned(OBJECT)?;
            assert!(f.bridge.acknowledge_native(&path, OBJECT)?);
            f.db.mark_unpinned(OBJECT)?;
            assert!(f.bridge.acknowledge_native(&path, OBJECT)?);
        }
        Ok(())
    })();
    // The generated fixture's retirement contract deliberately expects its
    // original size. Restore only these synthetic bytes if a regression fails.
    if result.is_err() {
        fs::write(&path, &edited).unwrap();
    }
    let cleanup = session.finish();
    result.unwrap();
    cleanup.unwrap();
    twodrive_local_files::native::verify_unregistered(&root).unwrap();
    assert_eq!(fs::read(path).unwrap(), edited);
}
#[cfg(all(windows, feature = "native-sync", feature = "native-test-hooks"))]
#[test]
#[ignore = "explicit NEW generated CFAPI root; atomic replacement and conversion recovery"]
fn native_atomic_replacement_retains_bytes_and_retries_after_conversion() {
    use std::sync::atomic::AtomicBool;
    use twodrive_local_files::native::{HydrationSource, persistent::PersistentSession};
    struct Source;
    impl HydrationSource for Source {
        fn size(&self, id: &[u8]) -> Result<usize, String> {
            if id != OBJECT.as_bytes() {
                return Err("foreign generated identity".into());
            }
            Ok(INITIAL.len())
        }
        fn read_range(
            &self,
            id: &[u8],
            offset: usize,
            length: usize,
            cancelled: &AtomicBool,
        ) -> Result<Vec<u8>, String> {
            self.size(id)?;
            if cancelled.load(Ordering::SeqCst) {
                return Err("cancelled".into());
            }
            INITIAL
                .get(offset..offset + length)
                .map(|b| b.to_vec())
                .ok_or("invalid generated range".into())
        }
    }
    let root = PathBuf::from(
        std::env::var_os("TWODRIVE_APPROVED_NATIVE_REPLACEMENT_ROOT")
            .expect("explicit new root required"),
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
    let checkpoint = root.with_extension("receipt.json");
    let provider = MockProvider::new();
    let f = fixture(&provider);
    f.bridge.hydrate(OBJECT).unwrap();
    let mut owner = PersistentSession::create_new(
        &root,
        b"twodrive-dev-native-v1:generated-office-only",
        &checkpoint,
        Arc::new(Source),
    )
    .unwrap();
    let relative = Path::new("generated-中文.txt");
    let visible = owner
        .publish_placeholder(relative, OBJECT.as_bytes(), INITIAL.len() as u64, false, 1)
        .unwrap();
    assert_eq!(fs::read(&visible).unwrap(), INITIAL);
    let temporary = root.join("generated-save.tmp");
    fs::write(&temporary, EDITED).unwrap();
    fs::rename(&temporary, &visible).unwrap();
    let reader = fs::File::open(&visible).unwrap();
    assert!(
        f.bridge
            .capture_native_replacement(&owner, relative, OBJECT)
            .is_err()
    );
    assert!(f.bridge.status(OBJECT).unwrap().native_dirty);
    drop(reader);
    f.bridge
        .capture_native_replacement(&owner, relative, OBJECT)
        .unwrap();
    assert_eq!(fs::read(&visible).unwrap(), EDITED);
    assert_eq!(bytes(&f.bridge), EDITED);
    assert!(
        owner
            .with_replacement_capture(relative, b"foreign", |_| Ok(()))
            .is_err()
    );
    assert!(!f.bridge.acknowledge_native(&visible, OBJECT).unwrap());
    assert!(f.bridge.upload(OBJECT, no_progress()).unwrap());
    assert!(f.bridge.acknowledge_native(&visible, OBJECT).unwrap());
    assert_eq!(provider.download(OBJECT).unwrap(), EDITED);

    let newer = vec![b'Z'; 512];
    fs::write(&temporary, &newer).unwrap();
    fs::rename(&temporary, &visible).unwrap();
    f.bridge.journal.record_native_edit(OBJECT).unwrap();
    assert!(
        owner
            .with_replacement_capture::<()>(relative, OBJECT.as_bytes(), |_| Err(
                "injected interruption before journal publication".into()
            ))
            .is_err()
    );
    owner.disconnect().unwrap();
    drop(owner);
    drop(f.bridge);
    let bridge = LocalFileBridge::open(
        f.db.clone(),
        &f.cache,
        Backend::new(provider.clone()),
        "generated-provider-account-1",
    )
    .unwrap();
    assert!(bridge.status(OBJECT).unwrap().native_dirty);
    let mut resumed =
        PersistentSession::resume_from_checkpoint(&root, &checkpoint, Arc::new(Source)).unwrap();
    bridge
        .capture_native_replacement(&resumed, relative, OBJECT)
        .unwrap();
    assert!(bridge.upload(OBJECT, no_progress()).unwrap());
    assert!(bridge.acknowledge_native(&visible, OBJECT).unwrap());
    assert_eq!(fs::read(&visible).unwrap(), newer);
    assert_eq!(provider.download(OBJECT).unwrap(), newer);
    fs::remove_file(&visible).unwrap();
    resumed.retire_empty_office_fixture().unwrap();
    drop(resumed);
    fs::remove_dir(&root).unwrap();
    fs::remove_file(checkpoint).unwrap();
}

#[cfg(all(windows, feature = "native-sync", feature = "native-test-hooks"))]
#[test]
#[ignore = "explicit NEW generated CFAPI root; complete replacement before first hydration"]
fn native_cold_replacement_creates_a_dirty_snapshot_without_old_download() {
    use std::sync::atomic::AtomicBool;
    use twodrive_local_files::native::{HydrationSource, persistent::PersistentSession};
    struct NoDownload;
    impl HydrationSource for NoDownload {
        fn size(&self, _: &[u8]) -> Result<usize, String> {
            Err("cold replacement must not download".into())
        }
        fn read_range(
            &self,
            _: &[u8],
            _: usize,
            _: usize,
            _: &AtomicBool,
        ) -> Result<Vec<u8>, String> {
            Err("no download".into())
        }
    }
    let root = PathBuf::from(
        std::env::var_os("TWODRIVE_APPROVED_COLD_REPLACEMENT_ROOT")
            .expect("explicit new generated root"),
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
    let checkpoint = root.with_extension("receipt.json");
    let provider = MockProvider::new();
    let f = fixture(&provider);
    f.db.upsert_metadata(&provider.get_metadata(OBJECT).unwrap().unwrap())
        .unwrap();
    let mut owner = PersistentSession::create_new(
        &root,
        b"twodrive-dev-native-v1:generated-office-only",
        &checkpoint,
        Arc::new(NoDownload),
    )
    .unwrap();
    let relative = Path::new("generated-中文.txt");
    let visible = owner
        .publish_placeholder(relative, OBJECT.as_bytes(), INITIAL.len() as u64, false, 1)
        .unwrap();
    let temp = root.join("generated-complete.tmp");
    fs::write(&temp, EDITED).unwrap();
    fs::rename(&temp, &visible).unwrap();
    assert!(
        f.db.get_by_remote_id(OBJECT)
            .unwrap()
            .unwrap()
            .cache_path
            .is_none()
    );
    let operation = (|| -> anyhow::Result<()> {
        let generation = f
            .bridge
            .capture_native_replacement(&owner, relative, OBJECT)?;
        assert_eq!(generation, 1);
        assert_eq!(bytes(&f.bridge), EDITED);
        assert!(!f.bridge.status(OBJECT)?.sync_confirmed());
        assert!(f.bridge.upload(OBJECT, no_progress())?);
        assert!(f.bridge.acknowledge_native(&visible, OBJECT)?);
        assert_eq!(provider.download(OBJECT)?, EDITED);
        Ok(())
    })();
    assert_eq!(fs::read(&visible).unwrap(), EDITED);
    fs::remove_file(&visible).unwrap();
    owner.retire_empty_office_fixture().unwrap();
    drop(owner);
    fs::remove_dir(&root).unwrap();
    fs::remove_file(checkpoint).unwrap();
    operation.unwrap();
}

#[cfg(all(windows, feature = "native-sync", feature = "native-test-hooks"))]
#[test]
#[ignore = "explicit NEW generated CFAPI root; local creation and conversion interruption recovery"]
fn native_new_file_keeps_its_local_identity_after_cloud_binding() {
    use std::sync::atomic::AtomicBool;
    use twodrive_local_files::native::{HydrationSource, persistent::PersistentSession};
    struct NoDownload;
    impl HydrationSource for NoDownload {
        fn size(&self, _: &[u8]) -> Result<usize, String> {
            Err("new file is resident".into())
        }
        fn read_range(
            &self,
            _: &[u8],
            _: usize,
            _: usize,
            _: &AtomicBool,
        ) -> Result<Vec<u8>, String> {
            Err("no download".into())
        }
    }
    let root = PathBuf::from(
        std::env::var_os("TWODRIVE_APPROVED_NEW_FILE_ROOT").expect("explicit new root"),
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
    let checkpoint = root.with_extension("receipt.json");
    let provider = MockProvider::empty();
    let f = fixture(&provider);
    let mut owner = PersistentSession::create_new(
        &root,
        b"twodrive-dev-native-v1:generated-office-only",
        &checkpoint,
        Arc::new(NoDownload),
    )
    .unwrap();
    let relative = Path::new("generated-中文.txt");
    let visible = root.join(relative);
    fs::write(&visible, EDITED).unwrap();
    let id = "local-upload-native-new";
    let root_binding = serde_json::to_string(owner.receipt()).unwrap();
    assert!(
        owner
            .with_replacement_capture_staged::<()>(
                relative,
                id.as_bytes(),
                |volume, file_id| {
                    f.bridge
                        .journal
                        .begin_native_creation(
                            id,
                            REMOTE_PATH,
                            &root_binding,
                            &format!("{volume:08x}:{file_id:016x}"),
                        )
                        .map_err(|e| e.to_string())
                },
                |_| Err("injected interruption after conversion before snapshot".into())
            )
            .is_err()
    );
    assert_eq!(
        f.bridge.journal.native_creations().unwrap()[0].state,
        "preparing"
    );
    owner.disconnect().unwrap();
    drop(owner);
    drop(f.bridge);
    let bridge = LocalFileBridge::open(
        f.db.clone(),
        &f.cache,
        Backend::new(provider.clone()),
        "generated-provider-account-1",
    )
    .unwrap();
    let mut owner =
        PersistentSession::resume_from_checkpoint(&root, &checkpoint, Arc::new(NoDownload))
            .unwrap();
    bridge.create_native_file(&owner, relative, id).unwrap();
    assert_eq!(
        bridge.journal.native_creations().unwrap()[0].state,
        "staged"
    );
    assert!(bridge.upload(id, no_progress()).unwrap());
    assert!(bridge.acknowledge_native(&visible, id).unwrap());
    assert_eq!(
        bridge.journal.native_creations().unwrap()[0]
            .cloud_id
            .as_deref(),
        Some(OBJECT)
    );
    fs::write(&visible, b"new local edit after first upload").unwrap();
    bridge
        .capture_native_replacement(&owner, relative, id)
        .unwrap();
    assert!(bridge.upload(id, no_progress()).unwrap());
    assert!(bridge.acknowledge_native(&visible, id).unwrap());
    assert_eq!(
        provider.download(OBJECT).unwrap(),
        b"new local edit after first upload"
    );
    assert_eq!(
        f.db.get_by_remote_id(id)
            .unwrap()
            .unwrap()
            .metadata
            .remote_id,
        id
    );
    fs::remove_file(&visible).unwrap();
    owner.retire_empty_office_fixture().unwrap();
    drop(owner);
    fs::remove_dir(&root).unwrap();
    fs::remove_file(checkpoint).unwrap();
}

#[test]
fn local_creation_binds_a_real_id_and_recovers_a_lost_reply_without_overwriting() {
    for lost_reply in [false, true] {
        let provider = MockProvider::empty();
        let f = fixture(&provider);
        let id = "local-upload-new-native";
        let snapshot = f.cache.join("new-complete");
        fs::write(&snapshot, EDITED).unwrap();
        f.bridge.stage_creation(id, REMOTE_PATH, &snapshot).unwrap();
        assert!(!f.bridge.status(id).unwrap().sync_confirmed());
        assert!(
            f.db.get_by_remote_id(id)
                .unwrap()
                .unwrap()
                .cloud_remote_id
                .is_none()
        );
        if lost_reply {
            provider.mode.store(2, Ordering::SeqCst);
            assert!(f.bridge.upload(id, no_progress()).is_err());
            assert!(f.bridge.status(id).unwrap().uncertain_upload);
            drop(f.bridge);
            provider.mode.store(0, Ordering::SeqCst);
            let reopened = LocalFileBridge::open(
                f.db.clone(),
                &f.cache,
                Backend::new(provider.clone()),
                "generated-provider-account-1",
            )
            .unwrap();
            assert!(reopened.reconcile(id).unwrap());
            assert!(reopened.status(id).unwrap().sync_confirmed());
        } else {
            assert!(f.bridge.upload(id, no_progress()).unwrap());
        }
        let record = f.db.get_by_remote_id(id).unwrap().unwrap();
        assert_eq!(record.cloud_remote_id.as_deref(), Some(OBJECT));
        assert_eq!(record.metadata.remote_id, id);
        assert_eq!(provider.download(OBJECT).unwrap(), EDITED);
        assert_eq!(fs::read(&snapshot).unwrap(), EDITED);
    }
    let provider = MockProvider::new();
    let f = fixture(&provider);
    let snapshot = f.cache.join("must-not-replace");
    fs::write(&snapshot, EDITED).unwrap();
    f.bridge
        .stage_creation("local-upload-conflicting", REMOTE_PATH, &snapshot)
        .unwrap();
    assert!(
        f.bridge
            .upload("local-upload-conflicting", no_progress())
            .is_err()
    );
    assert_eq!(
        f.bridge.status("local-upload-conflicting").unwrap().state,
        FileState::Conflict
    );
    assert_eq!(provider.download(OBJECT).unwrap(), INITIAL);
    assert_eq!(fs::read(&snapshot).unwrap(), EDITED);
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
#[cfg(all(windows, feature = "native-local-fixture"))]
fn pinned_cache_mismatch_during_native_ack_is_durable_recovery_work() {
    let provider = MockProvider::new();
    let f = fixture(&provider);
    f.bridge.hydrate(OBJECT).unwrap();
    f.db.mark_pinned(OBJECT).unwrap();
    let record = f.db.get_by_remote_id(OBJECT).unwrap().unwrap();
    fs::write(record.cache_path.unwrap(), EDITED).unwrap();
    // The changed private snapshot must be rejected before opening any native
    // path; no registered root is needed to test this acknowledgement branch.
    assert!(
        !f.bridge
            .acknowledge_native(&f.cache.join("not-a-native-path"), OBJECT)
            .unwrap()
    );
    let state = f.bridge.status(OBJECT).unwrap();
    assert!(!state.sync_confirmed());
    assert!(state.pinned && state.dirty_intent && state.recovery_required);
    assert_eq!(state.generation, 1);
    assert_eq!(bytes(&f.bridge), EDITED);
    assert_eq!(provider.download(OBJECT).unwrap(), INITIAL);
    assert!(!f.bridge.journal.record_external_edit(OBJECT, 0).unwrap());
    f.bridge.journal.recover_write(OBJECT).unwrap();
    assert!(f.bridge.upload(OBJECT, no_progress()).unwrap());
    assert!(f.bridge.status(OBJECT).unwrap().pinned);
    assert_eq!(provider.download(OBJECT).unwrap(), EDITED);
    assert!(!f.bridge.engine_release_ready(OBJECT, 1).unwrap());
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

#[test]
fn published_reader_refuses_new_remote_versions_and_holds_one_verified_lease() {
    use std::sync::atomic::AtomicBool;
    let provider = MockProvider::new();
    let f = fixture(&provider);
    let expected = provider.get_metadata(OBJECT).unwrap().unwrap();
    f.db.upsert_metadata(&expected).unwrap();
    let published = f.db.get_by_remote_id(OBJECT).unwrap().unwrap();
    provider.change(b"same identity, newer unpublished cloud content");
    let cancel = AtomicBool::new(false);
    assert!(f.bridge.open_published_read(&published, &cancel).is_err());
    let retained = f.db.get_by_remote_id(OBJECT).unwrap().unwrap();
    assert_eq!(retained.metadata.etag, published.metadata.etag);
    assert!(retained.cache_path.is_none());
    assert_eq!(retained.state, FileState::OnlineOnly);
    // Independently establish a current publication, then retain it throughout
    // multiple bounded transfers. Server changes cannot change that cache lease.
    let remote = provider.get_metadata(OBJECT).unwrap().unwrap();
    f.db.upsert_metadata(&remote).unwrap();
    let published = f.db.get_by_remote_id(OBJECT).unwrap().unwrap();
    let mut read = f.bridge.open_published_read(&published, &cancel).unwrap();
    assert_eq!(f.bridge.status(OBJECT).unwrap().readers, 1);
    assert!(f.bridge.edit(OBJECT).is_err());
    provider.change(b"another cloud version during the retained fetch");
    assert_eq!(read.read_range(0, 4, &cancel).unwrap(), b"same");
    assert_eq!(read.read_range(4, 9, &cancel).unwrap(), b" identity");
    cancel.store(true, Ordering::SeqCst);
    assert!(read.read_range(0, 1, &cancel).is_err());
    drop(read);
    assert_eq!(f.bridge.status(OBJECT).unwrap().readers, 0);
    cancel.store(false, Ordering::SeqCst);
    let mut writer = f.bridge.edit(OBJECT).unwrap();
    writer.write_all(b"local edited").unwrap();
    writer.finish().unwrap();
    assert!(f.bridge.open_published_read(&published, &cancel).is_err());
}

#[test]
fn published_reader_preserves_corruption_for_recovery_instead_of_hydrating_it() {
    use std::sync::atomic::AtomicBool;
    let provider = MockProvider::new();
    let f = fixture(&provider);
    f.bridge.hydrate(OBJECT).unwrap();
    let published = f.db.get_by_remote_id(OBJECT).unwrap().unwrap();
    let path = published.cache_path.as_ref().unwrap();
    fs::write(path, EDITED).unwrap();
    assert!(
        f.bridge
            .open_published_read(&published, &AtomicBool::new(false))
            .is_err()
    );
    assert_eq!(fs::read(path).unwrap(), EDITED);
    assert!(!f.bridge.status(OBJECT).unwrap().sync_confirmed());
    assert!(f.bridge.status(OBJECT).unwrap().recovery_required);
}

#[test]
fn paused_first_upload_never_calls_provider_and_remains_retryable_after_restart() {
    let provider = MockProvider::empty();
    let f = fixture(&provider);
    let pause = f.cache.join("paused");
    let bridge = LocalFileBridge {
        journal: f.bridge.journal.clone(),
        backend: Backend::new(provider.clone()).with_pause_file(pause.clone()),
    };
    let path = f.cache.join("new-native.bin");
    fs::write(&path, EDITED).unwrap();
    let id = "local-upload-paused-new-file";
    bridge.stage_creation(id, REMOTE_PATH, &path).unwrap();
    fs::write(&pause, b"paused").unwrap();
    assert!(bridge.upload(id, no_progress()).is_err());
    assert_eq!(provider.upload_calls.load(Ordering::SeqCst), 0);
    let state = bridge.status(id).unwrap();
    assert_eq!(state.state, FileState::Dirty);
    assert!(state.upload_generation.is_none() && !state.uncertain_upload);
    assert_eq!(fs::read(&path).unwrap(), EDITED);
    drop(bridge);
    let Fixture {
        db,
        cache,
        bridge: owner,
        _temp,
    } = f;
    drop(owner);
    fs::remove_file(&pause).unwrap();
    let reopened = LocalFileBridge::open(
        db,
        &cache,
        Backend::new(provider.clone()),
        "generated-provider-account-1",
    )
    .unwrap();
    assert!(reopened.upload(id, no_progress()).unwrap());
    assert_eq!(provider.upload_calls.load(Ordering::SeqCst), 1);
    let bound = reopened
        .journal
        .database()
        .get_by_remote_id(id)
        .unwrap()
        .unwrap();
    assert_eq!(bound.cloud_remote_id.as_deref(), Some(OBJECT));
    assert_eq!(fs::read(bound.cache_path.unwrap()).unwrap(), EDITED);
    assert_eq!(provider.download(OBJECT).unwrap(), EDITED);
    drop(reopened);
    drop(_temp);
}
