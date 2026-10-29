use super::auth;
use crate::{
    model::{CloudBackend, DeltaResult, MetadataEntry},
    provider::{Capabilities, Progress, Provider, StorageUsage, valid_path},
    state::State,
};
use anyhow::{Context, ensure};
use reqwest::{
    Method,
    blocking::{Client, RequestBuilder, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};
use url::Url;

const API: &str = "https://www.googleapis.com/drive/v2/";
const UPLOAD: &str = "https://www.googleapis.com/upload/drive/v2/";
const FOLDER: &str = "application/vnd.google-apps.folder";
const FIELDS: &str =
    "id,title,mimeType,fileSize,modifiedDate,etag,parents(id),labels(trashed),alternateLink";
const LIMIT: usize = 100_000;
const CHUNK: usize = 8 * 1024 * 1024;
#[derive(Debug, thiserror::Error)]
#[error("Google Drive {operation} 失败（HTTP {status}）{hint}")]
pub struct GoogleError {
    pub operation: &'static str,
    pub status: u16,
    pub hint: &'static str,
}
#[derive(Clone)]
pub struct GoogleDrive {
    state: State,
    client: Client,
    root: String,
    pub account_id: String,
    pub account_name: String,
    writable: bool,
    api: Url,
    upload: Url,
    #[cfg(test)]
    test_token: Option<String>,
}
#[derive(Clone, Deserialize)]
struct DriveFile {
    id: String,
    title: String,
    #[serde(rename = "mimeType")]
    mime: String,
    #[serde(default, rename = "fileSize")]
    size: String,
    #[serde(default, rename = "modifiedDate")]
    modified: String,
    #[serde(default)]
    etag: String,
    #[serde(default)]
    parents: Vec<Parent>,
    #[serde(default)]
    labels: Labels,
    #[serde(default, rename = "alternateLink")]
    link: String,
}
#[derive(Clone, Default, Deserialize)]
struct Labels {
    #[serde(default)]
    trashed: bool,
}
#[derive(Clone, Deserialize)]
struct Parent {
    id: String,
}
#[derive(Serialize, Deserialize)]
struct Intent {
    id: String,
    fingerprint: String,
    session: Option<String>,
}
fn valid_id(id: &str) -> anyhow::Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 256
            && id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "无效的 Google 文件 ID"
    );
    Ok(())
}
fn strong_etag(etag: &str) -> anyhow::Result<()> {
    ensure!(
        etag.len() >= 2
            && etag.starts_with('"')
            && etag.ends_with('"')
            && !etag[1..etag.len() - 1].contains(['"', '\r', '\n']),
        "Google 未返回可用于安全覆盖的 ETag"
    );
    Ok(())
}
fn component(name: &str) -> String {
    if name.is_empty() {
        return "%EMPTY".into();
    }
    let mut escaped = String::new();
    for ch in name.chars() {
        if ch.is_ascii_control() || matches!(ch, '/' | '\\') {
            escaped.push_str(&format!("%{:02X}", ch as u32));
        } else {
            escaped.push(ch);
        }
    }
    if escaped == "." || escaped == ".." || escaped.trim() != escaped {
        return name
            .as_bytes()
            .iter()
            .map(|b| format!("%{b:02X}"))
            .collect();
    }
    escaped
}
fn title(component: &str) -> anyhow::Result<String> {
    // New local names are literal. Existing escaped/duplicate display aliases
    // are resolved by opaque file ID, never decoded into another target name.
    ensure!(
        !component.is_empty()
            && component.len() <= 1024
            && !component.chars().any(|c| c.is_ascii_control()),
        "无效的 Google 文件名称"
    );
    Ok(component.into())
}
fn path_join(parent: &str, name: &str) -> String {
    format!("{}/{}", parent.trim_end_matches('/'), name)
}
impl DriveFile {
    fn native(&self) -> bool {
        self.mime.starts_with("application/vnd.google-apps.") && self.mime != FOLDER
    }
    fn shortcut(&self) -> Vec<u8> {
        // Google editor files have no ordinary file bytes. Expose an explicit browser shortcut.
        format!(
            "[InternetShortcut]\nURL={}\n",
            self.link.replace(['\n', '\r'], "")
        )
        .into_bytes()
    }
    fn metadata(&self, path: String) -> anyhow::Result<MetadataEntry> {
        valid_id(&self.id)?;
        let modified = time::OffsetDateTime::parse(
            &self.modified,
            &time::format_description::well_known::Rfc3339,
        )
        .map(|t| t.unix_timestamp())
        .unwrap_or(0);
        let size = if self.native() {
            self.shortcut().len() as u64
        } else {
            self.size.parse().unwrap_or(0)
        };
        Ok(if self.mime == FOLDER {
            MetadataEntry::new_dir(&self.id, path, modified, &self.etag)
        } else {
            MetadataEntry::new_file(&self.id, path, size, modified, &self.etag)
        })
    }
}
impl GoogleDrive {
    pub fn connect(state: State, route: &str, writable: bool) -> anyhow::Result<Self> {
        let mut drive = Self {
            state,
            client: auth::client(route)?,
            root: String::new(),
            account_id: String::new(),
            account_name: String::new(),
            writable,
            api: Url::parse(API)?,
            upload: Url::parse(UPLOAD)?,
            #[cfg(test)]
            test_token: None,
        };
        eprintln!("network: Google connection stage=account_probe_started");
        let about = drive.json(
            drive.request(Method::GET, "about")?.query(&[(
                "fields",
                "rootFolderId,user(permissionId,displayName,emailAddress)",
            )]),
            "连接",
        )?;
        drive.root = about["rootFolderId"]
            .as_str()
            .context("Google 未返回根目录 ID")?
            .into();
        drive.account_id = about["user"]["permissionId"]
            .as_str()
            .context("Google 未返回账号 ID")?
            .into();
        drive.account_name = about["user"]["emailAddress"]
            .as_str()
            .or(about["user"]["displayName"].as_str())
            .unwrap_or("Google Drive")
            .into();
        valid_id(&drive.root)?;
        eprintln!("network: Google connection stage=account_probe_completed");
        Ok(drive)
    }
    fn token(&self) -> anyhow::Result<String> {
        #[cfg(test)]
        if let Some(t) = &self.test_token {
            return Ok(t.clone());
        }
        auth::access_token(&self.state, &self.client)
    }
    fn request(&self, method: Method, path: &str) -> anyhow::Result<RequestBuilder> {
        Ok(self
            .client
            .request(method, self.api.join(path)?)
            .bearer_auth(self.token()?))
    }
    fn send(&self, request: RequestBuilder, operation: &'static str) -> anyhow::Result<Response> {
        let response = request.send().map_err(|_| {
            anyhow::anyhow!("Google Drive {operation} 连接失败，请检查网络；待处理数据保留在本机")
        })?;
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status().as_u16();
        Err(GoogleError {
            operation,
            status,
            hint: match status {
                401 => "；请重新登录",
                403 => "；请启用项目的 Google Drive API，并确认账号权限/配额",
                409 | 412 => "；远端版本发生冲突，未覆盖",
                429 => "；请稍后重试",
                _ => "",
            },
        }
        .into())
    }
    fn read_json(response: Response) -> anyhow::Result<Value> {
        let mut bytes = Vec::new();
        response
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 16 * 1024 * 1024, "Google 元数据响应过大");
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("Google 返回无效的元数据"))
    }
    fn json(&self, request: RequestBuilder, op: &'static str) -> anyhow::Result<Value> {
        Self::read_json(self.send(request, op)?)
    }
    fn file(&self, id: &str) -> anyhow::Result<Option<DriveFile>> {
        valid_id(id)?;
        let response = self
            .request(Method::GET, &format!("files/{id}"))?
            .query(&[("fields", FIELDS)]);
        let value = match self.json(response, "读取文件信息") {
            Ok(v) => v,
            Err(e)
                if e.downcast_ref::<GoogleError>()
                    .is_some_and(|e| e.status == 404) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let file: DriveFile =
            serde_json::from_value(value).map_err(|_| anyhow::anyhow!("Google 文件信息不完整"))?;
        Ok((!file.labels.trashed).then_some(file))
    }
    fn inventory(&self, parent: Option<&str>) -> anyhow::Result<Vec<DriveFile>> {
        if let Some(id) = parent {
            valid_id(id)?;
        }
        let mut all = Vec::<DriveFile>::new();
        let mut page = String::new();
        let mut seen = BTreeSet::new();
        let mut page_count = 0usize;
        let mut page_tokens = BTreeSet::new();
        loop {
            let q = parent
                .map(|id| format!("'{id}' in parents and trashed = false"))
                .unwrap_or_else(|| "trashed = false".into());
            let fields = format!("nextPageToken,incompleteSearch,items({FIELDS})");
            let v = self.json(
                self.request(Method::GET, "files")?.query(&[
                    ("q", q.as_str()),
                    ("maxResults", "1000"),
                    ("fields", fields.as_str()),
                    ("pageToken", page.as_str()),
                ]),
                "读取目录",
            )?;
            ensure!(
                v["incompleteSearch"] != true,
                "Google 索引搜索不完整，未发布部分结果"
            );
            for item in v["items"].as_array().context("Google 目录响应缺少 items")? {
                let f: DriveFile = serde_json::from_value(item.clone())
                    .map_err(|_| anyhow::anyhow!("Google 文件信息不完整"))?;
                valid_id(&f.id)?;
                ensure!(
                    parent.is_none_or(|id| f.parents.iter().any(|p| p.id == id))
                        && !f.labels.trashed,
                    "Google 目录返回非直属文件"
                );
                ensure!(seen.insert(f.id.clone()), "Google 目录包含重复 ID");
                all.push(f);
                ensure!(all.len() <= LIMIT, "Google 目录超过文件数量上限");
            }
            page_count += 1;
            if parent.is_none() {
                eprintln!(
                    "network: Google snapshot stage=page_received pages={page_count} objects={}",
                    all.len()
                );
            }
            ensure!(page_count <= LIMIT, "Google 分页次数超出上限");
            let next = v["nextPageToken"].as_str().unwrap_or("");
            if next.is_empty() {
                break;
            }
            ensure!(
                next.len() <= 16384 && next != page && page_tokens.insert(next.to_owned()),
                "无效的 Google 分页游标"
            );
            page = next.into();
        }
        Ok(all)
    }
    fn children(
        &self,
        parent: &str,
        path: &str,
    ) -> anyhow::Result<Vec<(DriveFile, MetadataEntry)>> {
        Self::entries(self.inventory(Some(parent))?, path)
    }
    fn entries(all: Vec<DriveFile>, path: &str) -> anyhow::Result<Vec<(DriveFile, MetadataEntry)>> {
        let mut counts = BTreeMap::new();
        for f in &all {
            *counts.entry(component(&f.title)).or_insert(0usize) += 1;
        }
        let mut result = Vec::new();
        for f in all {
            let mut name = component(&f.title);
            if counts[&name] > 1 {
                name.push_str(&format!("~g~{}", f.id));
            }
            if f.native() {
                name.push_str("~google.url");
            }
            ensure!(
                name.len() <= 255,
                "Google 文件名称超出文件系统上限；请在网页缩短名称"
            );
            let m = f.metadata(path_join(path, &name))?;
            result.push((f, m));
        }
        for _ in 0..4 {
            let mut names = BTreeMap::new();
            for (_, m) in &result {
                *names.entry(m.path.clone()).or_insert(0usize) += 1;
            }
            if names.values().all(|count| *count == 1) {
                break;
            }
            for (f, m) in &mut result {
                if names[&m.path] > 1 {
                    let name = format!("{}~g~{}", m.name, f.id);
                    ensure!(name.len() <= 255, "Google 文件名称超出文件系统上限");
                    *m = f.metadata(path_join(path, &name))?;
                }
            }
        }
        ensure!(
            result
                .iter()
                .map(|(_, m)| &m.path)
                .collect::<BTreeSet<_>>()
                .len()
                == result.len(),
            "Google 路径别名发生冲突"
        );
        result.sort_by(|a, b| a.1.path.cmp(&b.1.path));
        Ok(result)
    }
    fn resolve(&self, path: &str) -> anyhow::Result<Option<(DriveFile, MetadataEntry)>> {
        valid_path(path)?;
        if path == "/" {
            return self
                .file(&self.root)?
                .map(|f| {
                    let m = f.metadata("/".into())?;
                    Ok((f, m))
                })
                .transpose();
        }
        let mut id = self.root.clone();
        let mut parent = "/".to_string();
        let mut found = None;
        let parts: Vec<_> = path[1..].split('/').collect();
        for (i, part) in parts.iter().enumerate() {
            let next = path_join(&parent, part);
            found = self
                .children(&id, &parent)?
                .into_iter()
                .find(|(_, m)| m.path == next);
            let Some((f, m)) = &found else {
                return Ok(None);
            };
            if i + 1 < parts.len() {
                ensure!(m.is_dir, "路径中包含普通文件");
            }
            id = f.id.clone();
            parent = next;
        }
        Ok(found)
    }
    fn destination(&self, path: &str) -> anyhow::Result<(String, String)> {
        valid_path(path)?;
        ensure!(path != "/", "不能修改根目录");
        let (parent, name) = path.rsplit_once('/').unwrap();
        let parent = if parent.is_empty() { "/" } else { parent };
        let (f, m) = self.resolve(parent)?.context("目标父目录不存在")?;
        ensure!(m.is_dir, "目标父路径不是目录");
        Ok((
            f.id,
            if let Some((entry, _)) = self.resolve(path)? {
                entry.title
            } else {
                title(name)?
            },
        ))
    }
    fn check_write(&self) -> anyhow::Result<()> {
        ensure!(self.writable, "Google Drive 连接为只读");
        Ok(())
    }
    fn snapshot(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        // One paginated inventory, then traverse in memory. A network request per
        // directory can prevent a large account from reaching FUSE startup.
        eprintln!("network: Google snapshot stage=inventory_started");
        let started = std::time::Instant::now();
        let inventory = self.inventory(None)?;
        eprintln!(
            "network: Google snapshot stage=inventory_completed objects={} elapsed_ms={}",
            inventory.len(),
            started.elapsed().as_millis()
        );
        let mut by_parent = BTreeMap::<String, Vec<DriveFile>>::new();
        for file in inventory {
            for parent in &file.parents {
                by_parent
                    .entry(parent.id.clone())
                    .or_default()
                    .push(file.clone());
            }
        }
        let mut queue = VecDeque::from([(self.root.clone(), "/".to_owned())]);
        let mut seen = BTreeSet::from([self.root.clone()]);
        let mut all = Vec::new();
        while let Some((id, path)) = queue.pop_front() {
            for (f, m) in Self::entries(by_parent.remove(&id).unwrap_or_default(), &path)? {
                if !seen.insert(f.id) {
                    continue;
                }
                if m.is_dir {
                    queue.push_back((m.remote_id.clone(), m.path.clone()));
                }
                all.push(m);
                ensure!(all.len() <= LIMIT, "Google Drive 超出索引数量上限");
            }
        }
        eprintln!(
            "network: Google snapshot stage=tree_completed objects={}",
            all.len()
        );
        Ok(all)
    }
    fn session_url(&self, s: &str) -> anyhow::Result<Url> {
        let u = Url::parse(s).map_err(|_| anyhow::anyhow!("无效的 Google 上传会话"))?;
        ensure!(
            u.origin() == self.upload.origin()
                && u.path().starts_with(self.upload.path())
                && u.username().is_empty()
                && u.password().is_none()
                && u.fragment().is_none(),
            "拒绝不可信的 Google 上传会话地址"
        );
        Ok(u)
    }
    fn fingerprint(source: &Path) -> anyhow::Result<String> {
        let mut f = File::open(source)?;
        let mut h = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            let n = f.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            h.update(&buffer[..n]);
        }
        Ok(format!("{:x}", h.finalize()))
    }
    fn upload_stream(
        &self,
        path: &str,
        source: &Path,
        remote_id: Option<&str>,
        version: Option<&str>,
        progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        self.check_write()?;
        let _lock = auth::lock(&self.state, "google-write.lock")?;
        let (parent, name) = self.destination(path)?;
        let current = self.resolve(path)?;
        let size = std::fs::metadata(source)?.len();
        progress(0, size)?;
        let fingerprint = Self::fingerprint(source)?;
        let key = format!(
            "google-intent-{:x}.json",
            Sha256::digest(format!("{}:{parent}:{path}", self.account_id))
        );
        let existing: Option<Intent> = if self.state.dir.join(&key).exists() {
            Some(self.state.read(&key)?)
        } else {
            None
        };
        // Persist a generated identity before the first upload. Ambiguous replies never create a second object.
        let mut intent = if let Some(intent) = existing {
            ensure!(
                intent.fingerprint == fingerprint,
                "待恢复上传的本地内容已改变；请先恢复原上传或选择新路径"
            );
            intent
        } else {
            let id = if let Some(id) = remote_id {
                valid_id(id)?;
                let (f, m) = current.as_ref().context("远端文件已移除，未覆盖")?;
                ensure!(
                    f.id == id && !m.is_dir && !f.native(),
                    "上传对象与路径不匹配，或目标为 Google 在线文档"
                );
                let version = version.context("覆盖必须指定远端版本")?;
                strong_etag(version)?;
                ensure!(f.etag == version, "远端版本发生冲突，未覆盖");
                id.to_string()
            } else {
                ensure!(
                    version.is_none()
                        && current.is_none()
                        && !self
                            .children(
                                &parent,
                                if path.rsplit_once('/').unwrap().0.is_empty() {
                                    "/"
                                } else {
                                    path.rsplit_once('/').unwrap().0
                                }
                            )?
                            .iter()
                            .any(|(f, _)| f.title == name),
                    "远端目标已存在，未覆盖"
                );
                let ids = self.json(
                    self.request(Method::GET, "files/generateIds")?
                        .query(&[("maxResults", "1"), ("space", "drive")]),
                    "分配上传 ID",
                )?;
                let id = ids["ids"][0].as_str().context("Google 未返回上传 ID")?;
                valid_id(id)?;
                id.into()
            };
            let i = Intent {
                id,
                fingerprint: fingerprint.clone(),
                session: None,
            };
            self.state.save(&key, &i)?;
            i
        };
        ensure!(
            remote_id.is_none_or(|id| id == intent.id),
            "待恢复上传属于另一对象"
        );
        // A previous final PUT may have committed despite its lost reply.
        if let Some(f) = self.file(&intent.id)? {
            ensure!(
                !f.native() && f.title == name && f.parents.iter().any(|p| p.id == parent),
                "待恢复上传对象已移动或更名"
            );
            if f.size.parse::<u64>().ok() == Some(size) {
                let mut digest = DigestWriter(Sha256::new());
                self.download_to(&intent.id, &mut digest, &mut |_| Ok(()))?;
                if format!("{:x}", digest.0.finalize()) == fingerprint {
                    std::fs::remove_file(self.state.dir.join(&key))?;
                    progress(size, size)?;
                    return f.metadata(path.into());
                }
            }
            if let Some(v) = version {
                ensure!(f.etag == v, "远端版本发生冲突，未覆盖");
            } else {
                anyhow::bail!("待恢复上传 ID 已存在不同内容，未覆盖");
            }
        }
        if intent.session.is_none() {
            let method = if remote_id.is_some() {
                Method::PUT
            } else {
                Method::POST
            };
            let relative = if remote_id.is_some() {
                format!("files/{}", intent.id)
            } else {
                "files".into()
            };
            let mut req=self.client.request(method,self.upload.join(&relative)?).bearer_auth(self.token()?)
                .query(&[("uploadType","resumable"),("fields",FIELDS)])
                .header("X-Upload-Content-Type","application/octet-stream").header("X-Upload-Content-Length",size)
                .json(&if remote_id.is_some(){json!({})}else{json!({"id":intent.id,"title":name,"parents":[{"id":parent}],"mimeType":"application/octet-stream"})});
            if let Some(v) = version {
                strong_etag(v)?;
                req = req.header("If-Match", v);
            }
            let response = self.send(req, "开始上传")?;
            let location = response
                .headers()
                .get("Location")
                .and_then(|h| h.to_str().ok())
                .context("Google 未返回上传会话")?;
            self.session_url(location)?;
            intent.session = Some(location.into());
            self.state.save(&key, &intent)?;
        }
        let session = self.session_url(intent.session.as_ref().unwrap())?;
        // Query the committed byte range before resuming, including after process restarts.
        let response = self
            .client
            .put(session.clone())
            .bearer_auth(self.token()?)
            .header("Content-Length", 0)
            .header("Content-Range", format!("bytes */{size}"))
            .send()
            .map_err(|_| anyhow::anyhow!("Google 上传状态连接失败；会话已保留"))?;
        let mut offset = if response.status().as_u16() == 308 {
            Self::offset(&response, size)?
        } else if response.status().is_success() {
            let f: DriveFile = serde_json::from_value(Self::read_json(response)?)
                .map_err(|_| anyhow::anyhow!("Google 上传结果无效"))?;
            ensure!(f.id == intent.id, "Google 上传结果 ID 不匹配");
            std::fs::remove_file(self.state.dir.join(&key))?;
            progress(size, size)?;
            return f.metadata(path.into());
        } else if matches!(response.status().as_u16(), 404 | 410) {
            intent.session = None;
            self.state.save(&key, &intent)?;
            anyhow::bail!("Google 上传会话已过期，已保留对象 ID；请重试");
        } else {
            return Err(GoogleError {
                operation: "查询上传进度",
                status: response.status().as_u16(),
                hint: "",
            }
            .into());
        };
        let mut file = File::open(source)?;
        file.seek(SeekFrom::Start(offset))?;
        loop {
            progress(offset, size)?;
            let n = (size - offset).min(CHUNK as u64) as usize;
            let mut bytes = vec![0; n];
            file.read_exact(&mut bytes)?;
            let range = if size == 0 {
                "bytes */0".to_string()
            } else {
                format!("bytes {}-{}/{size}", offset, offset + n as u64 - 1)
            };
            let mut req = self
                .client
                .put(session.clone())
                .bearer_auth(self.token()?)
                .header("Content-Range", range)
                .header("Content-Type", "application/octet-stream")
                .body(bytes);
            if let Some(v) = version {
                req = req.header("If-Match", v);
            }
            let response = req.send().map_err(|_| {
                anyhow::anyhow!("Google 上传连接中断；会话及本地数据已保留，重试可恢复")
            })?;
            if response.status().as_u16() == 308 {
                let next = Self::offset(&response, size)?;
                ensure!(
                    next == offset + n as u64 && next < size,
                    "Google 上传进度不一致"
                );
                offset = next;
                continue;
            }
            if !response.status().is_success() {
                return Err(GoogleError {
                    operation: "上传",
                    status: response.status().as_u16(),
                    hint: "；本地数据已保留",
                }
                .into());
            }
            let f: DriveFile = serde_json::from_value(Self::read_json(response)?)
                .map_err(|_| anyhow::anyhow!("Google 上传结果无效"))?;
            ensure!(
                f.id == intent.id && f.size.parse::<u64>().ok() == Some(size),
                "Google 上传结果 ID 或大小不一致"
            );
            ensure!(
                Self::fingerprint(source)? == fingerprint,
                "上传期间本地文件发生变化，请检查云端结果"
            );
            std::fs::remove_file(self.state.dir.join(&key))?;
            progress(size, size)?;
            return f.metadata(path.into());
        }
    }
    fn offset(response: &Response, size: u64) -> anyhow::Result<u64> {
        let offset = match response.headers().get("Range") {
            None => 0,
            Some(h) => h
                .to_str()
                .ok()
                .and_then(|s| s.strip_prefix("bytes=0-"))
                .and_then(|s| s.parse::<u64>().ok())
                .and_then(|v| v.checked_add(1))
                .context("Google 上传范围无效")?,
        };
        ensure!(offset <= size, "Google 上传范围超出本地文件");
        Ok(offset)
    }
}
struct DigestWriter(Sha256);
impl Write for DigestWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.update(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Provider for GoogleDrive {
    fn storage_usage(
        &self,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Option<StorageUsage>> {
        check()?;
        let value = self.json(
            self.request(Method::GET, "about")?.query(&[(
                "fields",
                "quotaType,quotaBytesTotal,quotaBytesUsedAggregate",
            )]),
            "读取容量",
        )?;
        check()?;
        let bytes = |key: &str| -> anyhow::Result<Option<u64>> {
            let Some(value) = value.get(key).filter(|v| !v.is_null()) else {
                return Ok(None);
            };
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|s| s.parse::<u64>().ok()))
                .context("Google 返回了无效的容量数值")
                .map(Some)
        };
        let total = if value["quotaType"] == "UNLIMITED" {
            None
        } else {
            bytes("quotaBytesTotal")?
        };
        // The account limit is shared with Gmail/Photos: Drive-only usage would
        // overstate free space, so never substitute quotaBytesUsed here.
        let used = bytes("quotaBytesUsedAggregate")?;
        ensure!(
            total.is_some() || used.is_some(),
            "Google 未返回账号容量信息"
        );
        Ok(StorageUsage::account(total, used, None))
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
        let (f, m) = self.resolve(path)?.context("Google 目录不存在")?;
        ensure!(m.is_dir, "目标不是目录");
        Ok(self
            .children(&f.id, path)?
            .into_iter()
            .map(|(_, m)| m)
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
        self.upload_stream(path, source, id, version, &mut |d, t| progress(d, t))
    }
}
impl CloudBackend for GoogleDrive {
    fn is_conflict_error(&self, e: &anyhow::Error) -> bool {
        e.downcast_ref::<GoogleError>()
            .is_some_and(|e| matches!(e.status, 409 | 412))
    }
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        self.snapshot()
    }
    fn list_delta(&self, cursor: Option<&str>) -> anyhow::Result<DeltaResult> {
        let entries = self.snapshot()?;
        let current: BTreeSet<_> = entries.iter().map(|m| m.remote_id.clone()).collect();
        let previous: BTreeSet<String> = match cursor {
            None => BTreeSet::new(),
            Some(c) => serde_json::from_str(
                c.strip_prefix("google-snapshot-v1:")
                    .context("无效的 Google 索引游标")?,
            )?,
        };
        Ok(DeltaResult {
            deleted_remote_ids: previous.difference(&current).cloned().collect(),
            delta_link: Some(format!(
                "google-snapshot-v1:{}",
                serde_json::to_string(&current)?
            )),
            entries,
        })
    }
    fn get_metadata(&self, id: &str) -> anyhow::Result<Option<MetadataEntry>> {
        Ok(self.snapshot()?.into_iter().find(|m| m.remote_id == id))
    }
    fn get_metadata_by_path(&self, path: &str) -> anyhow::Result<Option<MetadataEntry>> {
        Ok(self.resolve(path)?.map(|(_, m)| m))
    }
    fn download(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        let mut v = Vec::new();
        self.download_to(id, &mut v, &mut |_| Ok(()))?;
        Ok(v)
    }
    fn download_to(
        &self,
        id: &str,
        writer: &mut dyn Write,
        progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        let f = self.file(id)?.context("Google 文件不存在")?;
        ensure!(f.mime != FOLDER, "不能下载目录");
        if f.native() {
            let b = f.shortcut();
            writer.write_all(&b)?;
            progress(b.len() as u64)?;
            return Ok(b.len() as u64);
        }
        let mut r = self.send(
            self.request(Method::GET, &format!("files/{id}"))?
                .query(&[("alt", "media")]),
            "下载",
        )?;
        let mut buf = [0u8; 65536];
        let mut done = 0;
        progress(0)?;
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            writer.write_all(&buf[..n])?;
            done += n as u64;
            progress(done)?;
        }
        let after = self.file(id)?.context("下载期间 Google 文件被移除")?;
        ensure!(
            after.etag == f.etag,
            "下载期间 Google 文件发生变化，请重新下载"
        );
        ensure!(
            f.size.parse::<u64>().ok() == Some(done),
            "Google 下载长度不一致，文件可能已改变"
        );
        Ok(done)
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
        let mut f = tempfile::NamedTempFile::new()?;
        f.write_all(&bytes)?;
        let id = self.resolve(path)?.map(|(f, _)| f.id);
        self.upload_stream(path, f.path(), id.as_deref(), version, &mut |_, _| Ok(()))
    }
    fn upload_file_with_version(
        &self,
        path: &str,
        source: &Path,
        id: Option<&str>,
        version: Option<&str>,
        progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        self.upload_stream(path, source, id, version, progress)
    }
    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        self.check_write()?;
        let _lock = auth::lock(&self.state, "google-write.lock")?;
        let (parent, name) = self.destination(path)?;
        if let Some((_, m)) = self.resolve(path)? {
            ensure!(m.is_dir, "同名文件已存在");
            return Ok(m);
        }
        ensure!(
            !self
                .children(&parent, "/")?
                .iter()
                .any(|(f, _)| f.title == name),
            "存在同名 Google 项目，请使用列表中的独立路径"
        );
        let v = self.json(
            self.request(Method::POST, "files")?
                .query(&[("fields", FIELDS)])
                .json(&json!({"title":name,"mimeType":FOLDER,"parents":[{"id":parent}]})),
            "创建目录",
        )?;
        let f: DriveFile = serde_json::from_value(v)?;
        f.metadata(path.into())
    }
    fn rename(&self, id: &str, new_path: &str) -> anyhow::Result<MetadataEntry> {
        self.check_write()?;
        valid_id(id)?;
        ensure!(id != self.root, "不能移动根目录");
        let _lock = auth::lock(&self.state, "google-write.lock")?;
        let (parent, name) = self.destination(new_path)?;
        if let Some((_, target)) = self.resolve(new_path)? {
            ensure!(target.remote_id == id, "目标已存在，未覆盖");
            return Ok(target);
        }
        ensure!(
            !self
                .children(&parent, "/")?
                .iter()
                .any(|(f, _)| f.title == name),
            "存在同名 Google 项目，未覆盖"
        );
        let f = self.file(id)?.context("Google 文件不存在")?;
        strong_etag(&f.etag)?;
        let old = f
            .parents
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let v = self.json(
            self.request(Method::PATCH, &format!("files/{id}"))?
                .query(&[
                    ("fields", FIELDS),
                    ("addParents", parent.as_str()),
                    ("removeParents", old.as_str()),
                ])
                .header("If-Match", &f.etag)
                .json(&json!({"title":name})),
            "移动/重命名",
        )?;
        let f: DriveFile = serde_json::from_value(v)?;
        f.metadata(new_path.into())
    }
    fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.check_write()?;
        valid_id(id)?;
        ensure!(id != self.root, "不能删除根目录");
        let _lock = auth::lock(&self.state, "google-write.lock")?;
        let Some(f) = self.file(id)? else {
            return Ok(());
        };
        strong_etag(&f.etag)?;
        self.send(
            self.request(Method::POST, &format!("files/{id}/trash"))?
                .header("If-Match", f.etag)
                .header("Content-Length", 0),
            "移入回收站",
        )?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
