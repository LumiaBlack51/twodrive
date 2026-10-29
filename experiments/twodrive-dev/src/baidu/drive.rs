use super::{
    auth::{self, Session},
    http::{Api, errno},
};
use crate::{
    model::{CloudBackend, DeltaResult, MetadataEntry},
    provider::{Capabilities, Progress, Provider, StorageUsage, valid_path},
    state::State,
};
use anyhow::{Context, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashSet, VecDeque},
    fs::File,
    io::{Read, Write},
    path::Path,
};
use url::Url;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Snapshot {
    binding: String,
    id: u64,
    path: String,
    size: u64,
    directory: bool,
    md5: String,
}
pub struct BaiduDrive {
    state: State,
    api: Api,
    root: String,
    generation: String,
    pub binding: String,
    pub account_name: String,
    _auth_lock: File,
    writable: bool,
}
fn number(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}
fn content_md5(value: &str) -> anyhow::Result<[u8; 16]> {
    // Content responses may encode the digest as hexadecimal rather than
    // RFC 1864 Base64. Both must resolve to exactly the same 16 digest bytes.
    // A 32-character hex string is also valid Base64, but decodes to 24 bytes.
    let bytes = if value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        (0..32)
            .step_by(2)
            .map(|at| u8::from_str_radix(&value[at..at + 2], 16))
            .collect::<Result<Vec<_>, _>>()
            .context("百度内容摘要格式无效")?
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(value)
            .context("百度内容摘要格式无效")?
    };
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("百度内容摘要长度无效"))
}
fn clean_root(root: &str) -> anyhow::Result<String> {
    valid_path(root)?;
    Ok(root.into())
}
impl BaiduDrive {
    pub fn connect(
        state: State,
        root: &str,
        route: &str,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Self> {
        Self::connect_with(state, root, Api::new(route)?, check)
    }
    pub(super) fn connect_with(
        state: State,
        root: &str,
        api: Api,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Self> {
        let root = clean_root(root)?;
        let auth_lock = auth::lock(&state, "auth.lock", false)?;
        ensure!(
            !auth::login_required(&state.dir),
            "请先完成或取消百度登录，再连接"
        );
        let session = auth::load(&state)?;
        let binding = format!(
            "baidu-root-v1:{:x}",
            Sha256::digest(serde_json::to_vec(&(&session.generation, &root))?)
        );
        let backend = Self {
            state,
            api,
            root,
            // Authentication generation and storage identity serve different
            // purposes: a valid login can address multiple disjoint roots.
            binding,
            generation: session.generation,
            account_name: session.name,
            _auth_lock: auth_lock,
            writable: false,
        };
        let user = backend.get("/rest/2.0/xpan/nas", &[("method", "uinfo")], check)?;
        ensure!(
            number(&user["uk"]) == Some(session.uk),
            "百度账号身份发生变化，请重新授权，原缓存保留"
        );
        backend.list_directory_checked("/", check)?;
        Ok(backend)
    }
    pub(super) fn validate_root(
        api: &Api,
        session: &Session,
        root: &str,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        valid_path(root)?;
        let value = api.get(
            "/rest/2.0/xpan/file",
            &session.access_token,
            &[
                ("method", "list"),
                ("dir", root),
                ("start", "0"),
                ("limit", "1"),
                ("order", "name"),
            ],
            check,
        )?;
        errno(&value)?;
        ensure!(value["list"].is_array(), "百度未返回授权根目录，原登录保留");
        Ok(())
    }
    fn get(
        &self,
        path: &str,
        params: &[(&str, &str)],
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Value> {
        let token = auth::access(&self.state, &self.api, &self.generation, None, check)?;
        let mut value = self.api.get(path, &token, params, check)?;
        if matches!(
            value["errno"]
                .as_i64()
                .or_else(|| value["error_code"].as_i64()),
            Some(-6 | 110 | 20016 | 20017 | 31045)
        ) {
            let token = auth::access(
                &self.state,
                &self.api,
                &self.generation,
                Some(&token),
                check,
            )?;
            value = self.api.get(path, &token, params, check)?;
        }
        errno(&value)?;
        Ok(value)
    }
    fn remote(&self, path: &str) -> anyhow::Result<String> {
        valid_path(path)?;
        Ok(if self.root == "/" {
            path.into()
        } else if path == "/" {
            self.root.clone()
        } else {
            format!("{}{path}", self.root)
        })
    }
    fn virtual_path(&self, path: &str) -> anyhow::Result<String> {
        valid_path(path)?;
        if self.root == "/" {
            return Ok(path.into());
        }
        if path == self.root {
            return Ok("/".into());
        }
        path.strip_prefix(&format!("{}/", self.root))
            .map(|p| format!("/{p}"))
            .context("百度返回了授权根目录以外的文件")
    }
    fn snapshot(&self, row: &Value) -> anyhow::Result<Snapshot> {
        let path = self.virtual_path(row["path"].as_str().context("百度文件缺少路径")?)?;
        let directory = match row["isdir"].as_u64() {
            Some(0) => false,
            Some(1) => true,
            _ => anyhow::bail!("百度文件类型无效"),
        };
        let md5 = row["md5"].as_str().unwrap_or_default();
        ensure!(md5.len() <= 128, "百度文件摘要过长");
        Ok(Snapshot {
            binding: self.binding.clone(),
            id: number(&row["fs_id"])
                .filter(|n| *n > 0)
                .context("百度文件缺少稳定标识")?,
            path,
            size: if directory {
                0
            } else {
                row["size"].as_u64().context("百度文件大小无效")?
            },
            directory,
            md5: md5.into(),
        })
    }
    fn metadata(snapshot: Snapshot, modified: i64) -> anyhow::Result<MetadataEntry> {
        let bytes = serde_json::to_vec(&snapshot)?;
        let path = snapshot.path;
        let parent_path = path
            .rsplit_once('/')
            .map(|(p, _)| if p.is_empty() { "/" } else { p })
            .unwrap_or("/")
            .into();
        let name = path.rsplit('/').next().unwrap_or_default().into();
        Ok(MetadataEntry {
            remote_id: format!("baidu:{}", URL_SAFE_NO_PAD.encode(&bytes)),
            path,
            parent_path,
            name,
            is_dir: snapshot.directory,
            size: snapshot.size,
            modified_unix: modified,
            created_unix: None,
            etag: format!("{:x}", Sha256::digest(&bytes)),
        })
    }
    fn decode(&self, id: &str) -> anyhow::Result<Snapshot> {
        ensure!(id.len() <= 32768, "百度文件标识过长");
        let bytes = URL_SAFE_NO_PAD
            .decode(id.strip_prefix("baidu:").context("无效的百度文件标识")?)
            .context("无效的百度文件标识")?;
        let s: Snapshot = serde_json::from_slice(&bytes).context("无效的百度文件快照")?;
        ensure!(
            s.binding == self.binding,
            "百度文件属于另一账号或旧登录，拒绝读取"
        );
        self.remote(&s.path)?;
        Ok(s)
    }
    fn meta(
        &self,
        id: u64,
        download: bool,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Option<Value>> {
        let fsids = format!("[{id}]");
        let value = self.get(
            "/rest/2.0/xpan/multimedia",
            &[
                ("method", "filemetas"),
                ("fsids", &fsids),
                ("dlink", if download { "1" } else { "0" }),
            ],
            check,
        )?;
        let rows = value["list"].as_array().context("百度文件元信息无效")?;
        ensure!(rows.len() <= 1, "百度返回了多余的文件元信息");
        if let Some(row) = rows.first() {
            ensure!(
                number(&row["fs_id"]) == Some(id),
                "百度返回了其他文件的元信息"
            );
            Ok(Some(row.clone()))
        } else {
            Ok(None)
        }
    }
    fn stream(
        &self,
        s: &Snapshot,
        writer: &mut dyn Write,
        progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        let mut check = || progress(0);
        let before = self
            .meta(s.id, true, &mut check)?
            .context("百度文件已不存在")?;
        ensure!(
            self.snapshot(&before)? == *s,
            "百度文件版本已改变，请刷新目录后重试"
        );
        ensure!(
            !s.md5.is_empty(),
            "百度未提供文件版本摘要，拒绝不确定的下载"
        );
        let mut url = Url::parse(
            before["dlink"]
                .as_str()
                .context("当前百度应用未获准下载此文件")?,
        )
        .context("百度下载地址无效")?;
        ensure!(
            self.api.trusted_download(&url),
            "百度返回了未受信任的下载地址"
        );
        let token = auth::access(&self.state, &self.api, &self.generation, None, &mut check)?;
        let pairs: Vec<_> = url
            .query_pairs()
            .filter(|(k, _)| k != "access_token")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(pairs)
            .append_pair("access_token", &token);
        let mut seen = HashSet::new();
        let mut response = None;
        for _ in 0..6 {
            progress(0)?;
            ensure!(
                self.api.trusted_download(&url) && seen.insert(url.clone()),
                "百度下载重定向越界或循环"
            );
            let r = self
                .api
                .client
                .get(url.clone())
                .send()
                .map_err(|_| anyhow::anyhow!("百度下载连接失败，未发布半文件"))?;
            if r.status().is_redirection() {
                let next = r
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .context("百度下载重定向缺少地址")?;
                url = url.join(next).context("百度下载重定向无效")?;
                continue;
            }
            ensure!(
                r.status() == reqwest::StatusCode::OK,
                "百度下载未获准或暂不可用（HTTP {}）",
                r.status().as_u16()
            );
            response = Some(r);
            break;
        }
        let mut response = response.context("百度下载重定向超过预算")?;
        if let Some(length) = response.content_length() {
            ensure!(length == s.size, "百度下载大小与文件快照不一致");
        }
        // Baidu metadata MD5 is opaque. Check Content-MD5 only when the content
        // response explicitly supplies it; metadata equality is checked twice.
        let expected = response
            .headers()
            .get("Content-MD5")
            .map(|v| v.to_str().map(str::to_owned))
            .transpose()
            .context("百度内容摘要无效")?
            .as_deref()
            .map(content_md5)
            .transpose()?;
        let mut digest = md5::Md5::new();
        let mut count = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            progress(count)?;
            let n = response
                .read(&mut buffer)
                .map_err(|_| anyhow::anyhow!("百度下载中断，未发布半文件"))?;
            if n == 0 {
                break;
            }
            count = count.checked_add(n as u64).context("下载长度溢出")?;
            ensure!(count <= s.size, "百度下载超过声明大小");
            digest.update(&buffer[..n]);
            writer.write_all(&buffer[..n])?;
        }
        ensure!(count == s.size, "百度下载不完整，未发布半文件");
        if let Some(expected) = expected {
            ensure!(
                expected.as_slice() == digest.finalize().as_slice(),
                "百度下载内容校验失败，未发布文件"
            );
        }
        let after = self
            .meta(s.id, false, &mut || progress(count))?
            .context("百度文件在下载时被移除")?;
        ensure!(
            self.snapshot(&after)? == *s,
            "百度文件在下载期间改变，未发布文件"
        );
        progress(count)?;
        Ok(count)
    }
}
impl Provider for BaiduDrive {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            upload: self.writable,
            conditional_upload: false,
            mkdir: self.writable,
            rename: self.writable,
            delete: self.writable,
        }
    }
    fn manual_upload_supported(&self) -> bool {
        self.writable
    }
    fn manual_upload(
        &self,
        path: &str,
        source: &Path,
        id: Option<&str>,
        version: Option<&str>,
        progress: Progress,
    ) -> anyhow::Result<MetadataEntry> {
        self.put_manual(path, source, id, version, progress)
    }
    fn storage_usage(
        &self,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Option<StorageUsage>> {
        let v = self.get(
            "/api/quota",
            &[("checkfree", "0"), ("checkexpire", "0")],
            check,
        )?;
        // The API's `free` is free-plan quota, not remaining bytes.
        Ok(StorageUsage::account(
            v["total"].as_u64(),
            v["used"].as_u64(),
            None,
        ))
    }
    fn list_directory(&self, path: &str) -> anyhow::Result<Vec<MetadataEntry>> {
        self.list_directory_checked(path, &mut || Ok(()))
    }
    fn list_directory_checked(
        &self,
        path: &str,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Vec<MetadataEntry>> {
        let remote = self.remote(path)?;
        let mut result = Vec::new();
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for page in 0..100 {
            let start = (page * 1000).to_string();
            let v = self.get(
                "/rest/2.0/xpan/file",
                &[
                    ("method", "list"),
                    ("dir", &remote),
                    ("start", &start),
                    ("limit", "1000"),
                    ("order", "name"),
                    ("desc", "0"),
                    ("web", "0"),
                ],
                check,
            )?;
            let rows = v["list"].as_array().context("百度目录响应缺少文件列表")?;
            ensure!(rows.len() <= 1000, "百度分页响应超出预算");
            for row in rows {
                let snapshot = self.snapshot(row)?;
                let metadata =
                    Self::metadata(snapshot.clone(), row["server_mtime"].as_i64().unwrap_or(0))?;
                ensure!(
                    metadata.parent_path == path
                        && ids.insert(snapshot.id)
                        && names.insert(metadata.name.clone()),
                    "百度目录含重复或越界条目，未发布不完整结果"
                );
                if let Some(name) = row["server_filename"].as_str() {
                    ensure!(name == metadata.name, "百度文件名与路径不一致");
                }
                result.push(metadata);
            }
            if rows.len() < 1000 {
                return Ok(result);
            }
        }
        anyhow::bail!("百度目录超过分页预算，未发布不完整结果")
    }
    fn upload_controlled(
        &self,
        _: &str,
        _: &Path,
        _: Option<&str>,
        _: Option<&str>,
        _: Progress,
    ) -> anyhow::Result<MetadataEntry> {
        anyhow::bail!("百度网盘当前只读，未启用安全条件写入")
    }
}
impl CloudBackend for BaiduDrive {
    fn is_conflict_error(&self, _: &anyhow::Error) -> bool {
        false
    }
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        let mut queue = VecDeque::from(["/".to_owned()]);
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        while let Some(path) = queue.pop_front() {
            ensure!(
                seen.insert(path.clone()) && seen.len() <= 10000,
                "百度目录遍历超出预算"
            );
            for row in self.list_directory(&path)? {
                if row.is_dir {
                    queue.push_back(row.path.clone());
                }
                out.push(row);
                ensure!(out.len() <= 100000, "百度索引超过预算，未发布不完整结果");
            }
        }
        Ok(out)
    }
    fn list_delta(&self, _: Option<&str>) -> anyhow::Result<DeltaResult> {
        anyhow::bail!("百度只读适配器尚未提供可靠增量流")
    }
    fn get_metadata(&self, id: &str) -> anyhow::Result<Option<MetadataEntry>> {
        let snapshot = self.decode(id)?;
        self.meta(snapshot.id, false, &mut || Ok(()))?
            .map(|v| Self::metadata(self.snapshot(&v)?, v["server_mtime"].as_i64().unwrap_or(0)))
            .transpose()
    }
    fn get_metadata_by_path(&self, path: &str) -> anyhow::Result<Option<MetadataEntry>> {
        valid_path(path)?;
        if path == "/" {
            return Ok(Some(MetadataEntry::new_dir(
                "baidu-root",
                "/",
                0,
                self.binding.clone(),
            )));
        }
        let parent = path
            .rsplit_once('/')
            .map(|(p, _)| if p.is_empty() { "/" } else { p })
            .unwrap();
        Ok(self
            .list_directory(parent)?
            .into_iter()
            .find(|e| e.path == path))
    }
    fn download(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        let s = self.decode(id)?;
        ensure!(s.size <= 16 * 1024 * 1024, "请使用流式下载接口读取大文件");
        let mut out = Vec::new();
        self.stream(&s, &mut out, &mut |_| Ok(()))?;
        Ok(out)
    }
    fn download_to(
        &self,
        id: &str,
        writer: &mut dyn Write,
        progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        let s = self.decode(id)?;
        ensure!(!s.directory, "不能下载目录");
        self.stream(&s, writer, progress)
    }
    fn upload(&self, _: &str, _: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        anyhow::bail!("百度网盘当前只读")
    }
    fn upload_with_etag(
        &self,
        _: &str,
        _: Vec<u8>,
        _: Option<&str>,
    ) -> anyhow::Result<MetadataEntry> {
        anyhow::bail!("百度网盘当前只读")
    }
    fn upload_file_with_version(
        &self,
        _: &str,
        _: &Path,
        _: Option<&str>,
        _: Option<&str>,
        _: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        anyhow::bail!("百度网盘当前只读")
    }
    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        self.mkdir_manual(path)
    }
    fn rename(&self, id: &str, path: &str) -> anyhow::Result<MetadataEntry> {
        self.rename_manual(id, path)
    }
    fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.delete_manual(id)
    }
}

#[path = "write.rs"]
mod write;
