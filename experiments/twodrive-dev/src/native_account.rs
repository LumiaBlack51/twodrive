//! Deferred, identity-pinned native backend. A sealed coordinator configuration
//! may reopen its own cache offline; every cloud operation verifies authority.
use crate::{
    model::{CloudBackend, DeltaResult, MetadataEntry},
    onedrive::OneDrive,
    provider::{Capabilities, Progress, Provider, TransferNotStarted},
};
use anyhow::{Context, ensure};
use std::{
    io::Write,
    path::{Path, PathBuf},
};
use twodrive_core::CloudIdentity;

pub(crate) struct NativeAccount {
    account: PathBuf,
    expected: CloudIdentity,
    writable: bool,
}
impl NativeAccount {
    pub(crate) fn new(
        account: PathBuf,
        expected: CloudIdentity,
        writable: bool,
    ) -> anyhow::Result<Self> {
        ensure!(
            account.is_absolute() && !expected.account.is_empty() && !expected.drive.is_empty(),
            "native account authority missing"
        );
        Ok(Self {
            account,
            expected,
            writable,
        })
    }
    pub(crate) fn verified(&self) -> anyhow::Result<OneDrive> {
        let provider = OneDrive::open(&self.account, self.writable)?;
        ensure!(
            provider.graph.readonly_identity(&mut || Ok(()))? == self.expected,
            "native account or drive changed"
        );
        Ok(provider)
    }
}
impl Provider for NativeAccount {
    fn create_directory_bound(
        &self,
        parent: &MetadataEntry,
        path: &str,
    ) -> anyhow::Result<MetadataEntry> {
        ensure!(self.writable, "native account is read-only");
        self.verified()?.graph.create_directory_bound(
            &self.expected,
            &parent.remote_id,
            path,
            &mut || Ok(()),
        )
    }
    fn move_bound(
        &self,
        entry: &MetadataEntry,
        parent: &MetadataEntry,
        path: &str,
    ) -> anyhow::Result<MetadataEntry> {
        ensure!(self.writable, "native account is read-only");
        self.verified()?.graph.move_item_bound(
            &self.expected,
            &entry.remote_id,
            &entry.etag,
            &parent.remote_id,
            path,
            &mut || Ok(()),
        )
    }
    fn delete_bound(&self, entry: &MetadataEntry) -> anyhow::Result<()> {
        ensure!(self.writable, "native account is read-only");
        self.verified()?.graph.delete_item_bound(
            &self.expected,
            &entry.remote_id,
            &entry.etag,
            &mut || Ok(()),
        )
    }
    fn supports_mount(&self) -> bool {
        false
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            upload: self.writable,
            conditional_upload: self.writable,
            ..Default::default()
        }
    }
    fn list_directory(&self, path: &str) -> anyhow::Result<Vec<MetadataEntry>> {
        self.verified()?.list_directory(path)
    }
    fn upload_controlled(
        &self,
        path: &str,
        source: &Path,
        id: Option<&str>,
        version: Option<&str>,
        progress: Progress,
    ) -> anyhow::Result<MetadataEntry> {
        // Only read-only authentication/identity I/O precedes this marker.
        // Once the provider starts a data request, errors remain ambiguous.
        ensure!(self.writable, "native account is read-only");
        let provider = self.verified().context(TransferNotStarted)?;
        provider.upload_controlled(path, source, id, version, progress)
    }
}
impl CloudBackend for NativeAccount {
    fn is_conflict_error(&self, error: &anyhow::Error) -> bool {
        error.to_string().contains("HTTP 412") || error.to_string().contains("Precondition Failed")
    }
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        self.verified()?.list_all()
    }
    fn list_delta(&self, cursor: Option<&str>) -> anyhow::Result<DeltaResult> {
        self.verified()?.list_delta(cursor)
    }
    fn get_metadata(&self, id: &str) -> anyhow::Result<Option<MetadataEntry>> {
        self.verified()?.get_metadata(id)
    }
    fn get_metadata_by_path(&self, path: &str) -> anyhow::Result<Option<MetadataEntry>> {
        self.verified()?.get_metadata_by_path(path)
    }
    fn download(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        self.verified()?.download(id)
    }
    fn download_to(
        &self,
        id: &str,
        writer: &mut dyn Write,
        progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        self.verified()?.download_to(id, writer, progress)
    }
    fn download_sized_to(
        &self,
        id: &str,
        size: u64,
        writer: &mut dyn Write,
        progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        self.verified()?
            .download_sized_to(id, size, writer, progress)
    }
    fn upload(&self, _: &str, _: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        anyhow::bail!("native uploads require an immutable snapshot")
    }
    fn create_folder(&self, _: &str) -> anyhow::Result<MetadataEntry> {
        anyhow::bail!("native directories require the versioned namespace coordinator")
    }
    fn rename(&self, _: &str, _: &str) -> anyhow::Result<MetadataEntry> {
        anyhow::bail!("native rename requires a versioned namespace intent")
    }
    fn delete(&self, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("native deletion requires a versioned namespace intent")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        local_files::{LocalFileBridge, no_progress},
        provider::Backend,
    };
    use sha2::{Digest, Sha256};
    #[test]
    fn missing_credentials_keep_cached_reads_and_retryable_creation() {
        let temp = tempfile::tempdir().unwrap();
        let private = temp.path().canonicalize().unwrap();
        let cache = private.join("cache");
        std::fs::create_dir(&cache).unwrap();
        let db = twodrive_core::Database::new(private.join("native.sqlite3"));
        db.init().unwrap();
        let metadata = MetadataEntry::new_file("cloud-id", "/generated.txt", 8, 1, "etag-1");
        db.upsert_metadata(&metadata).unwrap();
        let provider = NativeAccount::new(
            private.join("missing-credentials"),
            CloudIdentity {
                account: "generated-owner".into(),
                drive: "generated-drive".into(),
            },
            true,
        )
        .unwrap();
        let bridge = LocalFileBridge::open_bound(
            db.clone(),
            &cache,
            Backend::new(provider),
            "onedrive:generated-owner:generated-drive",
        )
        .unwrap();
        let source = cache.join("acknowledged.bin");
        std::fs::write(&source, b"cached!!").unwrap();
        bridge
            .journal()
            .publish_download(
                "cloud-id",
                0,
                &source,
                &format!("{:x}", Sha256::digest(b"cached!!")),
                "etag-1",
            )
            .unwrap();
        let expected = db.get_by_remote_id("cloud-id").unwrap().unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let mut read = bridge.open_published_read(&expected, &cancel).unwrap();
        assert_eq!(read.read_range(0, 8, &cancel).unwrap(), b"cached!!");
        drop(read);
        let local = cache.join("new.bin");
        std::fs::write(&local, b"new generated bytes").unwrap();
        let id = "local-upload-missing-credentials";
        bridge.stage_creation(id, "/new.txt", &local).unwrap();
        assert!(bridge.upload(id, no_progress()).is_err());
        let state = bridge.status(id).unwrap();
        assert_eq!(state.state, twodrive_core::FileState::Dirty);
        assert!(state.upload_generation.is_none() && !state.uncertain_upload);
        assert_eq!(std::fs::read(local).unwrap(), b"new generated bytes");
        drop(bridge);
    }
}
