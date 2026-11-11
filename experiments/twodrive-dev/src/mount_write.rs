//! Opt-in mount mutations for Providers with real conditional-upload support.
//! Failed content is kept in a private durable spool, never discarded on error.
use crate::{
    model::{CloudBackend, MetadataEntry},
    provider::{Backend, valid_path},
    state::{State, random_secret},
};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::File,
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
};
#[path = "editor_locks.rs"]
mod editor_locks;

pub fn account_cache_directory() -> &'static str {
    if cfg!(target_os = "linux") {
        "linux-accounts"
    } else {
        "windows-accounts"
    }
}

/// A profile's durable native cache follows the provider's storage identity,
/// never its permission revision. Old identities remain available for recovery.
pub fn account_cache(mount_state: &Path, binding: &str) -> std::path::PathBuf {
    mount_state
        .join(account_cache_directory())
        .join(format!("{:x}", Sha256::digest(binding.as_bytes())))
}

pub(crate) struct Writer {
    backend: Backend,
    state: State,
    observed: Mutex<HashMap<String, Version>>,
    mutation: Mutex<()>,
    editor_locks: Option<Arc<editor_locks::EditorLocks>>,
    local_excluded: crate::local_upload_exclusions::LocalExcluded,
}

#[derive(Clone, Deserialize, Serialize)]
struct Version {
    id: String,
    etag: String,
    directory: bool,
}
impl From<&MetadataEntry> for Version {
    fn from(m: &MetadataEntry) -> Self {
        Self {
            id: m.remote_id.clone(),
            etag: m.etag.clone(),
            directory: m.is_dir,
        }
    }
}
/// Durable private files; cache pruning never removes pending editor content.
pub fn recovery_status(directory: &Path) -> anyhow::Result<serde_json::Value> {
    let mut count = 0u64;
    let mut bytes = 0u64;
    if directory.exists() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            if entry.path().extension().is_some_and(|e| e == "content") {
                count += 1;
                bytes = bytes.saturating_add(entry.metadata()?.len());
            }
        }
    }
    let (locks, lock_bytes, failed_locks) = editor_locks::pending(directory)?;
    Ok(
        json!({"files":count + failed_locks,"bytes":bytes.saturating_add(lock_bytes),"directory":directory,"editor_pending":locks}),
    )
}

