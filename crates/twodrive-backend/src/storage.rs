use std::{fs, io::Write, path::Path};
use twodrive_core::{MetadataEntry, normalize_cloud_path};
pub trait CloudBackend: Send + Sync + 'static {
    /// Classify version conflicts for recovery. The default retains historical
    /// OneDrive error compatibility for existing backend implementations;
    /// providers with different error types should override this hook.
    fn is_conflict_error(&self, error: &anyhow::Error) -> bool {
        crate::onedrive::is_conflict_error(error)
    }

    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>>;
    fn get_metadata(&self, remote_id: &str) -> anyhow::Result<Option<MetadataEntry>> {
        Ok(self
            .list_all()?
            .into_iter()
            .find(|entry| entry.remote_id == remote_id))
    }
    fn get_metadata_by_path(&self, path: &str) -> anyhow::Result<Option<MetadataEntry>> {
        let path = normalize_cloud_path(path);
        Ok(self
            .list_all()?
            .into_iter()
            .find(|entry| entry.path == path))
    }
    fn list_delta(&self, _delta_link: Option<&str>) -> anyhow::Result<DeltaResult> {
        Ok(DeltaResult {
            entries: self.list_all()?,
            deleted_remote_ids: Vec::new(),
            delta_link: None,
        })
    }
    fn download(&self, remote_id: &str) -> anyhow::Result<Vec<u8>>;
    fn download_to(
        &self,
        remote_id: &str,
        writer: &mut dyn Write,
        on_progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        let content = self.download(remote_id)?;
        let bytes_done = content.len() as u64;
        writer.write_all(&content)?;
        on_progress(bytes_done)?;
        Ok(bytes_done)
    }
    fn download_sized_to(
        &self,
        remote_id: &str,
        _size: u64,
        writer: &mut dyn Write,
        on_progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        self.download_to(remote_id, writer, on_progress)
    }
    fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry>;
    fn upload_with_etag(
        &self,
        path: &str,
        content: Vec<u8>,
        if_match: Option<&str>,
    ) -> anyhow::Result<MetadataEntry> {
        let _ = if_match;
        self.upload(path, content)
    }
    fn upload_file_with_version(
        &self,
        path: &str,
        source_path: &Path,
        remote_id: Option<&str>,
        if_match: Option<&str>,
        on_progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        let _ = remote_id;
        let content = fs::read(source_path)?;
        let size = content.len() as u64;
        let uploaded = self.upload_with_etag(path, content, if_match)?;
        on_progress(size, size)?;
        Ok(uploaded)
    }
    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry>;
    fn rename(&self, remote_id: &str, new_path: &str) -> anyhow::Result<MetadataEntry>;
    fn delete(&self, remote_id: &str) -> anyhow::Result<()>;
}

#[derive(Debug, Clone, Default)]
pub struct DeltaResult {
    pub entries: Vec<MetadataEntry>,
    pub deleted_remote_ids: Vec<String>,
    pub delta_link: Option<String>,
}
