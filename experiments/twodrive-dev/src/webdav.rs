use crate::model::{CloudBackend, DeltaResult, MetadataEntry};
use crate::state::Credentials;
use anyhow::{Context, ensure};
use percent_encoding::percent_decode_str;
use reqwest::{
    Method, StatusCode,
    blocking::{Client, RequestBuilder, Response},
};
use roxmltree::Node;
use std::{
    collections::{BTreeSet, VecDeque},
    fs::File,
    io::{Read, Seek, Write},
    path::Path,
    time::Duration,
};
use url::{Host, Url};

const MAX_XML: u64 = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 100_000;
const PROPFIND: &str = "<?xml version=\"1.0\"?><d:propfind xmlns:d=\"DAV:\"><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/><d:getetag/></d:prop></d:propfind>";

#[derive(Debug, thiserror::Error)]
#[error("WebDAV {operation} returned HTTP {status}")]
pub struct DavError {
    pub operation: String,
    pub status: u16,
}

/// Format-independent transfer policy; frontends may force full upload without
/// implementing hashing or patch application. No file-extension allowlist.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UploadMode {
    #[default]
    Auto,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadKind {
    Full,
    Delta,
}

pub struct UploadOutcome {
    pub entry: MetadataEntry,
    pub kind: UploadKind,
    /// Committed request body bytes, excluding HTTP/QUIC framing and prior attempts.
    pub uploaded_bytes: u64,
    pub reused_bytes: u64,
    pub signature_bytes: u64,
}

struct UploadReader {
    file: File,
    done: u64,
    total: u64,
    progress: crate::provider::Progress,
    report_total: u64,
    report_start: u64,
    report_span: u64,
    paused: Option<std::path::PathBuf>,
}
impl Read for UploadReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.paused.as_ref().is_some_and(|p| p.exists()) {
            return Err(std::io::Error::other("upload paused"));
        }
        let done = self.report_start
            + ((self.done.min(self.total) as u128 * self.report_span as u128)
                / self.total.max(1) as u128) as u64;
        (self.progress)(done, self.report_total).map_err(std::io::Error::other)?;
        let count = self.file.read(buffer)?;
        self.done += count as u64;
        Ok(count)
    }
}
fn delta_unsupported(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<DavError>()
        .is_some_and(|e| matches!(e.status, 405 | 415 | 501))
}

#[derive(Clone)]
pub struct WebDavBackend {
    client: Client,
    base: Url,
    credentials: Option<Credentials>,
    writable: bool,
    connector: Option<std::sync::Arc<crate::network::HttpConnector>>,
    pause_file: Option<std::path::PathBuf>,
    route: String,
    upload_mode: UploadMode,
}

fn valid_path(path: &str) -> anyhow::Result<()> {
    ensure!(
        path.starts_with('/') && path.trim() == path && !path.contains(['\\', '\0']),
        "invalid WebDAV resource path"
    );
    ensure!(
        path == "/"
            || path
                .trim_matches('/')
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."),
        "invalid WebDAV resource components"
    );
    Ok(())
}

impl WebDavBackend {
    pub fn new(
        base: &str,
        credentials: Option<Credentials>,
        writable: bool,
    ) -> anyhow::Result<Self> {
        Self::new_with_options(base, credentials, writable, false)
    }
    pub fn new_with_options(
        base: &str,
        credentials: Option<Credentials>,
        writable: bool,
        allow_private_http: bool,
    ) -> anyhow::Result<Self> {
        let mut base = Url::parse(base).context("invalid WebDAV root URL")?;
        let loopback = match base.host() {
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        let private = matches!(base.host(), Some(Host::Ipv4(ip)) if ip.is_private() || ip.is_link_local())
            || matches!(base.host(), Some(Host::Ipv6(ip)) if ip.is_unique_local());
        ensure!(
            base.scheme() == "https"
                || (base.scheme() == "http" && (loopback || (allow_private_http && private))),
            "WebDAV requires HTTPS; unencrypted LAN HTTP requires explicit permission and a numeric private IP"
        );
        ensure!(
            base.username().is_empty()
                && base.password().is_none()
                && base.query().is_none()
                && base.fragment().is_none(),
            "put credentials in a private JSON file; root URL must not contain credentials, query or fragment"
        );
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(3600))
            .no_proxy()
            .build()?;
        Ok(Self {
            client,
            base,
            credentials,
            writable,
            connector: None,
            pause_file: None,
            route: "direct".into(),
            upload_mode: UploadMode::Auto,
        })
    }

