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
    io::{Read, Write},
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

#[derive(Clone)]
pub struct WebDavBackend {
    client: Client,
    base: Url,
    credentials: Option<Credentials>,
    writable: bool,
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
        let mut base = Url::parse(base).context("invalid WebDAV root URL")?;
        let loopback = match base.host() {
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        ensure!(
            base.scheme() == "https" || (base.scheme() == "http" && loopback),
            "WebDAV requires HTTPS; HTTP is allowed only on numeric loopback addresses"
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
        })
    }

    pub fn root_url(&self) -> &str {
        self.base.as_str()
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

    fn send(&self, operation: &str, request: RequestBuilder) -> anyhow::Result<Response> {
        // reqwest errors can contain URLs. Do not put server paths/credentials into logs.
        let response = request.send().map_err(|error| {
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
        Ok(response)
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
        self.send("PUT", self.versioned_put(path, if_match)?.body(content))?;
        self.get_metadata_by_path(path)?
            .context("uploaded DAV resource not found")
    }
    fn upload_file_with_version(
        &self,
        path: &str,
        source: &Path,
        _id: Option<&str>,
        if_match: Option<&str>,
        progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        let file = File::open(source)?;
        let size = file.metadata()?.len();
        progress(0, size)?;
        self.send(
            "PUT",
            self.versioned_put(path, if_match)?
                .header("Content-Length", size)
                .body(file),
        )?;
        progress(size, size)?;
        self.get_metadata_by_path(path)?
            .context("uploaded DAV resource not found")
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
