//! Shared-engine private-cache bridge. No CFAPI, GUI, provider switch or root
//! registration. Native capabilities stay disabled until the adapter owns the
//! corresponding OS locks and recovery path.
use crate::model::{CloudBackend, MetadataEntry};
use crate::provider::{Backend, Progress};
use anyhow::{Context, ensure};
use sha2::{Digest, Sha256};
#[cfg(any(test, all(windows, feature = "native-local-fixture")))]
use std::path::Path;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};
#[cfg(test)]
use twodrive_core::Database;
use twodrive_core::{CacheJournal, CacheSnapshot, CacheWriteLease};

#[derive(Clone)]
pub struct LocalFileBridge {
    journal: CacheJournal,
    backend: Backend,
}
impl LocalFileBridge {
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
    // Real-provider construction is not exposed until the registered adapter's
    // verified profile/revision identity is passed as a typed session authority.
    #[cfg(test)]
    pub(crate) fn open(
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
    pub fn status(&self, id: &str) -> anyhow::Result<CacheSnapshot> {
        self.journal.snapshot(id)
    }
    pub fn edit(&self, id: &str) -> anyhow::Result<CacheWriteLease> {
        self.journal.open_write(id)
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
        let (id, generation) = self.journal.begin_download(&core_metadata(&remote))?;
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
        match provider.upload_snapshot(
            &record.metadata.path,
            record.cache_path.as_deref().context("missing cache")?,
            record
                .cloud_remote_id
                .as_deref()
                .context("unbound cache object")?,
            &record.metadata.etag,
            progress,
        ) {
            Ok(remote) => {
                let digest = digest_file(lease.file())?;
                lease.acknowledge(&core_metadata(&remote), &digest)
            }
            Err(error) => {
                if provider.is_conflict_error(&error) {
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
        let remote_id = record
            .cloud_remote_id
            .as_deref()
            .context("unbound local item")?;
        let before = backend
            .get_metadata(remote_id)?
            .context("provider object missing")?;
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

pub(crate) fn digest_reader(reader: &mut impl Read) -> anyhow::Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
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
        created_unix: entry.created_unix,
        etag: entry.etag.clone(),
    }
}

/// Used by deterministic mock fixtures; not connected to desktop IPC.
pub fn no_progress() -> Progress {
    Arc::new(|_, _| Ok(()))
}

#[cfg(test)]
#[path = "local_files_tests.rs"]
mod tests;
