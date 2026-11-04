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
    Ok(json!({"files":count,"bytes":bytes,"directory":directory}))
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
) -> http::Response<dav_server::body::Body> {
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
            const LIMIT: u64 = 64 * 1024 * 1024 * 1024;
            ensure!(expected.is_none_or(|n| n <= LIMIT), "写入过大");
            let (id, mut file) = writer.spool(&path)?;
            let mut body = request.into_body();
            let mut count = 0u64;
            while let Some(frame) =
                tokio::time::timeout(std::time::Duration::from_secs(30), body.frame()).await?
            {
                let frame = frame?;
                if let Ok(data) = frame.into_data() {
                    count = count.checked_add(data.len() as u64).context("写入过大")?;
                    ensure!(count <= LIMIT, "写入过大");
                    file.write_all(&data)?;
                }
            }
            ensure!(expected.is_none_or(|n| n == count), "写入不完整");
            file.sync_all()?;
            drop(file);
            tokio::task::spawn_blocking(move || writer.put(&id, &path)).await??;
            return Ok::<_, anyhow::Error>(http::StatusCode::CREATED);
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
        tokio::task::spawn_blocking(move || match method.as_str() {
            "MKCOL" => writer.mkdir(&path),
            "DELETE" => writer.delete(&path),
            "MOVE" => writer.rename(&path, destination.as_deref().unwrap()),
            _ => anyhow::bail!("不支持的写入方法"),
        })
        .await??;
        Ok(http::StatusCode::NO_CONTENT)
    }
    .await;
    let (status, body) = match result {
        Ok(status) => (status, dav_server::body::Body::empty()),
        Err(_) => (
            http::StatusCode::CONFLICT,
            dav_server::body::Body::from(
                "写入未完成；请检查连接或版本冲突；已接收的编辑内容可从本机恢复目录找回。",
            ),
        ),
    };
    http::Response::builder().status(status).body(body).unwrap()
}
impl Writer {
    pub(crate) fn new(backend: Backend, directory: &Path) -> anyhow::Result<Arc<Self>> {
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
        }))
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
    pub(crate) fn put(&self, id: &str, path: &str) -> anyhow::Result<MetadataEntry> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| anyhow::anyhow!("写入锁不可用"))?;
        let content = self.state.dir.join(format!("{id}.content"));
        let result = (|| {
            let expected = self.expected(path)?;
            ensure!(!expected.as_ref().is_some_and(|m| m.is_dir), "目标是目录");
            let progress = Arc::new(|_, _| Ok(()));
            let meta = match expected {
                Some(m) => self.backend.manual_upload_snapshot(
                    path,
                    &content,
                    &m.remote_id,
                    &m.etag,
                    progress,
                )?,
                None => self
                    .backend
                    .manual_upload_path(path, &content, None, progress)?,
            };
            self.observed
                .lock()
                .map_err(|_| anyhow::anyhow!("版本记录不可用"))?
                .insert(path.into(), Version::from(&meta));
            self.state
                .save("observed.json", &*self.observed.lock().unwrap())?;
            self.refresh_parents(path)?;
            Ok(meta)
        })();
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
        result
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
    pub(crate) fn delete(&self, path: &str) -> anyhow::Result<()> {
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
        Ok(())
    }
    pub(crate) fn rename(&self, path: &str, to: &str) -> anyhow::Result<()> {
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
