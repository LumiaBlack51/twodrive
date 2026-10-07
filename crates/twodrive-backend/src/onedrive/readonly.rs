//! Graph read-only indexing and content transfer. Signed content URLs are ephemeral.
use super::{
    GraphBackend,
    browse::{endpoint, trusted_graph},
};
use std::{
    collections::HashSet,
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    time::Duration,
};
use twodrive_core::{CloudIdentity, Database, IndexedItem};

fn string(v: &serde_json::Value, key: &str) -> anyhow::Result<String> {
    v.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("invalid_graph_metadata"))
}
fn parse_item(v: &serde_json::Value) -> anyhow::Result<IndexedItem> {
    let deleted = v.get("deleted").is_some();
    if !deleted
        && v.get("file").is_some()
        && v.get("remoteItem").is_none()
        && v.get("package").is_none()
    {
        anyhow::ensure!(
            v.get("size").and_then(|s| s.as_u64()).is_some(),
            "invalid_file_size"
        );
    }
    Ok(IndexedItem {
        id: string(v, "id")?,
        parent: v
            .pointer("/parentReference/id")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        name: if deleted {
            String::new()
        } else {
            string(v, "name")?
        },
        kind: if v.get("remoteItem").is_some() || v.get("package").is_some() {
            "unsupported"
        } else if v.get("folder").is_some() {
            "folder"
        } else if v.get("file").is_some() {
            "file"
        } else {
            "unsupported"
        }
        .into(),
        size: v.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
        etag: v.get("eTag").and_then(|v| v.as_str()).unwrap_or("").into(),
        modified: v
            .get("lastModifiedDateTime")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        deleted,
    })
}
impl GraphBackend {
    pub fn readonly_identity(
        &self,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<CloudIdentity> {
        let v = self.browse_json(
            "https://graph.microsoft.com/v1.0/me/drive?$select=id,owner",
            check,
        )?;
        let drive = string(&v, "id")?;
        let account = v
            .pointer("/owner/user/id")
            .and_then(|v| v.as_str())
            .unwrap_or(&drive)
            .to_owned();
        Ok(CloudIdentity { account, drive })
    }
    /// Stage every page, then publish items and cursor in one SQLite transaction.
    /// Caller holds its account-generation guard while committing the returned cursor.
    pub fn stage_delta(
        &self,
        db: &Database,
        identity: &CloudIdentity,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<(String, bool, usize)> {
        let base = endpoint(&["drives", &identity.drive, "root", "delta"])?;
        let previous = db.cloud_delta(identity)?;
        let mut replace = previous.is_none();
        let mut url = previous.unwrap_or_else(|| format!("{base}?$top=100"));
        let mut seen = HashSet::new();
        let mut pages = 0;
        db.cloud_begin(identity)?;
        loop {
            check()?;
            anyhow::ensure!(
                seen.insert(url.clone()) && seen.len() <= 100000,
                "delta_continuation_loop"
            );
            anyhow::ensure!(
                trusted_graph(&url)?.path() == trusted_graph(&base)?.path(),
                "invalid_delta_continuation"
            );
            let body = match self.browse_json(&url, check) {
                Err(e) if !replace && e.to_string() == "delta_expired" => {
                    replace = true;
                    db.cloud_begin(identity)?;
                    seen.clear();
                    url = format!("{base}?$top=100");
                    continue;
                }
                other => other?,
            };
            let items = body
                .get("value")
                .and_then(|v| v.as_array())
                .ok_or_else(|| anyhow::anyhow!("invalid_delta_page"))?
                .iter()
                .map(parse_item)
                .collect::<anyhow::Result<Vec<_>>>()?;
            check()?;
            db.cloud_stage(identity, &items)?;
            pages += 1;
            if let Some(next) = body.get("@odata.nextLink").and_then(|v| v.as_str()) {
                url = next.into();
                continue;
            }
            let link = string(&body, "@odata.deltaLink")?;
            anyhow::ensure!(
                trusted_graph(&link)?.path() == trusted_graph(&base)?.path(),
                "invalid_delta_continuation"
            );
            check()?;
            return Ok((link, replace, pages));
        }
    }
    fn download_metadata(
        &self,
        identity: &CloudIdentity,
        item: &IndexedItem,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<serde_json::Value> {
        let url = endpoint(&["drives", &identity.drive, "items", &item.id])?;
        let v = self.browse_json(&url, check)?;
        let current = parse_item(&v)?;
        anyhow::ensure!(
            current.id == item.id
                && current.kind == "file"
                && !item.etag.is_empty()
                && current.etag == item.etag
                && current.size == item.size,
            "cloud_version_changed"
        );
        Ok(v)
    }
    pub fn download_partial(
        &self,
        identity: &CloudIdentity,
        item: &IndexedItem,
        path: &Path,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
        progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        anyhow::ensure!(file.metadata()?.len() <= item.size, "invalid_partial_size");
        for attempt in 0..3 {
            check()?;
            let meta = self.download_metadata(identity, item, check)?;
            let mut offset = file.metadata()?.len();
            progress(offset)?;
            if offset == item.size {
                file.sync_all()?;
                return Ok(());
            }
            let signed = string(&meta, "@microsoft.graph.downloadUrl")?;
            let url =
                url::Url::parse(&signed).map_err(|_| anyhow::anyhow!("invalid_download_target"))?;
            let secure = url.scheme() == "https";
            #[cfg(test)]
            let secure = secure || (url.scheme() == "http" && url.host_str() == Some("127.0.0.1"));
            anyhow::ensure!(
                secure
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.fragment().is_none(),
                "invalid_download_target"
            );
            // Deliberately a fresh client/request: no Microsoft Authorization header.
            let response = client
                .get(url)
                .header("Range", format!("bytes={offset}-"))
                .header("Accept-Encoding", "identity")
                .send();
            check()?;
            let transfer = (|| -> anyhow::Result<()> {
                let mut response =
                    response.map_err(|_| anyhow::anyhow!("download_network_error"))?;
                let status = response.status().as_u16();
                if status == 429 || status == 503 {
                    let delay = response
                        .headers()
                        .get("Retry-After")
                        .and_then(|v| v.to_str().ok())
                        .and_then(super::http::parse_retry_after_seconds)
                        .unwrap_or(Duration::from_secs(1));
                    anyhow::ensure!(delay <= Duration::from_secs(30), "rate_limited_retry_later");
                    super::http::checked_delay(delay, check)?;
                    anyhow::bail!("download_network_error");
                }
                anyhow::ensure!(status == 200 || status == 206, "download_http_error");
                if status == 200 {
                    file.set_len(0)?;
                    offset = 0;
                    progress(0)?;
                } else {
                    let expected = format!("bytes {offset}-{}/{}", item.size - 1, item.size);
                    anyhow::ensure!(
                        response
                            .headers()
                            .get("Content-Range")
                            .and_then(|v| v.to_str().ok())
                            == Some(expected.as_str()),
                        "invalid_content_range"
                    );
                }
                file.seek(SeekFrom::Start(offset))?;
                let mut buffer = [0u8; 128 * 1024];
                loop {
                    check()?;
                    let n = response
                        .read(&mut buffer)
                        .map_err(|_| anyhow::anyhow!("download_network_error"))?;
                    if n == 0 {
                        break;
                    }
                    anyhow::ensure!(offset + n as u64 <= item.size, "invalid_content_length");
                    file.write_all(&buffer[..n])?;
                    offset += n as u64;
                    progress(offset)?;
                }
                anyhow::ensure!(offset == item.size, "download_network_error");
                Ok(())
            })();
            file.sync_all()?;
            check()?;
            match transfer {
                Ok(()) => {
                    self.download_metadata(identity, item, check)?;
                    return Ok(());
                }
                Err(e)
                    if attempt < 2
                        && matches!(
                            e.to_string().as_str(),
                            "download_network_error" | "download_http_error"
                        ) =>
                {
                    super::http::checked_delay(Duration::from_millis(500), check)?
                }
                Err(e) => return Err(e),
            }
        }
        anyhow::bail!("download_retry_exhausted")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_http::{Header, Response, Server};
    fn identity() -> CloudIdentity {
        CloudIdentity {
            account: "account".into(),
            drive: "drive".into(),
        }
    }
    fn metadata(version: &str, url: &str) -> serde_json::Value {
        serde_json::json!({"id":"file","name":"test.txt","file":{},"size":10,"eTag":version,"@microsoft.graph.downloadUrl":url})
    }
    fn temp() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "twodrive-readonly-{}",
            super::super::auth::random_string(20)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
    #[test]
    fn delta_pages_incremental_and_expired_cursor_rebuild() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let graph = super::super::browse::tests::backend(&server);
        let root = temp();
        let db = Database::new(root.join("db"));
        db.init_cloud().unwrap();
        let base = "https://graph.microsoft.com/v1.0/drives/drive/root/delta";
        let worker = std::thread::spawn(move || {
            let pages = [
                (
                    200,
                    serde_json::json!({"value":[{"id":"file","name":"old","file":{},"size":10,"eTag":"v1"}],"@odata.nextLink":format!("{base}?page=2")}),
                ),
                (
                    200,
                    serde_json::json!({"value":[],"@odata.deltaLink":format!("{base}?cursor=1")}),
                ),
                (
                    200,
                    serde_json::json!({"value":[{"id":"file","name":"renamed","file":{},"size":10,"eTag":"v2","parentReference":{"id":"new-parent"}}],"@odata.deltaLink":format!("{base}?cursor=2")}),
                ),
                (
                    410,
                    serde_json::json!({"error":{"code":"resyncChangesApplyDifferences"}}),
                ),
                (
                    200,
                    serde_json::json!({"value":[{"id":"replacement","name":"new","file":{},"size":1,"eTag":"v1"}],"@odata.deltaLink":format!("{base}?cursor=3")}),
                ),
            ];
            for (status, body) in pages {
                let request = server
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap();
                assert_eq!(request.method().as_str(), "GET");
                request
                    .respond(Response::from_string(body.to_string()).with_status_code(status))
                    .unwrap();
            }
        });
        let (link, replace, pages) = graph.stage_delta(&db, &identity(), &mut || Ok(())).unwrap();
        assert!(replace);
        assert_eq!(pages, 2);
        assert!(db.cloud_items(&identity()).unwrap().is_empty());
        db.cloud_commit(&identity(), &link, replace).unwrap();
        let (link, replace, _) = graph.stage_delta(&db, &identity(), &mut || Ok(())).unwrap();
        assert!(!replace);
        db.cloud_commit(&identity(), &link, replace).unwrap();
        assert_eq!(db.cloud_items(&identity()).unwrap()[0].name, "renamed");
        let (link, replace, _) = graph.stage_delta(&db, &identity(), &mut || Ok(())).unwrap();
        assert!(replace);
        assert_eq!(db.cloud_items(&identity()).unwrap()[0].name, "renamed");
        db.cloud_commit(&identity(), &link, replace).unwrap();
        assert_eq!(db.cloud_items(&identity()).unwrap()[0].id, "replacement");
        worker.join().unwrap();
    }
    #[test]
    fn interrupted_delta_keeps_committed_generation() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let graph = super::super::browse::tests::backend(&server);
        let root = temp();
        let db = Database::new(root.join("db"));
        db.init_cloud().unwrap();
        let old = parse_item(&metadata("v1", "unused")).unwrap();
        db.cloud_stage(&identity(), std::slice::from_ref(&old))
            .unwrap();
        db.cloud_commit(
            &identity(),
            "https://graph.microsoft.com/v1.0/drives/drive/root/delta?old",
            true,
        )
        .unwrap();
        let worker = std::thread::spawn(move || {
            let r = server.recv().unwrap();
            r.respond(Response::from_string(
                r#"{"value":[],"@odata.nextLink":"https://evil.example/private"}"#,
            ))
            .unwrap();
        });
        assert!(graph.stage_delta(&db, &identity(), &mut || Ok(())).is_err());
        assert_eq!(db.cloud_items(&identity()).unwrap(), vec![old]);
        assert!(
            db.cloud_delta(&identity())
                .unwrap()
                .unwrap()
                .ends_with("?old")
        );
        worker.join().unwrap();
    }
    #[test]
    fn range_resume_200_restart_version_change_and_no_bearer() {
        for scenario in ["resume", "ignored_range", "version_change", "bad_range"] {
            let server = Server::http("127.0.0.1:0").unwrap();
            let graph = super::super::browse::tests::backend(&server);
            let content = Server::http("127.0.0.1:0").unwrap();
            let url = format!("http://{}/signed-secret", content.server_addr());
            let item = parse_item(&metadata("v1", &url)).unwrap();
            let root = temp();
            let path = root.join("file.partial");
            std::fs::write(&path, b"0123").unwrap();
            let meta_worker = std::thread::spawn(move || {
                let r = server.recv().unwrap();
                r.respond(Response::from_string(metadata("v1", &url).to_string()))
                    .unwrap();
                if scenario != "bad_range" {
                    let r = server
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap()
                        .unwrap();
                    r.respond(Response::from_string(
                        metadata(
                            if scenario == "version_change" {
                                "v2"
                            } else {
                                "v1"
                            },
                            &url,
                        )
                        .to_string(),
                    ))
                    .unwrap();
                }
            });
            let content_worker = std::thread::spawn(move || {
                let r = content.recv().unwrap();
                assert!(r.headers().iter().all(|h| !h.field.equiv("Authorization")));
                assert!(
                    r.headers()
                        .iter()
                        .any(|h| h.field.equiv("Range") && h.value.as_str() == "bytes=4-")
                );
                let response = if scenario == "ignored_range" {
                    Response::from_string("0123456789")
                } else {
                    Response::from_string("456789")
                        .with_status_code(206)
                        .with_header(
                            Header::from_bytes(
                                "Content-Range",
                                if scenario == "bad_range" {
                                    "bytes 0-5/10"
                                } else {
                                    "bytes 4-9/10"
                                },
                            )
                            .unwrap(),
                        )
                };
                r.respond(response).unwrap();
            });
            let mut progress = vec![];
            let result =
                graph.download_partial(&identity(), &item, &path, &mut || Ok(()), &mut |n| {
                    progress.push(n);
                    Ok(())
                });
            if scenario == "version_change" {
                assert_eq!(result.unwrap_err().to_string(), "cloud_version_changed");
            } else if scenario == "bad_range" {
                assert_eq!(result.unwrap_err().to_string(), "invalid_content_range");
                assert_eq!(std::fs::read(&path).unwrap(), b"0123");
            } else {
                result.unwrap();
                assert_eq!(std::fs::read(&path).unwrap(), b"0123456789");
                assert_eq!(progress.last(), Some(&10));
            }
            if scenario == "ignored_range" {
                assert!(progress.contains(&0));
            }
            meta_worker.join().unwrap();
            content_worker.join().unwrap();
        }
    }
    #[test]
    fn cancelled_download_preserves_partial_without_request() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let graph = super::super::browse::tests::backend(&server);
        let root = temp();
        let path = root.join("file.partial");
        std::fs::write(&path, b"0123").unwrap();
        let item = parse_item(&metadata("v1", "unused")).unwrap();
        let result = graph.download_partial(
            &identity(),
            &item,
            &path,
            &mut || anyhow::bail!("download_cancelled"),
            &mut |_| Ok(()),
        );
        assert_eq!(result.unwrap_err().to_string(), "download_cancelled");
        assert_eq!(std::fs::read(path).unwrap(), b"0123");
        assert!(
            server
                .recv_timeout(Duration::from_millis(50))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn interrupted_http_body_resumes_from_actual_partial_length() {
        use std::net::TcpListener;
        let metadata_server = Server::http("127.0.0.1:0").unwrap();
        let graph = super::super::browse::tests::backend(&metadata_server);
        let content = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/signed", content.local_addr().unwrap());
        let item = parse_item(&metadata("v1", &url)).unwrap();
        let root = temp();
        let path = root.join("partial");
        let meta = std::thread::spawn(move || {
            for _ in 0..3 {
                let r = metadata_server
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap();
                r.respond(Response::from_string(metadata("v1", &url).to_string()))
                    .unwrap();
            }
        });
        let body = std::thread::spawn(move || {
            for offset in [0, 4] {
                let (mut socket, _) = content.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut b = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    socket.read_exact(&mut b).unwrap();
                    request.push(b[0]);
                }
                let request = String::from_utf8(request).unwrap().to_lowercase();
                assert!(!request.contains("authorization:"));
                assert!(request.contains(&format!("range: bytes={offset}-")));
                if offset == 0 {
                    socket.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-9/10\r\nContent-Length: 10\r\nConnection: close\r\n\r\n0123").unwrap();
                } else {
                    socket.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 4-9/10\r\nContent-Length: 6\r\nConnection: close\r\n\r\n456789").unwrap();
                }
            }
        });
        graph
            .download_partial(&identity(), &item, &path, &mut || Ok(()), &mut |_| Ok(()))
            .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"0123456789");
        meta.join().unwrap();
        body.join().unwrap();
    }
    #[test]
    fn large_cancelled_transfer_resumes_across_worker_restart() {
        use std::sync::atomic::{AtomicBool, Ordering};
        const SIZE: usize = 4 * 1024 * 1024;
        let server = Server::http("127.0.0.1:0").unwrap();
        let graph = super::super::browse::tests::backend(&server);
        let content = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}/signed", content.server_addr());
        let mut value = metadata("v1", &url);
        value["size"] = SIZE.into();
        let item = parse_item(&value).unwrap();
        let root = temp();
        let path = root.join("partial");
        let meta = std::thread::spawn(move || {
            for _ in 0..3 {
                let r = server
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap()
                    .unwrap();
                r.respond(Response::from_string(value.to_string())).unwrap();
            }
        });
        let body = std::thread::spawn(move || {
            for attempt in 0..2 {
                let r = content
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap()
                    .unwrap();
                assert!(!r.headers().iter().any(|h| h.field.equiv("Authorization")));
                let range = r
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Range"))
                    .unwrap()
                    .value
                    .as_str();
                let offset: usize = range
                    .strip_prefix("bytes=")
                    .unwrap()
                    .trim_end_matches('-')
                    .parse()
                    .unwrap();
                if attempt == 0 {
                    assert_eq!(offset, 0);
                } else {
                    assert!(offset > 0 && offset < SIZE);
                }
                let _ = r.respond(
                    Response::from_data(vec![b'x'; SIZE - offset])
                        .with_status_code(206)
                        .with_header(
                            Header::from_bytes(
                                "Content-Range",
                                format!("bytes {offset}-{}/{}", SIZE - 1, SIZE),
                            )
                            .unwrap(),
                        ),
                );
            }
        });
        let cancelled = AtomicBool::new(false);
        let result = graph.download_partial(
            &identity(),
            &item,
            &path,
            &mut || {
                anyhow::ensure!(!cancelled.load(Ordering::SeqCst), "cancelled");
                Ok(())
            },
            &mut |n| {
                if n >= 128 * 1024 {
                    cancelled.store(true, Ordering::SeqCst);
                }
                Ok(())
            },
        );
        assert!(result.is_err());
        let partial = path.metadata().unwrap().len();
        assert!(partial > 0 && partial < SIZE as u64);
        // No in-memory offset is supplied: a new invocation reconstructs it from disk.
        graph
            .download_partial(&identity(), &item, &path, &mut || Ok(()), &mut |_| Ok(()))
            .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), vec![b'x'; SIZE]);
        meta.join().unwrap();
        body.join().unwrap();
    }
    #[test]
    fn invalid_file_size_is_never_an_empty_cache_candidate() {
        let mut v = metadata("v1", "unused");
        v["size"] = (-1).into();
        assert!(parse_item(&v).is_err());
        v.as_object_mut().unwrap().remove("file");
        v["folder"] = serde_json::json!({});
        assert!(parse_item(&v).is_ok());
    }
}
