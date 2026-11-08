//! Shared-engine private-cache bridge. No CFAPI, GUI, provider switch or root
//! registration. Native capabilities stay disabled until the adapter owns the
//! corresponding OS locks and recovery path.
use crate::model::{CloudBackend, MetadataEntry};
use crate::provider::{Backend, Progress};
use anyhow::{Context, ensure};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};
use twodrive_core::Database;
use twodrive_core::{CacheJournal, CacheSnapshot, CacheWriteLease};

#[derive(Clone)]
pub struct LocalFileBridge {
    journal: CacheJournal,
    backend: Backend,
}
#[cfg(all(test, windows, feature = "native-test-hooks"))]
thread_local! {pub(crate) static INTERRUPT_BACKUP_AFTER_REVERT:std::cell::Cell<bool>=const {std::cell::Cell::new(false)};}
#[cfg(all(test, windows, feature = "native-test-hooks"))]
thread_local! {pub(crate) static INTERRUPT_REMOTE_AFTER_NATIVE_UPDATE:std::cell::Cell<bool>=const {std::cell::Cell::new(false)};}
#[cfg(all(test, windows, feature = "native-test-hooks"))]
thread_local! {pub(crate) static INTERRUPT_REMOTE_AFTER_RENAME:std::cell::Cell<bool>=const {std::cell::Cell::new(false)};}
#[cfg(all(test, windows, feature = "native-test-hooks"))]
thread_local! {pub(crate) static INTERRUPT_REMOTE_AFTER_REMOVAL:std::cell::Cell<bool>=const {std::cell::Cell::new(false)};}
impl LocalFileBridge {
    #[cfg(all(windows, feature = "native-sync"))]
    pub(crate) fn apply_native_remote_removal(
        &self,
        owner: &twodrive_local_files::native::persistent::PersistentSession,
        expected: &twodrive_core::NativeProjection,
        pending: Option<twodrive_core::NativeRemoteRemoval>,
    ) -> anyhow::Result<()> {
        let relative: std::path::PathBuf = expected
            .path
            .strip_prefix('/')
            .context("remote removal path unrooted")?
            .split('/')
            .collect();
        if let Some(removal) = pending.as_ref()
            && !owner.entry_exists(&relative).map_err(anyhow::Error::msg)?
        {
            return self.journal.complete_native_remote_removal(removal);
        }
        let physical = twodrive_core::NativeFileIdentity {
            volume_serial: expected
                .volume_serial
                .context("remote removal volume missing")?,
            file_id: u64::from_str_radix(
                expected
                    .native_file_id
                    .as_deref()
                    .context("remote removal identity missing")?,
                16,
            )?,
        };
        owner
            .with_remote_change(
                &relative,
                &relative,
                &expected.tag,
                physical,
                expected.directory,
                |capture, _| {
                    let observed = capture.inspect_metadata()?;
                    if !observed.os_in_sync
                        || observed.modified_bytes != 0
                        || observed.logical_size != expected.size
                    {
                        return Err("remote deletion preserves local native changes".into());
                    }
                    let removal = if let Some(removal) = pending {
                        removal
                    } else {
                        self.journal
                            .prepare_native_remote_removal(expected, |file, hash| {
                                ensure!(
                                    digest_file(file)? == hash,
                                    "private bytes changed before remote removal"
                                );
                                Ok(())
                            })
                            .map_err(|e| e.to_string())?
                    };
                    capture.remove_remote_deleted()?;
                    #[cfg(all(test, feature = "native-test-hooks"))]
                    if INTERRUPT_REMOTE_AFTER_REMOVAL.with(|flag| flag.replace(false)) {
                        return Err("generated interruption after native remote deletion".into());
                    }
                    self.journal
                        .complete_native_remote_removal(&removal)
                        .map_err(|e| e.to_string())
                },
            )
            .map_err(anyhow::Error::msg)
    }
    #[cfg(all(windows, feature = "native-sync"))]
    pub(crate) fn apply_native_remote_version(
        &self,
        owner: &twodrive_local_files::native::persistent::PersistentSession,
        expected: &twodrive_core::NativeProjection,
        next: &MetadataEntry,
        pending: Option<twodrive_core::NativeRemoteUpdate>,
    ) -> anyhow::Result<()> {
        let relative: std::path::PathBuf = expected
            .path
            .strip_prefix('/')
            .context("remote path must be rooted")?
            .split('/')
            .collect();
        let physical = twodrive_core::NativeFileIdentity {
            volume_serial: expected
                .volume_serial
                .context("remote native volume missing")?,
            file_id: u64::from_str_radix(
                expected
                    .native_file_id
                    .as_deref()
                    .context("remote native physical identity missing")?,
                16,
            )?,
        };
        let target: std::path::PathBuf = next
            .path
            .strip_prefix('/')
            .context("remote destination must be rooted")?
            .split('/')
            .collect();
        let actual = if pending.is_some() && expected.path != next.path {
            let located = owner
                .locate_file_id(physical.volume_serial, physical.file_id)
                .map_err(anyhow::Error::msg)?
                .context("remote rename recovery identity missing")?;
            located
                .strip_prefix(owner.root())
                .context("remote rename recovery left root")?
                .to_path_buf()
        } else {
            relative
        };
        owner
            .with_remote_change(
                &actual,
                &target,
                &expected.tag,
                physical,
                expected.directory,
                |capture, rename| {
                    let observed = capture.inspect_metadata()?;
                    let stamp = capture
                        .modified_time()?
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(|e| e.to_string())?
                        .as_secs() as i64;
                    let resumed_rename = pending.is_some()
                        && expected.path != next.path
                        && actual == target
                        && (stamp == expected.modified_unix || stamp == next.modified_unix)
                        && (observed.logical_size == expected.size
                            || observed.logical_size == next.size);
                    if (!observed.os_in_sync && !resumed_rename) || observed.modified_bytes != 0 {
                        return Err("remote update preserves native edits".into());
                    }
                    if pending.is_none() && observed.logical_size != expected.size {
                        return Err("remote update native size changed".into());
                    }
                    if observed.pinned || observed.release_requested {
                        self.journal
                            .mirror_native_pins(&[(
                                expected.local_id.clone(),
                                observed.pinned,
                                observed.release_requested,
                            )])
                            .map_err(|e| e.to_string())?;
                    }
                    let preserve_pin = self
                        .journal
                        .database()
                        .get_by_remote_id(&expected.local_id)
                        .map_err(|e| e.to_string())?
                        .ok_or("remote pin owner missing")?
                        .effective_pinned();
                    let update = if let Some(update) = pending {
                        update
                    } else {
                        self.journal
                            .prepare_native_remote_update(expected, next, |file, digest| {
                                ensure!(
                                    digest_file(file)? == digest,
                                    "old private version changed before remote update"
                                );
                                Ok(())
                            })
                            .map_err(|e| e.to_string())?
                    };
                    rename()?;
                    #[cfg(all(test, feature = "native-test-hooks"))]
                    if INTERRUPT_REMOTE_AFTER_RENAME.with(|flag| flag.replace(false)) {
                        return Err("generated interruption after remote owned rename".into());
                    }
                    capture.update_remote_version(
                        next.size,
                        next.modified_unix,
                        actual != target || resumed_rename,
                        preserve_pin,
                    )?;
                    #[cfg(all(test, feature = "native-test-hooks"))]
                    if INTERRUPT_REMOTE_AFTER_NATIVE_UPDATE.with(|flag| flag.replace(false)) {
                        return Err("generated interruption after native remote update".into());
                    }
                    self.journal
                        .complete_native_remote_update(&update, physical)
                        .map_err(|e| e.to_string())
                },
            )
            .map_err(anyhow::Error::msg)
    }
    #[cfg(all(windows, feature = "native-local-fixture"))]
    #[cfg(test)]
    pub(crate) fn generated_fixture(
        db: twodrive_core::Database,
        cache: &Path,
    ) -> anyhow::Result<Self> {
        Self::generated_fixture_dataset(
            db,
            cache,
            twodrive_local_files::fixtures::Dataset::StandardV1,
            Arc::default(),
        )
    }
    #[cfg(all(windows, feature = "native-local-fixture"))]
    pub(crate) fn generated_fixture_dataset(
        db: twodrive_core::Database,
        cache: &Path,
        dataset: twodrive_local_files::fixtures::Dataset,
        counters: Arc<twodrive_local_files::fixtures::DownloadCounters>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            journal: CacheJournal::open(
                db,
                cache,
                &format!("{:x}", Sha256::digest(dataset.provider_binding())),
            )?,
            backend: Backend::new(crate::native_local_fixture::GeneratedProvider {
                dataset,
                counters,
            }),
        })
    }
    #[cfg(test)]
    pub(crate) fn open(
        db: Database,
        private_cache: &Path,
        backend: Backend,
        provider_binding: &str,
    ) -> anyhow::Result<Self> {
        Self::open_bound(db, private_cache, backend, provider_binding)
    }

    pub(crate) fn open_bound(
        db: Database,
        private_cache: &Path,
        backend: Backend,
        provider_binding: &str,
    ) -> anyhow::Result<Self> {
        ensure!(
            !provider_binding.is_empty() && provider_binding.len() <= 16384,
            "missing or invalid provider binding"
        );
        let digest = format!(
            "{:x}",
            Sha256::digest(
                [
                    b"twodrive-local-cache-binding-v1\0".as_slice(),
                    provider_binding.as_bytes()
                ]
                .concat()
            )
        );
        Ok(Self {
            journal: CacheJournal::open(db, private_cache, &digest)?,
            backend,
        })
    }
    pub fn journal(&self) -> &CacheJournal {
        &self.journal
    }
    #[cfg(all(windows, feature = "native-sync"))]
    pub(crate) fn split_native_backup(
        &self,
        owner: &twodrive_local_files::native::persistent::PersistentSession,
        backup: &Path,
        target: &Path,
        source: &twodrive_core::NativeProjection,
        physical: twodrive_core::NativeFileIdentity,
    ) -> anyhow::Result<()> {
        let id = &source.local_id;
        if self
            .journal
            .snapshot_if_owned(id)?
            .as_ref()
            .is_some_and(|s| s.uncertain_upload)
        {
            self.reconcile(id)?;
        }
        let record = self
            .journal
            .database()
            .get_by_remote_id(id)?
            .context("backup logical source missing")?;
        let existing = self
            .journal
            .native_backup_copies(&source.root_binding)?
            .into_iter()
            .find(|copy| copy.physical == physical && copy.source.local_id == *id);
        let backup_path = format!(
            "/{}",
            backup
                .components()
                .map(|p| p.as_os_str().to_str().context("invalid backup path"))
                .collect::<anyhow::Result<Vec<_>>>()?
                .join("/")
        );
        self.journal.record_native_edit(&source.cloud_id)?;
        owner
            .with_backup_capture(backup, &source.tag, physical, |backup_capture| {
                let Some(backup_capture) = backup_capture else {
                    return self
                        .journal
                        .complete_native_backup_copy(
                            existing
                                .as_ref()
                                .ok_or("ordinary backup has no prepared owner")?
                                .sequence,
                            digest_file,
                        )
                        .map_err(|e| e.to_string());
                };
                self.check_native_projection(backup_capture, source, false)?;
                let modified = backup_capture.modified_time()?;
                let mut snapshot = tempfile::NamedTempFile::new_in(self.journal.cache_root())
                    .map_err(|e| e.to_string())?;
                let size = backup_capture.copy_resident_to(snapshot.as_file_mut())?;
                snapshot
                    .as_file()
                    .set_times(std::fs::FileTimes::new().set_modified(modified))
                    .map_err(|e| e.to_string())?;
                snapshot.as_file().sync_all().map_err(|e| e.to_string())?;
                let digest = digest_file(snapshot.as_file_mut()).map_err(|e| e.to_string())?;
                owner.with_replacement_capture(target, &source.tag, |target_capture| {
                    let generation = self
                        .journal
                        .snapshot_if_owned(id)
                        .map_err(|e| e.to_string())?
                        .map(|s| s.generation);
                    self.capture_locked_native(target_capture, &record, generation)?;
                    let (volume_serial, file_id) = target_capture.physical_identity()?;
                    let target = twodrive_core::NativeFileIdentity {
                        volume_serial,
                        file_id,
                    };
                    let prepared = if let Some(copy) = existing.filter(|copy| {
                        copy.digest == digest && copy.size == size && copy.target == target
                    }) {
                        drop(snapshot);
                        copy
                    } else {
                        let (file, cache_path) = snapshot.keep().map_err(|e| e.to_string())?;
                        drop(file);
                        self.journal
                            .prepare_native_backup_copy(
                                twodrive_core::NativeBackupCopy {
                                    sequence: 0,
                                    root_file_id: owner.receipt().file_id,
                                    history_through: 0,
                                    source: source.clone(),
                                    path: backup_path,
                                    physical,
                                    target,
                                    cache_path,
                                    digest,
                                    size,
                                    modified_unix: modified
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map_err(|e| e.to_string())?
                                        .as_secs()
                                        as i64,
                                    state: "prepared".into(),
                                },
                                digest_file,
                            )
                            .map_err(|e| e.to_string())?
                    };
                    backup_capture.revert_after_verified_backup()?;
                    #[cfg(all(test, feature = "native-test-hooks"))]
                    if INTERRUPT_BACKUP_AFTER_REVERT.with(|flag| flag.replace(false)) {
                        return Err("generated interruption after backup revert".into());
                    }
                    self.journal
                        .complete_native_backup_copy(prepared.sequence, digest_file)
                        .map_err(|e| e.to_string())
                })
            })
            .map_err(anyhow::Error::msg)
    }
    #[cfg(all(windows, feature = "native-sync"))]
    pub(crate) fn backend(&self) -> &Backend {
        &self.backend
    }
    pub fn status(&self, id: &str) -> anyhow::Result<CacheSnapshot> {
        self.journal.snapshot(id)
    }
    pub fn edit(&self, id: &str) -> anyhow::Result<CacheWriteLease> {
        self.journal.open_write(id)
    }

    pub fn stage_creation(
        &self,
        id: &str,
        cloud_path: &str,
        snapshot: &Path,
    ) -> anyhow::Result<()> {
        crate::provider::valid_path(cloud_path)?;
        self.journal.stage_local_creation(id, cloud_path, snapshot)
    }

    #[cfg(all(
        windows,
        any(feature = "native-local-fixture", feature = "native-sync")
    ))]
    pub fn capture_native_edit(&self, path: &Path, id: &str) -> anyhow::Result<i64> {
        let record = self
            .journal
            .database()
            .get_by_remote_id(id)?
            .context("unknown native item")?;
        let cloud_id = record.cloud_remote_id.as_deref().unwrap_or(id);
        self.journal.record_native_edit(cloud_id)?;
        let generation = self
            .journal
            .snapshot_if_owned(id)?
            .map(|state| state.generation);
        #[cfg(feature = "native-sync")]
        let projection = self.journal.native_projection_for_id(id)?;
        #[cfg(feature = "native-sync")]
        let identity = projection
            .as_ref()
            .map(|p| p.tag.clone())
            .unwrap_or_else(|| id.as_bytes().to_vec());
        #[cfg(not(feature = "native-sync"))]
        let identity = id.as_bytes().to_vec();
        twodrive_local_files::native::with_native_capture(path, &identity, |locked| {
            #[cfg(feature = "native-sync")]
            if let Some(projection) = projection.as_ref() {
                self.check_native_projection(locked, projection, false)?;
            }
            self.capture_locked_native(locked, &record, generation)
        })
        .map_err(anyhow::Error::msg)
    }

    #[cfg(all(windows, feature = "native-sync"))]
    fn check_native_projection(
        &self,
        locked: &twodrive_local_files::native::NativeCapture<'_>,
        projection: &twodrive_core::NativeProjection,
        acknowledgement: bool,
    ) -> Result<(), String> {
        let (volume, file_id) = locked.physical_identity()?;
        let (root_id, identity) = locked.root_registration()?;
        let captured = self
            .journal
            .native_capture_binding(&projection.local_id)
            .map_err(|e| e.to_string())?;
        let published_matches = projection.volume_serial == Some(volume)
            && projection.native_file_id.as_deref() == Some(format!("{file_id:016x}").as_str());
        let captured_matches = captured.is_some_and(|binding| {
            binding.physical
                == twodrive_core::NativeFileIdentity {
                    volume_serial: volume,
                    file_id,
                }
        });
        let physical_matches = if acknowledgement && captured.is_some() {
            captured_matches
        } else {
            published_matches || captured_matches
        };
        if projection.state != "published"
            || !physical_matches
            || root_id == 0
            || identity != projection.root_binding.as_bytes()
        {
            return Err("native capture differs from its durable publication".into());
        }
        Ok(())
    }

    #[cfg(all(
        windows,
        any(feature = "native-local-fixture", feature = "native-sync")
    ))]
    fn capture_locked_native(
        &self,
        locked: &twodrive_local_files::native::NativeCapture<'_>,
        record: &twodrive_core::FileRecord,
        generation: Option<i64>,
    ) -> Result<i64, String> {
        if let Some(expected) = generation {
            let state = self
                .status(&record.metadata.remote_id)
                .map_err(|e| e.to_string())?;
            if state.generation != expected
                || state.readers != 0
                || state.writers != 0
                || state.upload_generation.is_some()
                || state.uncertain_upload
                || state.pending_operations
                || !matches!(
                    state.state,
                    twodrive_core::FileState::Dirty
                        | twodrive_core::FileState::Cached
                        | twodrive_core::FileState::Pinned
                )
            {
                return Err(format!(
                    "native capture awaits existing cache work: generation_match={} readers={} writers={} upload={} uncertain={} recovery={} pending={} state={:?}",
                    state.generation == expected,
                    state.readers,
                    state.writers,
                    state.upload_generation.is_some(),
                    state.uncertain_upload,
                    state.recovery_required,
                    state.pending_operations,
                    state.state
                ));
            }
        }
        let modified = locked.modified_time()?;
        let mut temporary = tempfile::NamedTempFile::new_in(self.journal.cache_root())
            .map_err(|e| e.to_string())?;
        let size = locked.copy_resident_to(temporary.as_file_mut())?;
        if modified != locked.modified_time()? {
            return Err("native source timestamp changed during capture".into());
        }
        temporary
            .as_file()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .map_err(|e| e.to_string())?;
        temporary.as_file().sync_all().map_err(|e| e.to_string())?;
        // Keep the owned file on any commit error: it contains user edits,
        // and must not be mistaken for a disposable download cache.
        let (file, captured) = temporary.keep().map_err(|e| e.to_string())?;
        drop(file);
        #[cfg(feature = "native-sync")]
        if let Some(projection) = self
            .journal
            .native_projection_for_id(&record.metadata.remote_id)
            .map_err(|e| e.to_string())?
        {
            let (_, root_identity) = locked.root_registration()?;
            if root_identity != projection.root_binding.as_bytes() {
                return Err("native capture root changed".into());
            }
            let (volume_serial, file_id) = locked.physical_identity()?;
            let physical = twodrive_core::NativeFileIdentity {
                volume_serial,
                file_id,
            };
            return match generation {
                Some(generation) => self.journal.stage_projected_native_edit(
                    &record.metadata.remote_id,
                    generation,
                    &captured,
                    size,
                    &projection,
                    physical,
                ),
                None => self.journal.stage_first_projected_native_edit(
                    record,
                    &captured,
                    size,
                    &projection,
                    physical,
                ),
            }
            .map_err(|e| e.to_string());
        }
        match generation {
            Some(generation) => self.journal.stage_native_edit(
                &record.metadata.remote_id,
                generation,
                &captured,
                size,
            ),
            None => self
                .journal
                .stage_first_native_edit(record, &captured, size),
        }
        .map_err(|e| e.to_string())
    }

    #[cfg(all(windows, feature = "native-sync"))]
    pub fn capture_native_replacement(
        &self,
        owner: &twodrive_local_files::native::persistent::PersistentSession,
        relative: &Path,
        id: &str,
    ) -> anyhow::Result<i64> {
        let record = self
            .journal
            .database()
            .get_by_remote_id(id)?
            .context("unknown replacement item")?;
        let cloud_id = record.cloud_remote_id.as_deref().unwrap_or(id);
        let parts = relative
            .components()
            .map(|part| match part {
                std::path::Component::Normal(name) => {
                    name.to_str().context("invalid replacement name")
                }
                _ => anyhow::bail!("bounded relative replacement path required"),
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        ensure!(
            format!("/{}", parts.join("/")) == record.metadata.path,
            "replacement does not match the bound cloud path"
        );
        self.journal.record_native_edit(cloud_id)?;
        let generation = self
            .journal
            .snapshot_if_owned(id)?
            .map(|state| state.generation);
        let projection = self.journal.native_projection_for_id(id)?;
        if let Some(projection) = projection.as_ref() {
            ensure!(
                projection.root_binding.as_bytes() == owner.receipt().identity.as_slice(),
                "replacement root differs from its publication"
            );
        }
        let identity = projection
            .as_ref()
            .map(|p| p.tag.clone())
            .unwrap_or_else(|| id.as_bytes().to_vec());
        owner
            .with_replacement_capture(relative, &identity, |locked| {
                self.capture_locked_native(locked, &record, generation)
            })
            .map_err(anyhow::Error::msg)
    }

    #[cfg(all(windows, feature = "native-sync"))]
    pub fn create_native_file(
        &self,
        owner: &twodrive_local_files::native::persistent::PersistentSession,
        relative: &Path,
        id: &str,
    ) -> anyhow::Result<()> {
        let parts = relative
            .components()
            .map(|part| match part {
                std::path::Component::Normal(name) => {
                    name.to_str().context("invalid creation name")
                }
                _ => anyhow::bail!("relative creation path required"),
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let cloud_path = format!("/{}", parts.join("/"));
        crate::provider::valid_path(&cloud_path)?;
        let root_binding = serde_json::to_string(owner.receipt())?;
        owner
            .with_replacement_capture_staged(
                relative,
                id.as_bytes(),
                |volume, file_id| {
                    self.journal
                        .begin_native_creation(
                            id,
                            &cloud_path,
                            &root_binding,
                            &format!("{volume:08x}:{file_id:016x}"),
                        )
                        .map_err(|e| e.to_string())
                },
                |locked| {
                    let modified = locked.modified_time()?;
                    let mut snapshot = tempfile::NamedTempFile::new_in(self.journal.cache_root())
                        .map_err(|e| e.to_string())?;
                    locked.copy_resident_to(snapshot.as_file_mut())?;
                    snapshot
                        .as_file()
                        .set_times(std::fs::FileTimes::new().set_modified(modified))
                        .map_err(|e| e.to_string())?;
                    snapshot.as_file().sync_all().map_err(|e| e.to_string())?;
                    let (file, path) = snapshot.keep().map_err(|e| e.to_string())?;
                    drop(file);
                    self.journal
                        .stage_local_creation(id, &cloud_path, &path)
                        .map_err(|e| e.to_string())
                },
            )
            .map_err(anyhow::Error::msg)
    }

    /// A cloud acknowledgement alone cannot mark a subsequently edited native
    /// file synchronized. Rehash both complete snapshots under retained leases.
    #[cfg(all(
        windows,
        any(feature = "native-local-fixture", feature = "native-sync")
    ))]
    pub fn acknowledge_native(&self, path: &Path, id: &str) -> anyhow::Result<bool> {
        let record = self
            .journal
            .database()
            .get_by_remote_id(id)?
            .context("unknown native item")?;
        let cloud_id = record.cloud_remote_id.as_deref().unwrap_or(id);
        let generation = self.status(id)?.generation;
        #[cfg(feature = "native-sync")]
        if let Some(projection) = self.journal.native_projection_for_id(id)? {
            return self.acknowledge_projected_native(path, &record, generation, &projection);
        }
        enum Verification {
            Confirmed,
            CacheChanged,
            NativeChanged,
        }
        let result = self
            .journal
            .with_acknowledged_cache(id, generation, |cache, expected| {
                if digest_file(cache)? != expected {
                    return Ok(Verification::CacheChanged);
                }
                twodrive_local_files::native::with_native_capture(path, id.as_bytes(), |locked| {
                    struct HashSink(Sha256);
                    impl std::io::Write for HashSink {
                        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                            self.0.update(data);
                            Ok(data.len())
                        }
                        fn flush(&mut self) -> std::io::Result<()> {
                            Ok(())
                        }
                    }
                    let mut hash = HashSink(Sha256::new());
                    let size = locked.copy_resident_to(&mut hash)?;
                    let modified = locked
                        .modified_time()?
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(|e| e.to_string())?
                        .as_secs() as i64;
                    if size != record.metadata.size
                        || modified != record.metadata.modified_unix
                        || format!("{:x}", hash.0.finalize()) != expected
                    {
                        return Ok(Verification::NativeChanged);
                    }
                    locked.set_in_sync_after_verification()?;
                    Ok(Verification::Confirmed)
                })
                .map_err(anyhow::Error::msg)
            })?;
        match result {
            Some(Verification::Confirmed) => Ok(true),
            Some(Verification::CacheChanged) => {
                self.journal.record_external_edit(id, generation)?;
                Ok(false)
            }
            Some(Verification::NativeChanged) => {
                self.journal.record_native_edit(cloud_id)?;
                Ok(false)
            }
            None => Ok(false),
        }
    }

    #[cfg(all(windows, feature = "native-sync"))]
    fn acknowledge_projected_native(
        &self,
        path: &Path,
        record: &twodrive_core::FileRecord,
        generation: i64,
        projection: &twodrive_core::NativeProjection,
    ) -> anyhow::Result<bool> {
        use std::cell::Cell;
        let cache_changed = Cell::new(false);
        let native_changed = Cell::new(false);
        let id = &record.metadata.remote_id;
        let confirmed =
            twodrive_local_files::native::with_native_capture(path, &projection.tag, |locked| {
                self.check_native_projection(locked, projection, true)?;
                self.journal
                    .with_native_projection_ack(id, generation, projection, |cache, expected| {
                        if digest_file(cache)? != expected {
                            cache_changed.set(true);
                            return Ok(false);
                        }
                        struct HashSink(Sha256);
                        impl std::io::Write for HashSink {
                            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                                self.0.update(bytes);
                                Ok(bytes.len())
                            }
                            fn flush(&mut self) -> std::io::Result<()> {
                                Ok(())
                            }
                        }
                        let mut hash = HashSink(Sha256::new());
                        let size = locked
                            .copy_resident_to(&mut hash)
                            .map_err(anyhow::Error::msg)?;
                        let modified = locked
                            .modified_time()
                            .map_err(anyhow::Error::msg)?
                            .duration_since(std::time::UNIX_EPOCH)?
                            .as_secs() as i64;
                        if size != record.metadata.size
                            || modified != record.metadata.modified_unix
                            || format!("{:x}", hash.0.finalize()) != expected
                        {
                            native_changed.set(true);
                            return Ok(false);
                        }
                        locked
                            .set_in_sync_after_verification()
                            .map_err(anyhow::Error::msg)?;
                        Ok(true)
                    })
                    .map_err(|e| e.to_string())
            })
            .map_err(anyhow::Error::msg)?;
        if cache_changed.get() {
            self.journal.record_external_edit(id, generation)?;
        }
        if native_changed.get() {
            self.journal
                .record_native_edit(record.cloud_remote_id.as_deref().unwrap_or(id))?;
        }
        Ok(confirmed == Some(true))
    }
    #[cfg(all(windows, feature = "native-sync"))]
    pub fn acknowledge_native_creation(
        &self,
        owner: &twodrive_local_files::native::persistent::PersistentSession,
        relative: &Path,
        id: &str,
    ) -> anyhow::Result<bool> {
        let record = self
            .journal
            .database()
            .get_by_remote_id(id)?
            .context("native creation disappeared")?;
        let relative_cloud = format!(
            "/{}",
            relative
                .components()
                .map(|part| match part {
                    std::path::Component::Normal(name) =>
                        name.to_str().context("invalid creation leaf"),
                    _ => anyhow::bail!("relative creation path required"),
                })
                .collect::<anyhow::Result<Vec<_>>>()?
                .join("/")
        );
        ensure!(
            relative_cloud == record.metadata.path,
            "creation acknowledgement path changed"
        );
        if self.journal.native_projection_for_id(id)?.is_some() {
            return self.acknowledge_native(&owner.root().join(relative), id);
        }
        let generation = self.status(id)?.generation;
        let root = std::str::from_utf8(&owner.receipt().identity)?;
        let receipt = serde_json::to_string(owner.receipt())?;
        let cache_changed = std::cell::Cell::new(false);
        let native_changed = std::cell::Cell::new(false);
        let confirmed = owner
            .with_replacement_capture(relative, id.as_bytes(), |locked| {
                let (volume_serial, file_id) = locked.physical_identity()?;
                self.journal
                    .with_created_projection_ack(
                        id,
                        generation,
                        root,
                        &receipt,
                        twodrive_core::NativeFileIdentity {
                            volume_serial,
                            file_id,
                        },
                        |cache, expected| {
                            if digest_file(cache)? != expected {
                                cache_changed.set(true);
                                return Ok(false);
                            }
                            if !Self::native_bytes_match(locked, &record, expected)
                                .map_err(anyhow::Error::msg)?
                            {
                                native_changed.set(true);
                                return Ok(false);
                            }
                            locked
                                .set_in_sync_after_verification()
                                .map_err(anyhow::Error::msg)?;
                            Ok(true)
                        },
                    )
                    .map_err(|e| e.to_string())
            })
            .map_err(anyhow::Error::msg)?;
        if cache_changed.get() {
            self.journal.record_external_edit(id, generation)?;
        }
        if native_changed.get() {
            self.journal
                .record_native_edit(record.cloud_remote_id.as_deref().unwrap_or(id))?;
        }
        Ok(confirmed == Some(true))
    }
    #[cfg(all(windows, feature = "native-sync"))]
    fn native_bytes_match(
        locked: &twodrive_local_files::native::NativeCapture<'_>,
        record: &twodrive_core::FileRecord,
        expected: &str,
    ) -> Result<bool, String> {
        struct HashSink(Sha256);
        impl std::io::Write for HashSink {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut hash = HashSink(Sha256::new());
        let size = locked.copy_resident_to(&mut hash)?;
        let modified = locked
            .modified_time()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs() as i64;
        Ok(size == record.metadata.size
            && modified == record.metadata.modified_unix
            && format!("{:x}", hash.0.finalize()) == expected)
    }

    /// Download by opaque object identity; recheck provider version after EOF.
    pub fn hydrate(&self, remote_id: &str) -> anyhow::Result<String> {
        self.hydrate_cancellable(remote_id, &std::sync::atomic::AtomicBool::new(false))
    }
    pub fn hydrate_cancellable(
        &self,
        remote_id: &str,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<String> {
        self.hydrate_version(remote_id, None, cancelled)
    }

    fn hydrate_version(
        &self,
        remote_id: &str,
        expected: Option<&twodrive_core::FileRecord>,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<String> {
        use std::sync::atomic::Ordering;
        ensure!(!cancelled.load(Ordering::SeqCst), "hydration cancelled");
        let backend = &self.backend;
        let remote = backend
            .get_metadata(remote_id)?
            .context("provider object missing")?;
        ensure!(
            !remote.is_dir && !remote.etag.is_empty(),
            "versioned file required"
        );
        let db = self.journal.database();
        let (id, generation) = if let Some(expected) = expected {
            ensure!(
                expected.cloud_remote_id.as_deref() == Some(remote.remote_id.as_str())
                    && same_cloud_version(&expected.metadata, &remote),
                "remote differs from published native version"
            );
            (
                expected.metadata.remote_id.clone(),
                self.journal.begin_bound_download(expected)?,
            )
        } else {
            self.journal.begin_download(&core_metadata(&remote))?
        };
        let result = (|| {
            let mut temporary = tempfile::NamedTempFile::new_in(self.journal.cache_root())?;
            let size = backend.download_to(remote_id, temporary.as_file_mut(), &mut |_| {
                ensure!(!cancelled.load(Ordering::SeqCst), "hydration cancelled");
                Ok(())
            })?;
            ensure!(size == remote.size, "incomplete provider download");
            temporary.as_file().sync_all()?;
            let after = backend
                .get_metadata(remote_id)?
                .context("provider object disappeared")?;
            ensure!(
                after.remote_id == remote.remote_id
                    && after.path == remote.path
                    && after.etag == remote.etag
                    && after.size == remote.size,
                "provider version changed during download"
            );
            let digest = digest_file(temporary.as_file_mut())?;
            ensure!(!cancelled.load(Ordering::SeqCst), "hydration cancelled");
            let (file, path) = temporary.keep()?;
            drop(file);
            self.journal
                .publish_download(&id, generation, &path, &digest, &remote.etag)?;
            Ok(id.clone())
        })();
        if result.is_err() {
            db.fail_hydration(&id, generation)?;
        }
        result
    }

    /// Retain one verified private-file lease throughout a native fetch. Its
    /// baseline is supplied by the persisted publication, never fresh discovery.
    pub fn open_published_read(
        &self,
        expected: &twodrive_core::FileRecord,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<PublishedRead> {
        use std::sync::atomic::Ordering;
        ensure!(!cancelled.load(Ordering::SeqCst), "native read cancelled");
        let id = &expected.metadata.remote_id;
        let current = self
            .journal
            .database()
            .get_by_remote_id(id)?
            .context("publication no longer exists")?;
        ensure!(
            same_version(&current.metadata, &expected.metadata)
                && current.cloud_remote_id == expected.cloud_remote_id
                && !expected.metadata.is_dir,
            "native publication version changed"
        );
        if current.cache_path.is_none() {
            self.hydrate_version(
                current
                    .cloud_remote_id
                    .as_deref()
                    .context("unbound publication")?,
                Some(expected),
                cancelled,
            )?;
        }
        let mut file = self.journal.open_read(id)?;
        let state = self.status(id)?;
        let actual = self
            .journal
            .database()
            .get_by_remote_id(id)?
            .context("publication disappeared")?;
        ensure!(
            state.sync_confirmed()
                && same_version(&actual.metadata, &expected.metadata)
                && actual.cloud_remote_id == expected.cloud_remote_id,
            "native source is not acknowledged"
        );
        let digest = file.acknowledged_digest().to_owned();
        if digest_reader_checked(&mut file, &mut || {
            ensure!(
                !cancelled.load(Ordering::SeqCst),
                "native verification cancelled"
            );
            Ok(())
        })? != digest
        {
            drop(file);
            self.journal.record_external_edit(id, state.generation)?;
            anyhow::bail!("private native source changed; preserved for recovery");
        }
        ensure!(!cancelled.load(Ordering::SeqCst), "native read cancelled");
        Ok(PublishedRead {
            file,
            bridge: self.clone(),
            id: id.clone(),
            generation: state.generation,
            size: expected.metadata.size,
        })
    }

    /// Provider's explicit conditional-write capability is mandatory; legacy
    /// CloudBackend default methods alone do not establish safe overwrites.
    pub fn upload(&self, id: &str, progress: Progress) -> anyhow::Result<bool> {
        let provider = &self.backend;
        ensure!(
            provider.capabilities().upload && provider.capabilities().conditional_upload,
            "conditional provider upload unavailable"
        );
        let mut lease = self.journal.begin_upload(id, digest_file)?;
        let record = lease.record().clone();
        let transfer = if let Some(cloud_id) = record.cloud_remote_id.as_deref() {
            provider.upload_snapshot(
                &record.metadata.path,
                record.cache_path.as_deref().context("missing cache")?,
                cloud_id,
                &record.metadata.etag,
                progress,
            )
        } else {
            ensure!(
                record.metadata.etag.is_empty(),
                "unbound creation has an invalid version"
            );
            provider.create_snapshot(
                &record.metadata.path,
                record
                    .cache_path
                    .as_deref()
                    .context("missing creation snapshot")?,
                progress,
            )
        };
        match transfer {
            Ok(remote) => {
                let digest = digest_file(lease.file())?;
                lease.acknowledge(&core_metadata(&remote), &digest)
            }
            Err(error) => {
                if error.is::<crate::provider::TransferNotStarted>() {
                    lease.reject_unstarted(digest_file)?;
                } else if provider.is_conflict_error(&error) {
                    lease.reject_conflict()?;
                }
                // All other errors can be an accepted write with a lost reply.
                // Drop retains the durable intent; no blind retry or release.
                Err(error)
            }
        }
    }

    pub fn reconcile(&self, id: &str) -> anyhow::Result<bool> {
        let backend = &self.backend;
        let record = self
            .journal
            .database()
            .get_by_remote_id(id)?
            .context("unknown local item")?;
        let before = match record.cloud_remote_id.as_deref() {
            Some(remote_id) => backend.get_metadata(remote_id)?,
            None => backend.get_metadata_by_path(&record.metadata.path)?,
        }
        .context("provider object missing; creation remains uncertain")?;
        let remote_id = before.remote_id.as_str();
        ensure!(
            !before.is_dir && !before.etag.is_empty(),
            "versioned provider file required"
        );
        let mut download = tempfile::tempfile_in(self.journal.cache_root())?;
        let size = backend.download_to(remote_id, &mut download, &mut |_| Ok(()))?;
        ensure!(size == before.size, "incomplete reconciliation download");
        let after = backend
            .get_metadata(remote_id)?
            .context("provider object disappeared")?;
        ensure!(
            after.remote_id == before.remote_id
                && !after.is_dir
                && after.path == before.path
                && after.etag == before.etag
                && after.size == before.size,
            "provider version changed during reconciliation"
        );
        let remote_digest = digest_file(&mut download)?;
        self.journal
            .reconcile_upload(id, &core_metadata(&after), &remote_digest, digest_file)
    }

    /// Diagnostic engine eligibility; this never dehydrates or removes a file.
    /// The native adapter still reports release unsupported and requires fresh
    /// CFAPI evidence under its own exclusive oplock before any future action.
    pub fn engine_release_ready(&self, id: &str, generation: i64) -> anyhow::Result<bool> {
        let verified = self
            .journal
            .with_clean_cache(id, generation, |file, expected| {
                Ok(digest_file(file)? == expected)
            })?;
        if verified == Some(false) {
            self.journal.record_external_edit(id, generation)?;
        }
        Ok(verified.unwrap_or(false))
    }
}

pub struct PublishedRead {
    file: twodrive_core::CacheReadLease,
    bridge: LocalFileBridge,
    id: String,
    generation: i64,
    size: u64,
}
impl PublishedRead {
    pub fn read_range(
        &mut self,
        offset: usize,
        length: usize,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<Vec<u8>> {
        use std::sync::atomic::Ordering;
        ensure!(
            !cancelled.load(Ordering::SeqCst)
                && length <= 65536
                && (offset as u64)
                    .checked_add(length as u64)
                    .is_some_and(|end| end <= self.size),
            "bounded uncancelled native range required"
        );
        let state = self.bridge.status(&self.id)?;
        ensure!(
            state.sync_confirmed() && state.generation == self.generation,
            "native read version invalidated"
        );
        self.file.seek(SeekFrom::Start(offset as u64))?;
        let mut bytes = vec![0; length];
        self.file.read_exact(&mut bytes)?;
        ensure!(!cancelled.load(Ordering::SeqCst), "native read cancelled");
        Ok(bytes)
    }
}

fn same_version(a: &MetadataEntry, b: &MetadataEntry) -> bool {
    same_cloud_version(a, b) && a.modified_unix == b.modified_unix
}
fn same_cloud_version(a: &MetadataEntry, b: &MetadataEntry) -> bool {
    a.path == b.path && a.etag == b.etag && a.size == b.size && a.is_dir == b.is_dir
}

pub(crate) fn digest_reader(reader: &mut impl Read) -> anyhow::Result<String> {
    digest_reader_checked(reader, &mut || Ok(()))
}
fn digest_reader_checked(
    reader: &mut impl Read,
    check: &mut dyn FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        check()?;
        let size = reader.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
pub fn digest_file(file: &mut File) -> anyhow::Result<String> {
    file.seek(SeekFrom::Start(0))?;
    digest_reader(file)
}
fn core_metadata(entry: &MetadataEntry) -> twodrive_core::MetadataEntry {
    twodrive_core::MetadataEntry {
        remote_id: entry.remote_id.clone(),
        path: entry.path.clone(),
        parent_path: entry.parent_path.clone(),
        name: entry.name.clone(),
        is_dir: entry.is_dir,
        size: entry.size,
        modified_unix: entry.modified_unix,
        etag: entry.etag.clone(),
    }
}

/// Used by deterministic mock fixtures; not connected to desktop IPC.
pub fn no_progress() -> Progress {
    Arc::new(|_, _| Ok(()))
}

#[cfg(test)]
#[path = "local_files_tests.rs"]
pub(crate) mod tests;