    pub fn routed(
        base: &str,
        credentials: Option<Credentials>,
        writable: bool,
        route: &str,
    ) -> anyhow::Result<Self> {
        Self::routed_with_options(base, credentials, writable, route, false)
    }
    pub fn routed_with_options(
        base: &str,
        credentials: Option<Credentials>,
        writable: bool,
        route: &str,
        allow_private_http: bool,
    ) -> anyhow::Result<Self> {
        ensure!(
            matches!(route, "direct" | "system"),
            "invalid network route"
        );
        let mut result = Self::new_with_options(base, credentials, writable, allow_private_http)?;
        result.route = route.into();
        // Numeric loopback is always private, including paired local gateways.
        if result.base.scheme() == "http"
            || matches!(result.base.host(), Some(Host::Ipv4(ip)) if ip.is_loopback())
            || matches!(result.base.host(), Some(Host::Ipv6(ip)) if ip.is_loopback())
        {
            return Ok(result);
        }
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(3600));
        if route == "direct" {
            let connector = std::sync::Arc::new(crate::network::HttpConnector::direct()?);
            builder = builder.no_proxy().proxy(connector.proxy.clone());
            result.connector = Some(connector);
        }
        result.client = builder.build()?;
        Ok(result)
    }
    pub fn route(&self) -> &str {
        &self.route
    }
    pub fn with_pause_file(mut self, file: std::path::PathBuf) -> Self {
        self.pause_file = Some(file);
        self
    }
    fn check_pause(&self) -> anyhow::Result<()> {
        ensure!(
            !self.pause_file.as_ref().is_some_and(|p| p.exists()),
            "同步已暂停；待传输数据保留在本机"
        );
        Ok(())
    }
    pub fn root_url(&self) -> &str {
        self.base.as_str()
    }

    /// Browse just one collection, without recursively scanning a whole share.
    pub fn list_directory(&self, path: &str) -> anyhow::Result<Vec<MetadataEntry>> {
        let path = if path == "/" {
            path
        } else {
            path.trim_end_matches('/')
        };
        let entries = self.propfind(path, 1)?;
        ensure!(
            entries
                .iter()
                .any(|entry| entry.path == path && entry.is_dir),
            "WebDAV collection not found"
        );
        let mut children: Vec<_> = entries
            .into_iter()
            .filter(|entry| entry.path != path && entry.parent_path == path)
            .collect();
        children.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
        Ok(children)
    }

    /// Streaming desktop upload with bounded memory and cooperative cancellation.
    pub fn upload_controlled(
        &self,
        path: &str,
        source: &Path,
        version: Option<&str>,
        progress: crate::provider::Progress,
    ) -> anyhow::Result<MetadataEntry> {
        self.upload_controlled_with_mode(path, source, version, self.upload_mode, progress)
            .map(|outcome| outcome.entry)
    }

    /// Backend policy exposed to frontends; Auto negotiates with the receiving
    /// peer, Full always retains the original conditional streaming PUT.
    pub fn with_upload_mode(mut self, mode: UploadMode) -> Self {
        self.upload_mode = mode;
        self
    }

    pub fn upload_controlled_with_mode(
        &self,
        path: &str,
        source: &Path,
        version: Option<&str>,
        mode: UploadMode,
        progress: crate::provider::Progress,
    ) -> anyhow::Result<UploadOutcome> {
        // Validate permissions and conditional tokens before any network I/O.
        let full = self.versioned_put(path, version)?;
        let file = File::open(source)?;
        ensure!(
            file.metadata()?.is_file(),
            "upload source must be a regular file"
        );
        let total = file.metadata()?.len();
        progress(0, total)?;
        let mut reader = UploadReader {
            file,
            done: 0,
            total,
            progress: progress.clone(),
            report_total: total,
            report_start: 0,
            report_span: total,
            paused: self.pause_file.clone(),
        };
        let mut signature_bytes = 0;
        if mode == UploadMode::Auto
            && total >= 128 * 1024
            && let Some(version) = version
            && let Some(stats) =
                self.try_delta_upload(path, version, &mut reader, &mut signature_bytes)?
        {
            return Ok(UploadOutcome {
                entry: self
                    .get_metadata_by_path(path)?
                    .context("uploaded DAV resource not found")?,
                kind: UploadKind::Delta,
                uploaded_bytes: stats.wire,
                reused_bytes: stats.reused,
                signature_bytes,
            });
        }
        reader.file.rewind()?;
        if reader.done > 0 && reader.report_start < total {
            reader.report_start = total / 2;
            reader.report_span = total - total / 2;
        }
        reader.done = 0;
        self.send(
            "PUT",
            full.body(reqwest::blocking::Body::sized(reader, total)),
        )?;
        // A late cancellation cannot undo a write the server already committed.
        Ok(UploadOutcome {
            entry: self
                .get_metadata_by_path(path)?
                .context("uploaded DAV resource not found")?,
            kind: UploadKind::Full,
            uploaded_bytes: total,
            reused_bytes: 0,
            signature_bytes,
        })
    }

    fn try_delta_upload(
        &self,
        path: &str,
        version: &str,
        reader: &mut UploadReader,
        signature_bytes: &mut u64,
    ) -> anyhow::Result<Option<crate::delta::Stats>> {
        use crate::delta;
        let options = match self.send("OPTIONS", self.request("OPTIONS", path)?) {
            Ok(response) => response,
            Err(error) if delta_unsupported(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        if options
            .headers()
            .get("Accept-Patch")
            .and_then(|v| v.to_str().ok())
            != Some(delta::MEDIA_TYPE)
        {
            return Ok(None);
        }
        drop(options);
        let response = match self.send(
            delta::SIGNATURE_METHOD,
            self.request(delta::SIGNATURE_METHOD, path)?
                .header("If-Match", version),
        ) {
            Ok(response) => response,
            Err(error) if delta_unsupported(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        // Negotiate a versioned, bounded format; an old/generic DAV service may
        // respond successfully to unknown methods without implementing delta.
        if response
            .headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok())
            != Some(delta::MEDIA_TYPE)
            || response.headers().get("ETag").and_then(|v| v.to_str().ok()) != Some(version)
        {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        response
            .take(delta::MAX_SIGNATURE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        *signature_bytes = bytes.len() as u64;
        if bytes.len() as u64 > delta::MAX_SIGNATURE_BYTES {
            return Ok(None);
        }
        let Ok(signature) = serde_json::from_slice::<delta::Signature>(&bytes) else {
            return Ok(None);
        };
        if signature.validate().is_err() {
            return Ok(None);
        }
        let mut patch = tempfile::tempfile()?;
        reader.report_span = reader.total / 2;
        let stats = delta::encode(reader, &signature, &mut patch)?;
        ensure!(
            stats.size == reader.total,
            "upload source changed length while preparing delta"
        );
        // Include the reverse-direction signature in the bandwidth decision.
        if stats.wire.saturating_add(bytes.len() as u64) >= reader.total {
            return Ok(None);
        }
        self.check_pause()?;
        (reader.progress)(reader.total / 2, reader.total)?;
        let patch = UploadReader {
            file: patch,
            done: 0,
            total: stats.wire,
            progress: reader.progress.clone(),
            report_total: reader.total,
            report_start: reader.total / 2,
            report_span: reader.total - reader.total / 2,
            paused: reader.paused.clone(),
        };
        let result = self.send(
            "PATCH",
            self.request("PATCH", path)?
                .header("If-Match", version)
                .header("Content-Type", delta::MEDIA_TYPE)
                .body(reqwest::blocking::Body::sized(patch, stats.wire)),
        );
        match result {
            Ok(_) => Ok(Some(stats)),
            // These replies guarantee the delta was rejected without a commit.
            // A transport error/5xx might follow a commit and must never replay.
            Err(error)
                if delta_unsupported(&error)
                    || error
                        .downcast_ref::<DavError>()
                        .is_some_and(|e| e.status == 422) =>
            {
                reader.report_start = reader.total;
                reader.report_span = 0;
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn url(&self, path: &str) -> anyhow::Result<Url> {
        valid_path(path)?;
        let mut url = self.base.clone();
        if path != "/" {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| anyhow::anyhow!("invalid base URL"))?;
            segments.pop_if_empty();
            for part in path.trim_matches('/').split('/') {
                segments.push(part);
            }
        }
        Ok(url)
    }

    fn request(&self, method: &str, path: &str) -> anyhow::Result<RequestBuilder> {
        let mut request = self
            .client
            .request(Method::from_bytes(method.as_bytes())?, self.url(path)?);
        if let Some(auth) = &self.credentials {
            request = request.basic_auth(&auth.username, Some(&auth.password));
        }
        Ok(request)
    }

    fn send(&self, operation: &str, mut request: RequestBuilder) -> anyhow::Result<Response> {
        // Only replay safe reads before any response body reaches the caller.
        let safe = matches!(operation, "GET" | "HEAD" | "PROPFIND");
        let mut attempts = 0;
        loop {
            self.check_pause()?;
            let retry = if safe && attempts < 2 {
                request.try_clone()
            } else {
                None
            };
            let response = request.send();
            let transient = response.as_ref().map_or_else(
                |error| error.is_connect() || error.is_timeout(),
                |response| matches!(response.status().as_u16(), 502..=504),
            );
            if transient && let Some(next) = retry {
                drop(response);
                attempts += 1;
                std::thread::sleep(Duration::from_millis(200 * attempts));
                request = next;
                continue;
            }
            let response = response.map_err(|error| {
                anyhow::anyhow!(
                    "WebDAV {operation} transport failed (timeout: {}, connect: {})",
                    error.is_timeout(),
                    error.is_connect()
                )
            })?;
            if !response.status().is_success() {
                return Err(DavError {
                    operation: operation.into(),
                    status: response.status().as_u16(),
                }
                .into());
            }
            return Ok(response);
        }
    }

    fn href_path(&self, href: &str, request_path: &str) -> anyhow::Result<String> {
        let url = self.url(request_path)?.join(href)?;
        ensure!(
            url.origin() == self.base.origin()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "PROPFIND href leaves configured origin"
        );
        let suffix = if url.path() == self.base.path().trim_end_matches('/') {
            ""
        } else {
            url.path()
                .strip_prefix(self.base.path())
                .ok_or_else(|| anyhow::anyhow!("PROPFIND href leaves configured root"))?
        };
        let mut parts = Vec::new();
        for part in suffix
            .trim_end_matches('/')
            .split('/')
            .filter(|part| !part.is_empty())
        {
            let decoded = percent_decode_str(part).decode_utf8()?;
            ensure!(
                !decoded.contains(['/', '\\', '\0']),
                "encoded separator in PROPFIND href"
            );
            parts.push(decoded.into_owned());
        }
        let path = format!("/{}", parts.join("/"));
        valid_path(&path)?;
        Ok(path)
    }

    fn propfind(&self, path: &str, depth: u8) -> anyhow::Result<Vec<MetadataEntry>> {
        let response = self.send(
            "PROPFIND",
            self.request("PROPFIND", path)?
                .header("Depth", depth.to_string())
                .header("Content-Type", "application/xml; charset=utf-8")
                .body(PROPFIND),
        )?;
        ensure!(
            response.status() == StatusCode::MULTI_STATUS,
            "PROPFIND must return HTTP 207"
        );
        let mut xml = String::new();
        response.take(MAX_XML + 1).read_to_string(&mut xml)?;
        ensure!(
            xml.len() as u64 <= MAX_XML,
            "PROPFIND response exceeds metadata limit"
        );
        let document = roxmltree::Document::parse(&xml)?;
        ensure!(
            is_dav(document.root_element(), "multistatus"),
            "invalid DAV multistatus"
        );
        let mut entries = Vec::new();
        let mut seen = BTreeSet::new();
        for response in document
            .root_element()
            .children()
            .filter(|node| is_dav(*node, "response"))
        {
            let href = child_text(response, "href").context("missing DAV href")?;
            let entry_path = self.href_path(href, path)?;
            ensure!(
                seen.insert(entry_path.clone()),
                "duplicate PROPFIND resource"
            );
            let mut properties = Vec::new();
            for propstat in response.children().filter(|node| is_dav(*node, "propstat")) {
                let status = child_text(propstat, "status").context("missing propstat status")?;
                if status.split_whitespace().nth(1) == Some("200") {
                    properties.extend(
                        propstat
                            .children()
                            .filter(|node| is_dav(*node, "prop"))
                            .flat_map(|prop| prop.children())
                            .filter(Node::is_element),
                    );
                }
            }
            let property = |name| properties.iter().find(|node| is_dav(**node, name)).copied();
            let resource_type = property("resourcetype").context(
                "PROPFIND missing successful resourcetype; refusing incomplete snapshot",
            )?;
            let is_dir = resource_type
                .children()
                .any(|node| is_dav(node, "collection"));
            let size = if is_dir {
                0
            } else {
                property("getcontentlength")
                    .and_then(|node| node.text())
                    .context("missing content length")?
                    .parse()?
            };
            let modified = property("getlastmodified")
                .and_then(|node| node.text())
                .and_then(|value| httpdate::parse_http_date(value).ok())
                .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |value| value.as_secs() as i64);
            let etag = property("getetag")
                .and_then(|node| node.text())
                .unwrap_or("");
            // Some DAV implementations (including dav-server's local metadata
            // adapter) emit an unquoted XML value although HTTP ETags are quoted.
            // Preserve quoted/weak tokens; normalize only the unquoted form.
            let etag = if etag.is_empty() || etag.starts_with('"') || etag.starts_with("W/") {
                etag.to_owned()
            } else {
                format!("\"{etag}\"")
            };
            entries.push(if is_dir {
                MetadataEntry::new_dir(&entry_path, &entry_path, modified, &etag)
            } else {
                MetadataEntry::new_file(&entry_path, &entry_path, size, modified, &etag)
            });
        }
        ensure!(!entries.is_empty(), "empty PROPFIND snapshot");
        Ok(entries)
    }

    fn check_write(&self) -> anyhow::Result<()> {
        ensure!(
            self.writable,
            "WebDAV backend is read-only; enable --write explicitly"
        );
        Ok(())
    }
    fn versioned_put(&self, path: &str, if_match: Option<&str>) -> anyhow::Result<RequestBuilder> {
        self.check_write()?;
        let request = self.request("PUT", path)?;
        Ok(if let Some(etag) = if_match {
            ensure!(
                !etag.is_empty(),
                "server has no ETag; cannot safely overwrite"
            );
            ensure!(
                etag.starts_with('"')
                    && etag.ends_with('"')
                    && etag.len() >= 2
                    && !etag[1..etag.len() - 1].contains(['"', '\r', '\n']),
                "conditional writes require one quoted strong ETag"
            );
            request.header("If-Match", etag)
        } else {
            request.header("If-None-Match", "*")
        })
    }
}

fn is_dav(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element()
        && node.tag_name().namespace() == Some("DAV:")
        && node.tag_name().name() == name
}
fn child_text<'a>(node: Node<'a, 'a>, name: &str) -> Option<&'a str> {
    node.children()
        .find(|node| is_dav(*node, name))
        .and_then(|node| node.text())
}

impl CloudBackend for WebDavBackend {
    fn is_conflict_error(&self, error: &anyhow::Error) -> bool {
        error
            .downcast_ref::<DavError>()
            .is_some_and(|error| matches!(error.status, 409 | 412 | 423))
    }
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        let mut queue = VecDeque::from(["/".to_owned()]);
        let mut visited = BTreeSet::new();
        let mut all = Vec::new();
        while let Some(parent) = queue.pop_front() {
            ensure!(visited.insert(parent.clone()), "cyclic DAV tree");
            for entry in self.propfind(&parent, 1)? {
                if entry.path == parent {
                    continue;
                }
                ensure!(
                    entry.parent_path == parent,
                    "Depth:1 returned a non-child resource"
                );
                if entry.is_dir {
                    queue.push_back(entry.path.clone());
                }
                all.push(entry);
                ensure!(
                    all.len() <= MAX_ENTRIES,
                    "WebDAV tree exceeds metadata limit"
                );
            }
        }
        Ok(all)
    }
    fn list_delta(&self, cursor: Option<&str>) -> anyhow::Result<DeltaResult> {
        let entries = self.list_all()?;
        let current: BTreeSet<_> = entries
            .iter()
            .map(|entry| entry.remote_id.clone())
            .collect();
        let previous: BTreeSet<String> = if let Some(cursor) = cursor {
            serde_json::from_str(
                cursor
                    .strip_prefix("dav-snapshot-v1:")
                    .context("invalid WebDAV snapshot cursor")?,
            )?
        } else {
            BTreeSet::new()
        };
        Ok(DeltaResult {
            entries,
            deleted_remote_ids: previous.difference(&current).cloned().collect(),
            delta_link: Some(format!(
                "dav-snapshot-v1:{}",
                serde_json::to_string(&current)?
            )),
        })
    }
    fn get_metadata(&self, id: &str) -> anyhow::Result<Option<MetadataEntry>> {
        self.get_metadata_by_path(id)
    }
    fn get_metadata_by_path(&self, path: &str) -> anyhow::Result<Option<MetadataEntry>> {
        match self.propfind(path, 0) {
            Ok(entries) => Ok(entries.into_iter().find(|entry| entry.path == path)),
            Err(error)
                if error
                    .downcast_ref::<DavError>()
                    .is_some_and(|error| error.status == 404) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
    fn download(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.download_to(id, &mut bytes, &mut |_| Ok(()))?;
        Ok(bytes)
    }
    fn download_to(
        &self,
        id: &str,
        writer: &mut dyn Write,
        progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        let mut response = self.send("GET", self.request("GET", id)?)?;
        let mut buffer = vec![0; 256 * 1024];
        let mut total = 0;
        loop {
            let count = response.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            writer.write_all(&buffer[..count])?;
            total += count as u64;
            progress(total)?;
        }
        Ok(total)
    }
    fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        self.upload_with_etag(path, content, None)
    }
    fn upload_with_etag(
        &self,
        path: &str,
        content: Vec<u8>,
        if_match: Option<&str>,
    ) -> anyhow::Result<MetadataEntry> {
        if if_match.is_none() || content.len() < 128 * 1024 || self.upload_mode == UploadMode::Full
        {
            self.send("PUT", self.versioned_put(path, if_match)?.body(content))?;
            return self
                .get_metadata_by_path(path)?
                .context("uploaded DAV resource not found");
        }
        let mut source = tempfile::NamedTempFile::new()?;
        source.write_all(&content)?;
        self.upload_controlled(
            path,
            source.path(),
            if_match,
            std::sync::Arc::new(|_, _| Ok(())),
        )
    }

    fn upload_file_with_version(
        &self,
        path: &str,
        source: &Path,
        _id: Option<&str>,
        if_match: Option<&str>,
        progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
            mpsc,
        };
        let total = File::open(source)?.metadata()?.len();
        progress(0, total)?;
        let done = Arc::new(AtomicU64::new(0));
        let cancelled = Arc::new(AtomicBool::new(false));
        std::thread::scope(|scope| {
            let (tx, rx) = mpsc::channel();
            let bytes = done.clone();
            let stop = cancelled.clone();
            scope.spawn(move || {
                let result = self.upload_controlled(
                    path,
                    source,
                    if_match,
                    Arc::new(move |n, _| {
                        ensure!(!stop.load(Ordering::Relaxed), "上传已取消");
                        bytes.store(n, Ordering::Relaxed);
                        Ok(())
                    }),
                );
                let _ = tx.send(result);
            });
            let mut cancellation = None;
            loop {
                match rx.recv_timeout(Duration::from_millis(150)) {
                    Ok(result) => {
                        if let Some(error) = cancellation {
                            return Err(error);
                        }
                        let entry = result?;
                        progress(total, total)?;
                        return Ok(entry);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if cancellation.is_none()
                            && let Err(error) = progress(done.load(Ordering::Relaxed), total)
                        {
                            cancelled.store(true, Ordering::Relaxed);
                            cancellation = Some(error);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        anyhow::bail!("上传工作线程已退出")
                    }
                }
            }
        })
    }
    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        self.check_write()?;
        if let Err(error) = self.send("MKCOL", self.request("MKCOL", path)?)
            && error
                .downcast_ref::<DavError>()
                .is_none_or(|error| error.status != 405)
        {
            return Err(error);
        }
        let entry = self
            .get_metadata_by_path(path)?
            .context("DAV collection not found")?;
        ensure!(entry.is_dir, "MKCOL destination is not a directory");
        Ok(entry)
    }
    fn rename(&self, id: &str, path: &str) -> anyhow::Result<MetadataEntry> {
        self.check_write()?;
        self.send(
            "MOVE",
            self.request("MOVE", id)?
                .header("Destination", self.url(path)?.as_str())
                .header("Overwrite", "F"),
        )?;
        self.get_metadata_by_path(path)?
            .context("moved DAV resource not found")
    }
    fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.check_write()?;
        match self.send("DELETE", self.request("DELETE", id)?) {
            Ok(_) => Ok(()),
            Err(error)
                if error
                    .downcast_ref::<DavError>()
                    .is_some_and(|error| error.status == 404) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

impl crate::provider::Provider for WebDavBackend {
    fn capabilities(&self) -> crate::provider::Capabilities {
        crate::provider::Capabilities {
            upload: self.writable,
            mkdir: self.writable,
            rename: self.writable,
            delete: self.writable,
            conditional_upload: true,
        }
    }
    fn list_directory(&self, path: &str) -> anyhow::Result<Vec<MetadataEntry>> {
        WebDavBackend::list_directory(self, path)
    }
    fn upload_controlled(
        &self,
        path: &str,
        source: &Path,
        _remote_id: Option<&str>,
        version: Option<&str>,
        progress: crate::provider::Progress,
    ) -> anyhow::Result<MetadataEntry> {
        WebDavBackend::upload_controlled(self, path, source, version, progress)
    }
}

#[cfg(test)]
mod desktop_tests {
    use super::*;
    use std::{
        io::BufRead,
        net::TcpListener,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::Instant,
    };
    fn flaky_server() -> (String, Arc<AtomicUsize>, thread::JoinHandle<()>) {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", server.local_addr().unwrap());
        server.set_nonblocking(true).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let worker = thread::spawn(move || {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(2) {
                let Ok((mut socket, _)) = server.accept() else {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                }
                let first = count.fetch_add(1, Ordering::SeqCst) == 0;
                let response = if first {
                    "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                } else {
                    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                };
                let _ = socket.write_all(response.as_bytes());
            }
        });
        (url, calls, worker)
    }
    #[test]
    fn unencrypted_nas_requires_explicit_numeric_private_address() {
        for url in [
            "http://192.168.1.5/dav",
            "http://10.1.2.3/",
            "http://[fd12::5]/",
        ] {
            assert!(WebDavBackend::new(url, None, true).is_err());
            assert!(WebDavBackend::new_with_options(url, None, true, true).is_ok());
        }
        for url in [
            "http://8.8.8.8/",
            "http://nas.local/",
            "http://user:secret@192.168.1.5/",
            "http://192.168.1.5/?token=x",
        ] {
            assert!(WebDavBackend::new_with_options(url, None, true, true).is_err());
        }
        assert!(WebDavBackend::new("http://127.0.0.1/", None, true).is_ok());
        assert!(WebDavBackend::new("https://nas.example/dav", None, true).is_ok());
    }
    #[test]
    fn transient_read_is_retried_but_mutation_is_never_replayed() {
        let (url, calls, server) = flaky_server();
        let backend = WebDavBackend::new(&url, None, true).unwrap();
        let response = backend.send("PROPFIND", backend.request("PROPFIND", "/").unwrap());
        server.join().unwrap();
        assert!(response.is_ok(), "a transient read must recover");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let (url, calls, server) = flaky_server();
        let backend = WebDavBackend::new(&url, None, true).unwrap();
        assert!(
            backend
                .send("PUT", backend.request("PUT", "/file").unwrap())
                .is_err()
        );
        server.join().unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "mutation must never be replayed"
        );
    }
}
