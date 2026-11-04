//! Adapter around the existing Graph engine. Desktop account paths are explicit
//! and independent of the installed stable client's XDG configuration.
use crate::{
    model::{CloudBackend, DeltaResult, MetadataEntry},
    provider::{Capabilities, Progress, Provider},
    state::State,
};
use anyhow::ensure;
use std::{io::Write, path::Path, sync::Arc};
use twodrive_backend::{CloudBackend as GraphStorage, GraphBackend};
use twodrive_core::{AppPaths, TokenStore};

pub fn paths(directory: &Path) -> anyhow::Result<AppPaths> {
    let state = State::new(directory.to_path_buf())?;
    Ok(AppPaths {
        config_dir: state.dir.clone(),
        config_path: state.dir.join("config.toml"),
        data_dir: state.dir.clone(),
        cache_dir: state.dir.join("cache"),
        db_path: state.dir.join("metadata.sqlite3"),
        mount_dir: state.dir.join("internal-mount"),
        token_path: state.dir.join("tokens.json"),
    })
}
fn entry(m: twodrive_core::MetadataEntry) -> MetadataEntry {
    MetadataEntry {
        remote_id: m.remote_id,
        path: m.path,
        parent_path: m.parent_path,
        name: m.name,
        is_dir: m.is_dir,
        size: m.size,
        modified_unix: m.modified_unix,
        created_unix: m.created_unix,
        etag: m.etag,
    }
}
#[derive(Clone)]
pub struct OneDrive {
    pub graph: Arc<GraphBackend>,
    writable: bool,
}
impl OneDrive {
    pub fn open(directory: &Path, writable: bool) -> anyhow::Result<Self> {
        Ok(Self {
            graph: Arc::new(GraphBackend::from_paths(&paths(directory)?)?),
            writable,
        })
    }
    pub fn logout(directory: &Path) -> anyhow::Result<()> {
        TokenStore::new(paths(directory)?.token_path).delete()
    }
    fn check_write(&self) -> anyhow::Result<()> {
        ensure!(self.writable, "OneDrive 连接为只读");
        Ok(())
    }
}
impl Provider for OneDrive {
    fn storage_usage(
        &self,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Option<crate::provider::StorageUsage>> {
        let quota = self.graph.readonly_quota(check)?;
        Ok(crate::provider::StorageUsage::account(
            quota["total"].as_u64(),
            quota["used"].as_u64(),
            quota["remaining"].as_u64(),
        ))
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            upload: self.writable,
            mkdir: self.writable,
            rename: self.writable,
            delete: self.writable,
            conditional_upload: self.writable,
        }
    }
    fn list_directory(&self, path: &str) -> anyhow::Result<Vec<MetadataEntry>> {
        self.list_directory_checked(path, &mut || Ok(()))
    }
    fn list_directory_checked(
        &self,
        path: &str,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Vec<MetadataEntry>> {
        Ok(self
            .graph
            .desktop_directory(path, check)?
            .into_iter()
            .map(entry)
            .collect())
    }
    fn upload_controlled(
        &self,
        path: &str,
        source: &Path,
        id: Option<&str>,
        version: Option<&str>,
        progress: Progress,
    ) -> anyhow::Result<MetadataEntry> {
        self.upload_file_with_version(path, source, id, version, &mut |d, t| progress(d, t))
    }
}
impl CloudBackend for OneDrive {
    fn is_conflict_error(&self, e: &anyhow::Error) -> bool {
        GraphStorage::is_conflict_error(self.graph.as_ref(), e)
    }
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        Ok(self.graph.list_all()?.into_iter().map(entry).collect())
    }
    fn list_delta(&self, cursor: Option<&str>) -> anyhow::Result<DeltaResult> {
        let d = self.graph.list_delta(cursor)?;
        Ok(DeltaResult {
            entries: d.entries.into_iter().map(entry).collect(),
            deleted_remote_ids: d.deleted_remote_ids,
            delta_link: d.delta_link,
        })
    }
    fn get_metadata(&self, id: &str) -> anyhow::Result<Option<MetadataEntry>> {
        Ok(self.graph.get_metadata(id)?.map(entry))
    }
    fn get_metadata_by_path(&self, path: &str) -> anyhow::Result<Option<MetadataEntry>> {
        Ok(self.graph.get_metadata_by_path(path)?.map(entry))
    }
    fn download(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        self.graph.download(id)
    }
    fn download_to(
        &self,
        id: &str,
        writer: &mut dyn Write,
        progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        self.graph.download_to(id, writer, progress)
    }
    #[cfg(target_os = "linux")]
    fn download_sized_to(
        &self,
        id: &str,
        size: u64,
        writer: &mut dyn Write,
        progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        self.graph.download_sized_to(id, size, writer, progress)
    }
    fn upload(&self, path: &str, bytes: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        self.upload_with_etag(path, bytes, None)
    }
    fn upload_with_etag(
        &self,
        path: &str,
        bytes: Vec<u8>,
        version: Option<&str>,
    ) -> anyhow::Result<MetadataEntry> {
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(&bytes)?;
        let id = self.get_metadata_by_path(path)?.map(|m| m.remote_id);
        self.upload_file_with_version(
            path,
            file.path(),
            id.as_deref(),
            version,
            &mut |_, _| Ok(()),
        )
    }
    fn upload_file_with_version(
        &self,
        path: &str,
        source: &Path,
        id: Option<&str>,
        version: Option<&str>,
        progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        self.check_write()?;
        Ok(entry(self.graph.upload_desktop_file(
            path, source, id, version, progress,
        )?))
    }
    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        self.check_write()?;
        Ok(entry(self.graph.create_folder(path)?))
    }
    fn rename(&self, id: &str, path: &str) -> anyhow::Result<MetadataEntry> {
        self.check_write()?;
        Ok(entry(self.graph.rename(id, path)?))
    }
    fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.check_write()?;
        self.graph.delete(id)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_account_paths_never_use_stable_configuration() {
        let temp = tempfile::tempdir().unwrap();
        let p = paths(&temp.path().join("onedrive")).unwrap();
        // State resolves the directory; Windows adds its verbatim path prefix.
        let account = temp.path().join("onedrive").canonicalize().unwrap();
        assert_eq!(p.config_path, account.join("config.toml"));
        assert_eq!(p.token_path, account.join("tokens.json"));
        assert_eq!(p.data_dir, p.config_dir);
    }
}