fn resource(encoded: &str) -> anyhow::Result<String> {
    let path = percent_encoding::percent_decode_str(encoded)
        .decode_utf8()?
        .into_owned();
    let path = if path == "/" {
        path
    } else {
        path.trim_end_matches('/').to_owned()
    };
    valid_path(&path)?;
    ensure!(
        path.len() <= 4096 && !path.chars().any(char::is_control),
        "无效路径"
    );
    Ok(path)
}
pub(crate) async fn handle(
    writer: Arc<Writer>,
    request: http::Request<hyper::body::Incoming>,
    address: std::net::SocketAddr,
) -> (http::Response<dav_server::body::Body>, bool) {
    use http_body_util::BodyExt;
    let result = async {
        let path = resource(request.uri().path())?;
        let method = request.method().as_str().to_owned();
        if method == "PUT" {
            ensure!(
                !request.headers().contains_key("Content-Range"),
                "不支持分段写入"
            );
            let expected = request
                .headers()
                .get("Content-Length")
                .map(|h| h.to_str()?.parse::<u64>().map_err(anyhow::Error::from))
                .transpose()?;
            let limit = if writer.editor_locks.is_some() && editor_locks::matches(&path) {
                editor_locks::LIMIT
            } else {
                64 * 1024 * 1024 * 1024
            };
            ensure!(expected.is_none_or(|n| n <= limit), "写入过大");
            let (id, mut file) = writer.spool(&path)?;
            let mut body = request.into_body();
            let mut count = 0u64;
            while let Some(frame) =
                tokio::time::timeout(std::time::Duration::from_secs(30), body.frame()).await?
            {
                let frame = frame?;
                if let Ok(data) = frame.into_data() {
                    count = count.checked_add(data.len() as u64).context("写入过大")?;
                    ensure!(count <= limit, "写入过大");
                    file.write_all(&data)?;
                }
            }
            ensure!(expected.is_none_or(|n| n == count), "写入不完整");
            file.sync_all()?;
            drop(file);
            let local_only = tokio::task::spawn_blocking(move || writer.put(&id, &path)).await??;
            return Ok::<_, anyhow::Error>((http::StatusCode::CREATED, !local_only));
        }
        let destination = if method == "MOVE" {
            let destination = url::Url::parse(
                request
                    .headers()
                    .get("Destination")
                    .context("缺少目标")?
                    .to_str()?,
            )?;
            ensure!(
                destination.origin().ascii_serialization() == format!("http://{address}")
                    && destination.query().is_none()
                    && destination.fragment().is_none(),
                "跨挂载移动不受支持"
            );
            Some(resource(destination.path())?)
        } else {
            None
        };
        let local_only = tokio::task::spawn_blocking(move || match method.as_str() {
            "MKCOL" => writer.mkdir(&path).map(|()| false),
            "DELETE" => writer.delete(&path),
            "MOVE" => writer
                .rename(&path, destination.as_deref().unwrap())
                .map(|()| false),
            _ => anyhow::bail!("不支持的写入方法"),
        })
        .await??;
        Ok((http::StatusCode::NO_CONTENT, !local_only))
    }
    .await;
    let (status, body, invalidate) = match result {
        Ok((status, invalidate)) => (status, dav_server::body::Body::empty(), invalidate),
        Err(_) => (
            http::StatusCode::CONFLICT,
            dav_server::body::Body::from(
                "写入未完成；请检查连接或版本冲突；已接收的编辑内容可从本机恢复目录找回。",
            ),
            // Even a failed cloud operation may have completed upstream.
            true,
        ),
    };
    (
        http::Response::builder().status(status).body(body).unwrap(),
        invalidate,
    )
}
impl Writer {
    pub(crate) fn new(backend: Backend, directory: &Path) -> anyhow::Result<Arc<Self>> {
        Self::new_with_editor_locks(backend, directory, false)
    }
    pub(crate) fn new_with_editor_locks(
        backend: Backend,
        directory: &Path,
        local_locks: bool,
    ) -> anyhow::Result<Arc<Self>> {
        let c = backend.capabilities();
        ensure!(
            c.upload && backend.manual_upload_supported() && c.mkdir && c.rename && c.delete,
            "此网盘尚不支持带版本保护的可编辑挂载"
        );
        let state = State::new(directory.to_owned())?;
        let observed = if state.dir.join("observed.json").exists() {
            state.read("observed.json")?
        } else {
            HashMap::new()
        };
        Ok(Arc::new(Self {
            backend,
            state,
            observed: Mutex::new(observed),
            mutation: Mutex::default(),
            local_excluded: crate::local_upload_exclusions::LocalExcluded::new(
                &directory.join("local-excluded"),
            )?,
            editor_locks: local_locks
                .then(|| editor_locks::EditorLocks::new(&directory.join("editor-locks")))
                .transpose()?,
        }))
    }
    pub(crate) fn start_editor_worker(
        self: &Arc<Self>,
        stop: Arc<std::sync::atomic::AtomicBool>,
        deleted: editor_locks::Deleted,
    ) {
        if let Some(locks) = &self.editor_locks {
            locks.start(Arc::downgrade(self), stop, deleted);
        }
    }
    pub(crate) fn editor_file(
        &self,
        path: &str,
    ) -> anyhow::Result<Option<(MetadataEntry, Vec<u8>)>> {
        self.editor_locks
            .as_ref()
            .map_or(Ok(None), |locks| locks.file(path))
    }
    pub(crate) fn excluded_file(
        &self,
        path: &str,
    ) -> anyhow::Result<Option<(MetadataEntry, File)>> {
        self.local_excluded.file(path)
    }
    pub(crate) fn editor_entries(&self, parent: &str) -> anyhow::Result<Vec<MetadataEntry>> {
        let mut entries = self
            .editor_locks
            .as_ref()
            .map_or_else(|| Ok(Vec::new()), |locks| locks.entries(parent))?;
        entries.extend(self.local_excluded.entries(parent)?);
        Ok(entries)
    }
    pub(crate) fn editor_hidden(&self) -> anyhow::Result<std::collections::HashSet<String>> {
        self.editor_locks
            .as_ref()
            .map_or_else(|| Ok(Default::default()), |locks| locks.hidden())
    }
    pub(crate) fn observe(&self, entry: &MetadataEntry) -> anyhow::Result<()> {
        self.observe_many(std::slice::from_ref(entry))
    }
    pub(crate) fn observe_many(&self, entries: &[MetadataEntry]) -> anyhow::Result<()> {
        let mut seen = self
            .observed
            .lock()
            .map_err(|_| anyhow::anyhow!("版本记录不可用"))?;
        let mut changed = false;
        for entry in entries {
            if entry.path != "/" && !seen.contains_key(&entry.path) {
                ensure!(seen.len() < 100000, "挂载版本记录超过预算");
                seen.insert(entry.path.clone(), Version::from(entry));
                changed = true;
            }
        }
        if changed {
            self.state.save("observed.json", &*seen)?;
        }
        Ok(())
    }
    fn refresh_parents(&self, path: &str) -> anyhow::Result<()> {
        self.refresh_parents_many(&[path])
    }
    fn refresh_parents_many(&self, paths: &[&str]) -> anyhow::Result<()> {
        let mut parents = std::collections::HashSet::new();
        for path in paths {
            let mut current = *path;
            for _ in 0..128 {
                let Some((parent, _)) = current.rsplit_once('/') else {
                    break;
                };
                if parent.is_empty() {
                    break;
                }
                if parents.insert(parent.to_owned())
                    && let Some(m) = self.backend.get_metadata_by_path(parent)?
                {
                    ensure!(m.is_dir, "父目录类型改变");
                    self.observed
                        .lock()
                        .map_err(|_| anyhow::anyhow!("版本记录不可用"))?
                        .insert(parent.into(), Version::from(&m));
                }
                current = parent;
            }
        }
        self.state
            .save("observed.json", &*self.observed.lock().unwrap())?;
        Ok(())
    }
    fn expected(&self, path: &str) -> anyhow::Result<Option<MetadataEntry>> {
        valid_path(path)?;
        ensure!(path != "/", "不能修改挂载根目录");
        let current = self.backend.get_metadata_by_path(path)?;
        let observed = self
            .observed
            .lock()
            .map_err(|_| anyhow::anyhow!("版本记录不可用"))?;
        if let Some(old) = observed.get(path) {
            ensure!(
                current.as_ref().is_some_and(|v| v.remote_id == old.id
                    && v.etag == old.etag
                    && v.is_dir == old.directory),
                "云端版本已改变，未覆盖，编辑内容保留在本机"
            );
        } else {
            ensure!(current.is_none(), "尚未读取目标版本，拒绝覆盖现有文件");
        }
        Ok(current)
    }
    pub(crate) fn spool(&self, path: &str) -> anyhow::Result<(String, File)> {
        valid_path(path)?;
        ensure!(path != "/", "不能写入挂载根目录");
        let status = recovery_status(&self.state.dir)?;
        ensure!(
            status["files"].as_u64().unwrap_or(0) < 16,
            "待恢复文件过多，请先打开本机恢复目录处理"
        );
        let id = random_secret();
        let content = self.state.dir.join(format!("{id}.content"));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(content)?;
        self.state.save(
            &format!("{id}.json"),
            &json!({"path":path,"state":"receiving"}),
        )?;
        Ok((id, file))
    }
    pub(crate) fn put(&self, id: &str, path: &str) -> anyhow::Result<bool> {
        let content = self.state.dir.join(format!("{id}.content"));
        self.backend.check_local_write()?;
        let excluded = self.backend.sync_policy().excludes_file(path);
        let local_only = excluded || (self.editor_locks.is_some() && editor_locks::matches(path));
        let result = if excluded || self.local_excluded.file(path)?.is_some() {
            self.local_excluded.put(path, &content).and_then(|()| {
                self.local_excluded
                    .file(path)?
                    .map(|(m, _)| m)
                    .context("本地文件未发布")
            })
        } else if let Some(locks) = self
            .editor_locks
            .as_ref()
            .filter(|_| editor_locks::matches(path))
        {
            locks.put(self, id, path, &content)
        } else {
            self.put_content(path, &content)
        };
        match &result {
            Ok(_) => {
                std::fs::remove_file(content)?;
                std::fs::remove_file(self.state.dir.join(format!("{id}.json")))?;
            }
            Err(_) => {
                self.state.save(
                    &format!("{id}.json"),
                    &json!({"path":path,"state":"needs_recovery"}),
                )?;
            }
        }
        result.map(|_| local_only)
    }
    fn put_content(&self, path: &str, content: &Path) -> anyhow::Result<MetadataEntry> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| anyhow::anyhow!("写入锁不可用"))?;
        let expected = self.expected(path)?;
        ensure!(!expected.as_ref().is_some_and(|m| m.is_dir), "目标是目录");
        let progress = Arc::new(|_, _| Ok(()));
        let meta = match expected {
            Some(m) => self.backend.manual_upload_snapshot(
                path,
                content,
                &m.remote_id,
                &m.etag,
                progress,
            )?,
            None => self
                .backend
                .manual_upload_path(path, content, None, progress)?,
        };
        self.observed
            .lock()
            .map_err(|_| anyhow::anyhow!("版本记录不可用"))?
            .insert(path.into(), Version::from(&meta));
        self.state
            .save("observed.json", &*self.observed.lock().unwrap())?;
        self.refresh_parents(path)?;
        Ok(meta)
    }
    pub(crate) fn mkdir(&self, path: &str) -> anyhow::Result<()> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| anyhow::anyhow!("写入锁不可用"))?;
        ensure!(self.expected(path)?.is_none(), "目标已存在");
        let m = self.backend.create_folder(path)?;
        self.refresh_parents(path)?;
        self.observed
            .lock()
            .map_err(|_| anyhow::anyhow!("版本记录不可用"))?
            .insert(path.into(), Version::from(&m));
        self.state
            .save("observed.json", &*self.observed.lock().unwrap())?;
        Ok(())
    }
    pub(crate) fn delete(&self, path: &str) -> anyhow::Result<bool> {
        self.backend.check_local_write()?;
        ensure!(
            !self.local_excluded.has_children(path)?,
            "目录仍含本地排除文件，请先处理这些文件"
        );
        if self.local_excluded.delete(path)? {
            return Ok(true);
        }
        if let Some(locks) = &self.editor_locks
            && locks.delete(path)?
        {
            return Ok(true);
        }
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| anyhow::anyhow!("写入锁不可用"))?;
        let m = self.expected(path)?.context("目标不存在")?;
        if m.is_dir {
            ensure!(
                self.backend.list_directory(path)?.is_empty(),
                "请先处理目录内文件，再删除空目录"
            );
        }
        self.backend.delete(&m.remote_id)?;
        self.refresh_parents(path)?;
        self.observed
            .lock()
            .map_err(|_| anyhow::anyhow!("版本记录不可用"))?
            .remove(path);
        self.state
            .save("observed.json", &*self.observed.lock().unwrap())?;
        Ok(false)
    }
    fn delete_editor_version(&self, path: &str, id: &str, etag: &str) -> anyhow::Result<()> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| anyhow::anyhow!("写入锁不可用"))?;
        if let Some(current) = self.backend.get_metadata_by_path(path)? {
            ensure!(
                !current.is_dir && current.remote_id == id && current.etag == etag,
                "云端锁已改变，未删除其他编辑器的锁"
            );
            self.backend.delete(id)?;
            self.refresh_parents(path)?;
        }
        self.observed
            .lock()
            .map_err(|_| anyhow::anyhow!("版本记录不可用"))?
            .remove(path);
        self.state
            .save("observed.json", &*self.observed.lock().unwrap())?;
        Ok(())
    }
    pub(crate) fn rename(&self, path: &str, to: &str) -> anyhow::Result<()> {
        self.backend.check_local_write()?;
        ensure!(
            !self.local_excluded.has_children(path)?,
            "目录仍含本地排除文件，请先处理这些文件"
        );
        if self.local_excluded.file(path)?.is_some() {
            ensure!(
                self.backend.sync_policy().excludes_file(to),
                "请将本地排除文件另存为普通文件后上传"
            );
            ensure!(self.local_excluded.rename(path, to)?, "本地源文件不存在");
            return Ok(());
        }
        ensure!(
            !self.backend.sync_policy().excludes_file(to),
            "不能将云端文件改名为排除文件；请另存本地副本"
        );
        ensure!(
            self.editor_file(path)?.is_none()
                && self.editor_file(to)?.is_none()
                && !self.editor_hidden()?.contains(path)
                && !self.editor_hidden()?.contains(to),
            "请先关闭正在使用的Office临时锁"
        );
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| anyhow::anyhow!("写入锁不可用"))?;
        valid_path(path)?;
        valid_path(to)?;
        let observed = self
            .observed
            .lock()
            .map_err(|_| anyhow::anyhow!("版本记录不可用"))?
            .get(path)
            .cloned()
            .context("尚未读取源文件版本，拒绝重命名")?;
        // The adapter checks this exact observed version with fresh metadata.
        // Do not check the same source/target twice across service layers.
        let next = self.backend.rename_snapshot(
            path,
            to,
            &observed.id,
            &observed.etag,
            observed.directory,
        )?;
        self.refresh_parents_many(&[path, to])?;
        let mut seen = self
            .observed
            .lock()
            .map_err(|_| anyhow::anyhow!("版本记录不可用"))?;
        seen.retain(|p, _| p != path && !p.starts_with(&format!("{path}/")));
        seen.insert(to.into(), Version::from(&next));
        self.state.save("observed.json", &*seen)?;
        Ok(())
    }
}
