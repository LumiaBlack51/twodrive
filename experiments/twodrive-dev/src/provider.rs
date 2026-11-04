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
    time::Instant,
};

pub type Progress = Arc<dyn Fn(u64, u64) -> anyhow::Result<()> + Send + Sync>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageUsage {
    pub total: Option<u64>,
    pub used: Option<u64>,
    pub free: Option<u64>,
    /// Account quota can include other services; it is not a folder-size sum.
    pub scope: String,
}
impl StorageUsage {
    pub fn account(total: Option<u64>, used: Option<u64>, free: Option<u64>) -> Option<Self> {
        if total.is_none() && used.is_none() && free.is_none() {
            return None;
        }
        Some(Self {
            total,
            used,
            free: free.or_else(|| total.zip(used).map(|(t, u)| t.saturating_sub(u))),
            scope: "account".into(),
        })
    }
}

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
    fn capabilities(&self) -> Capabilities;
    fn storage_usage(
        &self,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Option<StorageUsage>> {
        check()?;
        Ok(None)
    }
    /// Optional complete root listing already validated by this connection.
    /// Only a mount display seed: never evidence for file contents or writes.
    fn initial_directory_listing(&self) -> Option<(Instant, Vec<MetadataEntry>)> {
        None
    }
    fn list_directory(&self, path: &str) -> anyhow::Result<Vec<MetadataEntry>>;
    fn list_directory_checked(
        &self,
        path: &str,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Vec<MetadataEntry>> {
        check()?;
        let entries = self.list_directory(path)?;
        check()?;
        Ok(entries)
    }
    /// No version means atomic create-only; a version requires atomic comparison.
    /// remote_id is an opaque provider ID, never a UI path. Implementations must
    /// stream with bounded memory and stop when the progress callback fails.
    /// Explicit user writes with recovery, without claiming an atomic CAS.
    fn manual_upload_supported(&self) -> bool {
        false
    }
    fn manual_upload(
        &self,
        _path: &str,
        _source: &Path,
        _id: Option<&str>,
        _version: Option<&str>,
        _progress: Progress,
    ) -> anyhow::Result<MetadataEntry> {
        anyhow::bail!("此网盘未提供手动写入")
    }
    /// Check the observed source and create-only destination using fresh data.
    /// Adapters may coalesce lookups, but must not use display caches or weaken
    /// version/type checks. Manual providers do not gain atomic CAS semantics.
    fn rename_snapshot(
        &self,
        path: &str,
        to: &str,
        id: &str,
        version: &str,
        directory: bool,
    ) -> anyhow::Result<MetadataEntry> {
        let current = self.get_metadata_by_path(path)?.context("源文件不存在")?;
        ensure!(
            current.remote_id == id && current.etag == version && current.is_dir == directory,
            "云端版本已改变，未重命名"
        );
        ensure!(
            self.get_metadata_by_path(to)?.is_none(),
            "目标已存在，拒绝覆盖"
        );
        self.rename(id, to)
    }
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
    writable: bool,
    write_guard: Option<(PathBuf, String)>,
}
impl Backend {
    pub fn new(provider: impl Provider) -> Self {
        Self {
            inner: Arc::new(provider),
            paused: None,
            writable: true,
            write_guard: None,
        }
    }
    pub fn with_pause_file(mut self, path: PathBuf) -> Self {
        self.paused = Some(path);
        self
    }
    pub fn with_write_access(mut self, writable: bool, guard: Option<(PathBuf, String)>) -> Self {
        self.writable = writable;
        self.write_guard = guard;
        self
    }
    fn check_write(&self) -> anyhow::Result<()> {
        ensure!(self.writable, "此连接未获写入权限");
        ensure!(
            self.write_guard.as_ref().is_none_or(|(path, token)| {
                crate::provider_guard::write_permit_allowed(path, token)
            }),
            "写入已锁定，请在客户端解锁"
        );
        self.check_pause()
    }
    fn check_pause(&self) -> anyhow::Result<()> {
        ensure!(
            !self.paused.as_ref().is_some_and(|p| p.exists()),
            "同步已暂停；待传输数据保留在本机"
        );
        Ok(())
    }
    pub fn capabilities(&self) -> Capabilities {
        if self.writable
            && self.write_guard.as_ref().is_none_or(|(path, token)| {
                crate::provider_guard::write_permit_allowed(path, token)
            })
        {
            self.inner.capabilities()
        } else {
            Capabilities::default()
        }
    }
    pub fn storage_usage(
        &self,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Option<StorageUsage>> {
        self.inner.storage_usage(check)
    }
    pub fn initial_directory_listing(&self) -> Option<(Instant, Vec<MetadataEntry>)> {
        self.inner.initial_directory_listing()
    }
    pub fn list_directory(&self, path: &str) -> anyhow::Result<Vec<MetadataEntry>> {
        self.check_pause()?;
        valid_path(path)?;
        self.inner.list_directory(path)
    }
    pub fn list_directory_checked(
        &self,
        path: &str,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Vec<MetadataEntry>> {
        self.check_pause()?;
        valid_path(path)?;
        self.inner.list_directory_checked(path, check)
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
    pub fn rename_snapshot(
        &self,
        path: &str,
        to: &str,
        id: &str,
        version: &str,
        directory: bool,
    ) -> anyhow::Result<MetadataEntry> {
        self.check_write()?;
        ensure!(self.capabilities().rename, "此连接不支持重命名");
        valid_path(path)?;
        valid_path(to)?;
        ensure!(
            path != "/" && to != "/" && path != to && !to.starts_with(&format!("{path}/")),
            "无效的移动目标"
        );
        self.inner.rename_snapshot(path, to, id, version, directory)
    }
    pub fn delete_path(&self, path: &str) -> anyhow::Result<()> {
        ensure!(path != "/", "不能删除共享根目录");
        self.delete(&self.entry(path)?.remote_id)
    }
    pub fn manual_upload_supported(&self) -> bool {
        self.capabilities().upload
            && (self.capabilities().conditional_upload || self.inner.manual_upload_supported())
    }
    pub fn manual_upload_path(
        &self,
        path: &str,
        source: &Path,
        version: Option<&str>,
        progress: Progress,
    ) -> anyhow::Result<MetadataEntry> {
        if self.capabilities().conditional_upload {
            return self.upload_path(path, source, version, progress);
        }
        self.check_write()?;
        valid_path(path)?;
        ensure!(self.manual_upload_supported(), "此连接不支持手动写入");
        let current = self.get_metadata_by_path(path)?;
        crate::provider_guard::snapshot(
            self,
            path,
            current.as_ref().map(|m| m.remote_id.as_str()),
            version,
        )?;
        let guard = self.clone();
        let progress: Progress = Arc::new(move |done, total| {
            guard.check_write()?;
            progress(done, total)
        });
        self.inner.manual_upload(
            path,
            source,
            current.as_ref().map(|m| m.remote_id.as_str()),
            version,
            progress,
        )
    }
    pub fn manual_upload_snapshot(
        &self,
        path: &str,
        source: &Path,
        id: &str,
        version: &str,
        progress: Progress,
    ) -> anyhow::Result<MetadataEntry> {
        if self.capabilities().conditional_upload {
            return self.upload_snapshot(path, source, id, version, progress);
        }
        self.check_write()?;
        ensure!(self.manual_upload_supported(), "此连接不支持手动写入");
        let guard = self.clone();
        let progress: Progress = Arc::new(move |done, total| {
            guard.check_write()?;
            progress(done, total)
        });
        self.inner
            .manual_upload(path, source, Some(id), Some(version), progress)
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
        self.check_write()?;
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
        let write_owner = self.clone();
        self.inner.upload_controlled(
            path,
            source,
            entry.as_ref().map(|e| e.remote_id.as_str()),
            version,
            Arc::new(move |done, total| {
                write_owner.check_write()?;
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
        self.check_write()?;
        let caps = self.capabilities();
        ensure!(
            caps.upload && caps.conditional_upload && !id.is_empty() && !version.is_empty(),
            "versioned conditional upload unavailable"
        );
        let paused = self.paused.clone();
        let write_owner = self.clone();
        self.inner.upload_controlled(
            path,
            source,
            Some(id),
            Some(version),
            Arc::new(move |done, total| {
                write_owner.check_write()?;
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
        self.check_write()?;
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
        self.check_write()?;
        self.inner
            .upload_file_with_version(path, source, id, version, &mut |done, total| {
                self.check_write()?;
                progress(done, total)
            })
    }
    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        self.check_pause()?;
        valid_path(path)?;
        ensure!(self.capabilities().mkdir, "此连接不支持创建目录");
        self.check_write()?;
        self.inner.create_folder(path)
    }
    fn rename(&self, id: &str, path: &str) -> anyhow::Result<MetadataEntry> {
        self.check_pause()?;
        valid_path(path)?;
        ensure!(self.capabilities().rename, "此连接不支持重命名");
        self.check_write()?;
        self.inner.rename(id, path)
    }
    fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.check_pause()?;
        ensure!(self.capabilities().delete, "此连接不支持删除");
        self.check_write()?;
        self.inner.delete(id)
    }
}
