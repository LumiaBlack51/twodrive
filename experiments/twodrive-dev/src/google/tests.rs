use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

type FixtureFiles = Arc<Mutex<BTreeMap<String, (Value, Vec<u8>)>>>;
struct Fixture {
    drive: GoogleDrive,
    files: FixtureFiles,
    stale: Arc<AtomicBool>,
    media_change: Arc<AtomicBool>,
    list_requests: Arc<AtomicUsize>,
    quota: Arc<Mutex<Value>>,
    quota_requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    _dir: tempfile::TempDir,
}
fn resource(id: &str, name: &str, parent: &str, mime: &str, bytes: &[u8], version: usize) -> Value {
    json!({"id":id,"title":name,"parents":[{"id":parent}],"mimeType":mime,"fileSize":bytes.len().to_string(),"modifiedDate":"2026-10-14T00:00:00Z","etag":format!("\"v{version}\""),"labels":{"trashed":false},"alternateLink":"https://docs.google.com/document/d/test/edit"})
}
impl Fixture {
    fn new() -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", server.server_addr());
        let base = Url::parse(&format!("{origin}/drive/v2/")).unwrap();
        let upload = Url::parse(&format!("{origin}/upload/drive/v2/")).unwrap();
        let files = Arc::new(Mutex::new(BTreeMap::from([(
            "root".into(),
            (resource("root", "root", "root", FOLDER, b"", 1), vec![]),
        )])));
        let thread_files = files.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let stale = Arc::new(AtomicBool::new(false));
        let thread_stale = stale.clone();
        let media_change = Arc::new(AtomicBool::new(false));
        let thread_media_change = media_change.clone();
        let list_requests = Arc::new(AtomicUsize::new(0));
        let thread_list_requests = list_requests.clone();
        let quota = Arc::new(Mutex::new(
            json!({"quotaType":"LIMITED","quotaBytesTotal":"1000","quotaBytesUsedAggregate":"700","quotaBytesUsed":"200"}),
        ));
        let thread_quota = quota.clone();
        let quota_requests = Arc::new(AtomicUsize::new(0));
        let thread_quota_requests = quota_requests.clone();
        let thread = std::thread::spawn(move || {
            let mut counter = 0;
            let mut sessions = BTreeMap::<String, (String, Value, Vec<u8>, Option<String>)>::new();
            while !thread_stop.load(Ordering::Relaxed) {
                let Some(mut req) = server
                    .recv_timeout(std::time::Duration::from_millis(50))
                    .unwrap()
                else {
                    continue;
                };
                assert!(
                    req.headers().iter().any(
                        |h| h.field.equiv("Authorization") && h.value.as_str() == "Bearer test"
                    )
                );
                let u = Url::parse(&format!("{origin}{}", req.url())).unwrap();
                let path = u.path().to_string();
                let q: BTreeMap<_, _> = u
                    .query_pairs()
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect();
                let method = req.method().to_string();
                let if_match = req
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("If-Match"))
                    .map(|h| h.value.to_string());
                let range = req
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Content-Range"))
                    .map(|h| h.value.to_string());
                let mut body = Vec::new();
                req.as_reader().read_to_end(&mut body).unwrap();
                let mut files = thread_files.lock().unwrap();
                let mut status = 200;
                let mut headers = Vec::<(&str, String)>::new();
                let mut result = json!({});
                let mut media = None;
                if path == "/drive/v2/about" && method == "GET" {
                    assert_eq!(
                        q.get("fields").map(String::as_str),
                        Some("quotaType,quotaBytesTotal,quotaBytesUsedAggregate")
                    );
                    thread_quota_requests.fetch_add(1, Ordering::Relaxed);
                    result = thread_quota.lock().unwrap().clone();
                    status = result["_status"].as_u64().unwrap_or(200) as u16;
                } else if path == "/drive/v2/files/generateIds" {
                    counter += 1;
                    result = json!({"ids":[format!("f{counter}")]});
                } else if path == "/drive/v2/files" && method == "GET" {
                    thread_list_requests.fetch_add(1, Ordering::Relaxed);
                    let parent = q.get("q").unwrap().split('\'').nth(1);
                    let items: Vec<_> = files
                        .values()
                        .filter(|(v, _)| {
                            parent.is_none_or(|p| v["parents"][0]["id"] == p)
                                && v["id"] != "root"
                                && v["labels"]["trashed"] != true
                        })
                        .map(|(v, _)| v.clone())
                        .collect();
                    result = json!({"items":items});
                } else if path == "/drive/v2/files" && method == "POST" {
                    counter += 1;
                    let v: Value = serde_json::from_slice(&body).unwrap();
                    let id = format!("d{counter}");
                    result = resource(
                        &id,
                        v["title"].as_str().unwrap(),
                        v["parents"][0]["id"].as_str().unwrap(),
                        FOLDER,
                        b"",
                        1,
                    );
                    files.insert(id, (result.clone(), vec![]));
                } else if path.starts_with("/upload/drive/v2/files")
                    && q.get("uploadType").is_some_and(|v| v == "resumable")
                {
                    let v: Value = serde_json::from_slice(&body).unwrap();
                    let (id, metadata) = if method == "PUT" {
                        let id = path.rsplit('/').next().unwrap().to_string();
                        (id.clone(), files[&id].0.clone())
                    } else {
                        (v["id"].as_str().unwrap().into(), v)
                    };
                    if if_match
                        .as_ref()
                        .is_some_and(|etag| files[&id].0["etag"] != *etag)
                    {
                        status = 412;
                    }
                    if status == 200 {
                        let sid = format!("s{counter}");
                        counter += 1;
                        headers.push((
                            "Location",
                            format!("{origin}/upload/drive/v2/session/{sid}"),
                        ));
                        sessions.insert(sid, (id, metadata, Vec::new(), if_match));
                    }
                } else if path.starts_with("/upload/drive/v2/session/") {
                    let sid = path.rsplit('/').next().unwrap();
                    let (id, metadata, bytes, etag) = sessions.get_mut(sid).unwrap();
                    if thread_stale.swap(false, Ordering::Relaxed) {
                        files.get_mut(id).unwrap().0["etag"] = json!("\"external\"");
                    }
                    if range.as_ref().unwrap().starts_with("bytes */")
                        && range.as_deref() != Some("bytes */0")
                    {
                        status = 308;
                        if !bytes.is_empty() {
                            headers.push(("Range", format!("bytes=0-{}", bytes.len() - 1)));
                        }
                    } else if etag.as_ref().is_some_and(|v| files[id].0["etag"] != *v) {
                        status = 412;
                    } else {
                        bytes.extend(body);
                        let total = range
                            .unwrap()
                            .rsplit('/')
                            .next()
                            .unwrap()
                            .parse::<usize>()
                            .unwrap();
                        if bytes.len() < total {
                            status = 308;
                            headers.push(("Range", format!("bytes=0-{}", bytes.len() - 1)));
                        } else {
                            result = resource(
                                id,
                                metadata["title"].as_str().unwrap(),
                                metadata["parents"][0]["id"].as_str().unwrap(),
                                "application/octet-stream",
                                bytes,
                                2,
                            );
                            files.insert(id.clone(), (result.clone(), bytes.clone()));
                        }
                    }
                } else if path.starts_with("/drive/v2/files/") {
                    let rest = path.strip_prefix("/drive/v2/files/").unwrap();
                    let id = rest.trim_end_matches("/trash");
                    if let Some((v, b)) = files.get_mut(id) {
                        if if_match.as_ref().is_some_and(|etag| *etag != v["etag"]) {
                            status = 412;
                        } else if rest.ends_with("/trash") {
                            v["labels"]["trashed"] = json!(true);
                            result = v.clone();
                        } else if method == "PATCH" {
                            let updates: Value = serde_json::from_slice(&body).unwrap();
                            v["title"] = updates["title"].clone();
                            v["parents"] = json!([{"id":q["addParents"]}]);
                            v["etag"] = json!("\"renamed\"");
                            result = v.clone();
                        } else if q.get("alt").is_some_and(|a| a == "media") {
                            if if_match.is_some() {
                                status = 412;
                            } else {
                                media = Some(b.clone());
                            }
                            if thread_media_change.swap(false, Ordering::Relaxed) {
                                v["etag"] = json!("\"changed-during-read\"");
                            }
                        } else {
                            result = v.clone();
                        }
                    } else {
                        status = 404;
                    }
                } else {
                    panic!("unexpected fixture request: {method} {path}");
                }
                drop(files);
                let bytes = media.unwrap_or_else(|| serde_json::to_vec(&result).unwrap());
                let mut response = tiny_http::Response::from_data(bytes).with_status_code(status);
                for (key, value) in headers {
                    response =
                        response.with_header(tiny_http::Header::from_bytes(key, value).unwrap());
                }
                let _ = req.respond(response);
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let state = State::new(dir.path().join("private")).unwrap();
        let drive = GoogleDrive {
            state,
            client: auth::client("direct").unwrap(),
            root: "root".into(),
            account_id: "fixture-account".into(),
            account_name: "Test".into(),
            writable: true,
            api: base,
            upload,
            test_token: Some("test".into()),
        };
        Self {
            drive,
            files,
            stale,
            media_change,
            list_requests,
            quota,
            quota_requests,
            stop,
            thread: Some(thread),
            _dir: dir,
        }
    }
    fn add(&self, id: &str, name: &str, mime: &str, bytes: &[u8]) {
        self.files.lock().unwrap().insert(
            id.into(),
            (resource(id, name, "root", mime, bytes, 1), bytes.to_vec()),
        );
    }
}

#[test]
fn storage_usage_reads_account_aggregate_without_listing_or_write_permission() {
    let mut fixture = Fixture::new();
    fixture.drive.writable = false;
    let usage = fixture
        .drive
        .storage_usage(&mut || Ok(()))
        .unwrap()
        .unwrap();
    assert_eq!(usage.total, Some(1000));
    assert_eq!(usage.used, Some(700));
    assert_eq!(usage.free, Some(300));
    assert_eq!(usage.scope, "account");
    assert_eq!(fixture.quota_requests.load(Ordering::Relaxed), 1);
    assert_eq!(fixture.list_requests.load(Ordering::Relaxed), 0);
}

#[test]
fn storage_usage_preserves_unknown_limits_zero_and_failures() {
    let fixture = Fixture::new();
    for (value, expected) in [
        (
            json!({"quotaType":"UNLIMITED","quotaBytesTotal":"0","quotaBytesUsedAggregate":"41"}),
            (None, Some(41), None),
        ),
        (
            json!({"quotaType":"LIMITED","quotaBytesTotal":"0","quotaBytesUsedAggregate":"0"}),
            (Some(0), Some(0), Some(0)),
        ),
        (
            json!({"quotaType":"LIMITED","quotaBytesTotal":"10","quotaBytesUsedAggregate":"12"}),
            (Some(10), Some(12), Some(0)),
        ),
        (
            json!({"quotaType":"LIMITED","quotaBytesTotal":"10"}),
            (Some(10), None, None),
        ),
    ] {
        *fixture.quota.lock().unwrap() = value;
        let usage = fixture
            .drive
            .storage_usage(&mut || Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!((usage.total, usage.used, usage.free), expected);
    }
    for value in [
        json!({"quotaBytesTotal":"invalid-private-response"}),
        json!({"_status":503}),
    ] {
        *fixture.quota.lock().unwrap() = value;
        let error = fixture
            .drive
            .storage_usage(&mut || Ok(()))
            .unwrap_err()
            .to_string();
        assert!(!error.contains("invalid-private-response"));
    }
    assert_eq!(fixture.list_requests.load(Ordering::Relaxed), 0);
}

#[test]
fn storage_usage_observes_cancellation_before_and_after_request() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .drive
            .storage_usage(&mut || anyhow::bail!("cancelled"))
            .is_err()
    );
    assert_eq!(fixture.quota_requests.load(Ordering::Relaxed), 0);
    let mut calls = 0;
    assert!(
        fixture
            .drive
            .storage_usage(&mut || {
                calls += 1;
                anyhow::ensure!(calls == 1, "cancelled");
                Ok(())
            })
            .is_err()
    );
    assert_eq!(calls, 2);
    assert_eq!(fixture.quota_requests.load(Ordering::Relaxed), 1);
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}
#[test]
fn google_http_read_write_move_trash_and_delta() {
    let f = Fixture::new();
    let d = &f.drive;
    d.create_folder("/folder").unwrap();
    let m = d.upload("/folder/file.txt", b"first".to_vec()).unwrap();
    assert_eq!(d.download(&m.remote_id).unwrap(), b"first");
    assert!(d.upload("/folder/file.txt", b"overwrite".to_vec()).is_err());
    let m = d
        .upload_with_etag("/folder/file.txt", b"changed".to_vec(), Some(&m.etag))
        .unwrap();
    assert_eq!(d.download(&m.remote_id).unwrap(), b"changed");
    assert!(
        d.upload_with_etag("/folder/file.txt", b"wrong".to_vec(), Some("\"stale\""))
            .is_err()
    );
    let m = d.rename(&m.remote_id, "/moved.txt").unwrap();
    assert!(
        d.get_metadata_by_path("/folder/file.txt")
            .unwrap()
            .is_none()
    );
    let before = d.list_delta(None).unwrap();
    d.delete(&m.remote_id).unwrap();
    let after = d.list_delta(before.delta_link.as_deref()).unwrap();
    assert!(after.deleted_remote_ids.contains(&m.remote_id));
    assert_eq!(
        f.files.lock().unwrap()[&m.remote_id].0["labels"]["trashed"],
        true
    );
}
#[test]
fn duplicate_titles_are_distinct_and_native_docs_are_explicit_shortcuts() {
    let f = Fixture::new();
    f.add("a", "same", "application/octet-stream", b"a");
    f.add("b", "same", "application/octet-stream", b"b");
    f.add("c", "你好/Doc", "application/vnd.google-apps.document", b"");
    let list = f.drive.list_directory("/").unwrap();
    assert_eq!(list.len(), 3);
    assert!(list.iter().any(|m| m.path == "/same~g~a"));
    assert!(list.iter().any(|m| m.path == "/same~g~b"));
    assert!(f.drive.get_metadata_by_path("/same").unwrap().is_none());
    let doc = list.iter().find(|m| m.remote_id == "c").unwrap();
    assert!(doc.path.contains("你好%2FDoc~google.url"));
    assert_eq!(f.drive.download("c").unwrap().len() as u64, doc.size);
    assert!(
        f.drive
            .upload_with_etag(&doc.path, b"bad".to_vec(), Some(&doc.etag))
            .is_err()
    );
    for name in ["你好", "a~g~b", "100%"] {
        assert_eq!(component(name), name);
        assert_eq!(title(name).unwrap(), name);
    }
    for name in ["", ".", "..", " x ", "foo/bar"] {
        assert!(!component(name).contains('/'));
    }
    let p = f
        .drive
        .upload("/local #%.txt", b"percent".to_vec())
        .unwrap();
    assert_eq!(p.path, "/local #%.txt");
    assert_eq!(f.drive.download(&p.remote_id).unwrap(), b"percent");
    let changed = f
        .drive
        .upload_with_etag("/same~g~a", b"distinct".to_vec(), Some("\"v1\""))
        .unwrap();
    assert_eq!(changed.remote_id, "a");
    assert_eq!(f.drive.download("b").unwrap(), b"b");
}
#[test]
fn cancelled_chunked_upload_resumes_saved_identity_and_offset() {
    let f = Fixture::new();
    let mut source = tempfile::NamedTempFile::new().unwrap();
    let bytes = vec![42; CHUNK + 17];
    source.write_all(&bytes).unwrap();
    let error = f
        .drive
        .upload_stream("/large", source.path(), None, None, &mut |done, _| {
            ensure!(done == 0, "cancelled");
            Ok(())
        })
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"));
    let saved: Vec<_> = std::fs::read_dir(&f.drive.state.dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|p| {
            p.file_name()
                .to_string_lossy()
                .starts_with("google-intent-")
        })
        .collect();
    assert_eq!(saved.len(), 1);
    let m = f
        .drive
        .upload_stream("/large", source.path(), None, None, &mut |_, _| Ok(()))
        .unwrap();
    assert_eq!(f.drive.download(&m.remote_id).unwrap(), bytes);
    assert!(!saved[0].path().exists());
}
#[test]
fn server_side_version_change_is_rejected_without_overwrite() {
    let f = Fixture::new();
    f.add("a", "file", "application/octet-stream", b"old");
    f.stale.store(true, Ordering::Relaxed);
    let e = f
        .drive
        .upload_with_etag("/file", b"new".to_vec(), Some("\"v1\""))
        .unwrap_err();
    assert!(f.drive.is_conflict_error(&e));
    assert_eq!(f.files.lock().unwrap()["a"].1, b"old");
}
#[test]
fn untrusted_upload_urls_and_readonly_mutations_are_rejected() {
    let mut f = Fixture::new();
    assert!(
        f.drive
            .session_url("https://evil.invalid/upload/drive/v2/session")
            .is_err()
    );
    assert!(
        f.drive
            .session_url("https://www.googleapis.com.evil.invalid/upload/drive/v2/session")
            .is_err()
    );
    f.drive.writable = false;
    assert!(f.drive.upload("/x", vec![]).is_err());
    assert!(f.drive.create_folder("/x").is_err());
    assert!(f.drive.rename("root", "/x").is_err());
    assert!(f.drive.delete("root").is_err());
}

#[test]
fn media_etag_differs_and_metadata_change_during_download_is_rejected() {
    let f = Fixture::new();
    f.add("a", "file", "application/octet-stream", b"old");
    assert_eq!(f.drive.download("a").unwrap(), b"old");
    f.media_change.store(true, Ordering::Relaxed);
    assert!(
        f.drive
            .download("a")
            .unwrap_err()
            .to_string()
            .contains("发生变化")
    );
}

#[test]
fn snapshot_uses_one_inventory_for_nested_directories() {
    let f = Fixture::new();
    f.add("d1", "one", FOLDER, b"");
    f.add("d2", "two", FOLDER, b"");
    f.add("a", "file", "application/octet-stream", b"data");
    {
        let mut files = f.files.lock().unwrap();
        files.get_mut("d2").unwrap().0["parents"] = json!([{"id":"d1"}]);
        files.get_mut("a").unwrap().0["parents"] = json!([{"id":"d2"}]);
    }
    let entries = f.drive.list_all().unwrap();
    assert!(entries.iter().any(|m| m.path == "/one/two/file"));
    assert_eq!(f.list_requests.load(Ordering::Relaxed), 1);
}
