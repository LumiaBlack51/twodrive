//! Provider-independent desktop storage contract. Paths select entries; opaque
//! remote IDs select objects. A provider must advertise conditional writes before
//! uploads/backups are offered; CloudBackend's legacy defaults do not promise them.
use crate::model::{CloudBackend, DeltaResult, MetadataEntry};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

pub type Progress = Arc<dyn Fn(u64, u64) -> anyhow::Result<()> + Send + Sync>;

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct Capabilities {
    pub upload: bool,
    pub mkdir: bool,
    pub rename: bool,
    pub delete: bool,
    pub conditional_upload: bool,
}
impl Capabilities {
    pub fn backup(self) -> bool {
        self.upload && self.mkdir && self.conditional_upload
    }
}

pub trait Provider: CloudBackend {
    /// Optional protocol process control, used by bounded desktop operations.
    fn set_operation_cancel(&self, _cancel: Arc<std::sync::atomic::AtomicBool>) {}
    /// Adapters without a persistent version/cache contract may forbid mounts.
    fn supports_mount(&self) -> bool {
        true
    }
    fn capabilities(&self) -> Capabilities;
    fn list_directory(&self, path: &str) -> anyhow::Result<Vec<MetadataEntry>>;
    /// No version means atomic create-only; a version requires atomic comparison.
    /// remote_id is an opaque provider ID, never a UI path. Implementations must
    /// stream with bounded memory and stop when the progress callback fails.
    fn upload_controlled(
        &self,
        path: &str,
        source: &Path,
        remote_id: Option<&str>,
        version: Option<&str>,
        progress: Progress,
    ) -> anyhow::Result<MetadataEntry>;
}

#[derive(Clone)]
pub struct Backend {
    inner: Arc<dyn Provider>,
    paused: Option<PathBuf>,
}
impl Backend {
    pub fn new(provider: impl Provider) -> Self {
        Self {
            inner: Arc::new(provider),
            paused: None,
        }
    }
    pub fn with_pause_file(mut self, path: PathBuf) -> Self {
        self.paused = Some(path);
        self
    }
    fn check_pause(&self) -> anyhow::Result<()> {
        ensure!(
            !self.paused.as_ref().is_some_and(|p| p.exists()),
            "同步已暂停；待传输数据保留在本机"
        );
        Ok(())
    }
    pub fn supports_mount(&self) -> bool {
        self.inner.supports_mount()
    }
    pub fn set_operation_cancel(&self, cancel: Arc<std::sync::atomic::AtomicBool>) {
        self.inner.set_operation_cancel(cancel);
    }
    pub fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    pub fn list_directory(&self, path: &str) -> anyhow::Result<Vec<MetadataEntry>> {
        self.check_pause()?;
        valid_path(path)?;
        self.inner.list_directory(path)
    }
    pub fn entry(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        valid_path(path)?;
        self.get_metadata_by_path(path)?
            .context("远端文件不存在，请刷新后重试")
    }
    pub fn download_path_to(
        &self,
        path: &str,
        writer: &mut dyn Write,
        progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        let entry = self.entry(path)?;
        ensure!(!entry.is_dir, "不能下载目录");
        self.download_to(&entry.remote_id, writer, progress)
    }
    pub fn rename_path(&self, path: &str, to: &str) -> anyhow::Result<MetadataEntry> {
        valid_path(to)?;
        ensure!(path != "/", "不能重命名共享根目录");
        self.rename(&self.entry(path)?.remote_id, to)
    }
    pub fn delete_path(&self, path: &str) -> anyhow::Result<()> {
        ensure!(path != "/", "不能删除共享根目录");
        self.delete(&self.entry(path)?.remote_id)
    }
    pub fn upload_path(
        &self,
        path: &str,
        source: &Path,
        version: Option<&str>,
        progress: Progress,
    ) -> anyhow::Result<MetadataEntry> {
        valid_path(path)?;
        self.check_pause()?;
        let caps = self.capabilities();
        ensure!(
            caps.upload && caps.conditional_upload,
            "此连接不支持安全的条件上传"
        );
        let entry = self.get_metadata_by_path(path)?;
        if let Some(entry) = &entry {
            ensure!(!entry.is_dir, "上传目标是目录");
            ensure!(
                version.is_some_and(|v| !v.is_empty() && v == entry.etag),
                "远端版本发生冲突，未覆盖；请刷新后重试"
            );
        } else {
            ensure!(version.is_none(), "远端文件已移除，未重新创建");
        }
        let paused = self.paused.clone();
        self.inner.upload_controlled(
            path,
            source,
            entry.as_ref().map(|e| e.remote_id.as_str()),
            version,
            Arc::new(move |done, total| {
                ensure!(!paused.as_ref().is_some_and(|p| p.exists()), "同步已暂停");
                progress(done, total)
            }),
        )
    }
}

impl Backend {
    /// A journal claim owns an opaque object/version snapshot. Do not select a
    /// potentially replaced object by path again at transfer time.
    pub fn upload_snapshot(
        &self,
        path: &str,
        source: &Path,
        id: &str,
        version: &str,
        progress: Progress,
    ) -> anyhow::Result<MetadataEntry> {
        valid_path(path)?;
        self.check_pause()?;
        let caps = self.capabilities();
        ensure!(
            caps.upload && caps.conditional_upload && !id.is_empty() && !version.is_empty(),
            "versioned conditional upload unavailable"
        );
        let paused = self.paused.clone();
        self.inner.upload_controlled(
            path,
            source,
            Some(id),
            Some(version),
            Arc::new(move |done, total| {
                ensure!(
                    !paused.as_ref().is_some_and(|path| path.exists()),
                    "synchronization paused"
                );
                progress(done, total)
            }),
        )
    }
}

pub fn valid_path(path: &str) -> anyhow::Result<()> {
    ensure!(
        path.starts_with('/')
            && path.trim() == path
            && !path.contains(['\\', '\0'])
            && (path == "/"
                || path[1..]
                    .split('/')
                    .all(|p| !p.is_empty() && p != "." && p != "..")),
        "invalid resource path"
    );
    Ok(())
}

impl CloudBackend for Backend {
    fn is_conflict_error(&self, e: &anyhow::Error) -> bool {
        self.inner.is_conflict_error(e)
    }
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        self.check_pause()?;
        self.inner.list_all()
    }
    fn list_delta(&self, cursor: Option<&str>) -> anyhow::Result<DeltaResult> {
        self.check_pause()?;
        self.inner.list_delta(cursor)
    }
    fn get_metadata(&self, id: &str) -> anyhow::Result<Option<MetadataEntry>> {
        self.check_pause()?;
        self.inner.get_metadata(id)
    }
    fn get_metadata_by_path(&self, path: &str) -> anyhow::Result<Option<MetadataEntry>> {
        self.check_pause()?;
        valid_path(path)?;
        self.inner.get_metadata_by_path(path)
    }
    fn download(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        self.check_pause()?;
        self.inner.download(id)
    }
    fn download_to(
        &self,
        id: &str,
        writer: &mut dyn Write,
        progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        self.check_pause()?;
        self.inner.download_to(id, writer, &mut |done| {
            self.check_pause()?;
            progress(done)
        })
    }
    #[cfg(target_os = "linux")]
    fn download_sized_to(
        &self,
        id: &str,
        size: u64,
        writer: &mut dyn Write,
        progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        self.check_pause()?;
        self.inner.download_sized_to(id, size, writer, &mut |done| {
            self.check_pause()?;
            progress(done)
        })
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
        self.check_pause()?;
        ensure!(
            self.capabilities().upload && self.capabilities().conditional_upload,
            "此连接不支持安全的条件上传"
        );
        self.inner.upload_with_etag(path, bytes, version)
    }
    fn upload_file_with_version(
        &self,
        path: &str,
        source: &Path,
        id: Option<&str>,
        version: Option<&str>,
        progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        self.check_pause()?;
        ensure!(
            self.capabilities().upload && self.capabilities().conditional_upload,
            "此连接不支持安全的条件上传"
        );
        self.inner
            .upload_file_with_version(path, source, id, version, &mut |done, total| {
                self.check_pause()?;
                progress(done, total)
            })
    }
    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        self.check_pause()?;
        valid_path(path)?;
        ensure!(self.capabilities().mkdir, "此连接不支持创建目录");
        self.inner.create_folder(path)
    }
    fn rename(&self, id: &str, path: &str) -> anyhow::Result<MetadataEntry> {
        self.check_pause()?;
        valid_path(path)?;
        ensure!(self.capabilities().rename, "此连接不支持重命名");
        self.inner.rename(id, path)
    }
    fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.check_pause()?;
        ensure!(self.capabilities().delete, "此连接不支持删除");
        self.inner.delete(id)
    }
}
