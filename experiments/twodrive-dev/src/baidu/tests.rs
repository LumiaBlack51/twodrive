use super::{
    auth::{self, Session},
    drive::BaiduDrive,
    http::Api,
};
use crate::{
    model::CloudBackend,
    provider::Provider,
    state::{State, now},
};
use base64::Engine;
use serde_json::{Value, json};
use sha2::Digest;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tiny_http::{Header, Response, Server};
use url::Url;

type Reply = Response<std::io::Cursor<Vec<u8>>>;
fn json_reply(value: Value) -> Reply {
    Response::from_data(serde_json::to_vec(&value).unwrap())
        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
}
fn row(id: u64, path: &str) -> Value {
    json!({"fs_id":id,"path":path,"server_filename":path.rsplit('/').next().unwrap(),"isdir":0,"size":DATA.len(),"md5":"opaque-server-md5","server_mtime":42})
}
const DATA: &[u8] = b"download bytes from correct Baidu account";
struct Fixture {
    _root: tempfile::TempDir,
    state: State,
    api: Api,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Fixture {
    fn new(mut custom: impl FnMut(&Url, &str) -> Option<Reply> + Send + 'static) -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path().join("private")).unwrap();
        auth::save(
            &state,
            &Session {
                app_key: "synthetic-key".into(),
                secret_key: "synthetic-SECRET".into(),
                access_token: "synthetic-ACCESS".into(),
                refresh_token: "synthetic-REFRESH".into(),
                expires_at: now() + 3600,
                uk: 7,
                name: "Synthetic account".into(),
                generation: "login-a".into(),
                identity_pending: false,
            },
        )
        .unwrap();
        let server = Server::http("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", server.server_addr());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let calls = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let base = origin.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                if let Some(request) = server.recv_timeout(Duration::from_millis(25)).unwrap() {
                    assert_eq!(
                        request.method().as_str(),
                        "GET",
                        "read-only adapter must never mutate files"
                    );
                    let u = Url::parse(&format!("{base}{}", request.url())).unwrap();
                    calls.lock().unwrap().push(u.path().to_owned());
                    let reply = custom(&u, &base).unwrap_or_else(|| match u.path() {
                        "/rest/2.0/xpan/nas" => {
                            json_reply(json!({"errno":0,"uk":7,"netdisk_name":"Synthetic account"}))
                        }
                        "/rest/2.0/xpan/file" => {
                            json_reply(json!({"errno":0,"list":[row(10,"/中文 #%.txt")]}))
                        }
                        "/rest/2.0/xpan/multimedia" => {
                            let mut r = row(10, "/中文 #%.txt");
                            r["dlink"] = json!(format!("{base}/content"));
                            json_reply(json!({"errno":0,"list":[r]}))
                        }
                        "/content" => Response::from_data(DATA.to_vec()).with_header(
                            Header::from_bytes(
                                "Content-MD5",
                                base64::engine::general_purpose::STANDARD
                                    .encode(md5::Md5::digest(DATA)),
                            )
                            .unwrap(),
                        ),
                        "/api/quota" => {
                            json_reply(json!({"errno":0,"total":1000,"used":200,"free":50}))
                        }
                        _ => json_reply(json!({"errno":2})),
                    });
                    let _ = request.respond(reply);
                }
            }
        });
        Self {
            _root: root,
            state,
            api: Api::local(origin),
            requests,
            stop,
            thread: Some(thread),
        }
    }
    fn backend(&self, root: &str) -> BaiduDrive {
        BaiduDrive::connect_with(self.state.clone(), root, self.api.clone(), &mut || Ok(()))
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

#[test]
fn storage_root_change_isolates_dirty_native_cache_without_reauthorizing() {
    let f = Fixture::new(|u, _| {
        if u.path() == "/rest/2.0/xpan/file" {
            let dir = u
                .query_pairs()
                .find(|(key, _)| key == "dir")
                .unwrap()
                .1
                .into_owned();
            Some(json_reply(
                json!({"errno":0,"list":[row(10, &format!("{dir}/same.txt"))]}),
            ))
        } else {
            None
        }
    });
    let mount = f._root.path().join("same-profile-mount-state");
    let a = f.backend("/root-A");
    let old_object = a.list_directory("/").unwrap()[0].remote_id.clone();
    let original_binding = a.binding.clone();
    let cache_a = crate::mount_write::account_cache(&mount, &a.binding);
    std::fs::create_dir_all(cache_a.join("vfs/vfsMeta/pending")).unwrap();
    std::fs::write(
        cache_a.join("vfs/vfsMeta/pending/new.txt"),
        br#"{"Dirty":true}"#,
    )
    .unwrap();
    std::fs::write(cache_a.join("unuploaded-new-file.content"), b"root A only").unwrap();
    drop(a);
    let b = f.backend("/root-B");
    assert!(
        b.get_metadata(&old_object).is_err(),
        "an old-root object must not be interpreted within the new root"
    );
    let cache_b = crate::mount_write::account_cache(&mount, &b.binding);
    assert_ne!(
        cache_a, cache_b,
        "changing only the storage root must never select root A's dirty VFS cache"
    );
    assert!(
        !cache_b.exists(),
        "a fresh root must not inherit pending new files"
    );
    assert_eq!(
        std::fs::read(cache_a.join("unuploaded-new-file.content")).unwrap(),
        b"root A only"
    );
    assert_eq!(auth::load(&f.state).unwrap().generation, "login-a");
    drop(b);
    let reopened = f.backend("/root-A");
    assert_eq!(
        reopened.binding, original_binding,
        "reopening the same root must retain recovery identity"
    );
    assert_eq!(
        crate::mount_write::account_cache(&mount, &reopened.binding),
        cache_a
    );
    assert!(
        BaiduDrive::connect_with(
            f.state.clone(),
            "/root-A/../root-B",
            f.api.clone(),
            &mut || Ok(())
        )
        .is_err()
    );
}

#[test]
fn browses_downloads_checks_account_quota_and_keeps_opaque_ids() {
    let f = Fixture::new(|_, _| None);
    let b = f.backend("/");
    let entries = b.list_directory("/").unwrap();
    assert_eq!(entries[0].name, "中文 #%.txt");
    assert!(!entries[0].remote_id.starts_with('/'));
    assert_eq!(b.download(&entries[0].remote_id).unwrap(), DATA);
    let usage = b.storage_usage(&mut || Ok(())).unwrap().unwrap();
    assert_eq!(
        usage.free,
        Some(800),
        "free-plan quota must not become remaining bytes"
    );
    assert!(!b.capabilities().upload);
    assert!(b.create_folder("/forbidden").is_err());
    assert!(b.delete(&entries[0].remote_id).is_err());
    let mut snapshot = auth::load(&f.state).unwrap();
    snapshot.generation = "other-login".into();
    auth::save(&f.state, &snapshot).unwrap();
    assert!(b.download(&entries[0].remote_id).is_err());
    assert!(
        !f.requests
            .lock()
            .unwrap()
            .iter()
            .any(|v| v.contains("filemanager"))
    );
}
#[test]
fn resolves_restricted_root_and_rejects_escape_and_repeated_pages() {
    let f = Fixture::new(|u, _| {
        if u.path() == "/rest/2.0/xpan/file" {
            assert!(
                u.query_pairs()
                    .any(|(k, v)| k == "dir" && v == "/apps/TwoDrive")
            );
            Some(json_reply(
                json!({"errno":0,"list":[row(1,"/apps/TwoDrive/文件.txt")]}),
            ))
        } else {
            None
        }
    });
    let b = f.backend("/apps/TwoDrive");
    assert_eq!(b.list_directory("/").unwrap()[0].path, "/文件.txt");
    assert!(b.list_directory("/../private").is_err());
    let outside = Fixture::new(|u, _| {
        if u.path().ends_with("/file") {
            Some(json_reply(
                json!({"errno":0,"list":[row(1,"/apps/Other/escape.txt")]}),
            ))
        } else {
            None
        }
    });
    assert!(
        BaiduDrive::connect_with(
            outside.state.clone(),
            "/apps/TwoDrive",
            outside.api.clone(),
            &mut || Ok(())
        )
        .is_err()
    );
    let repeated = Fixture::new(|u, _| {
        if u.path().ends_with("/file") {
            Some(json_reply(
                json!({"errno":0,"list":(0..1000).map(|i|row(i+1,&format!("/file-{i}.txt"))).collect::<Vec<_>>()}),
            ))
        } else {
            None
        }
    });
    assert!(
        BaiduDrive::connect_with(
            repeated.state.clone(),
            "/",
            repeated.api.clone(),
            &mut || Ok(())
        )
        .is_err()
    );
}
#[test]
fn paginates_without_recursive_inventory_or_url_override() {
    let f = Fixture::new(|u, _| {
        if u.path().ends_with("/file") {
            let start = u
                .query_pairs()
                .find(|(k, _)| k == "start")
                .unwrap()
                .1
                .parse::<u64>()
                .unwrap();
            Some(json_reply(
                json!({"errno":0,"list":if start==0{(0..1000).map(|i|row(i+1,&format!("/file-{i}.txt"))).collect::<Vec<_>>()}else{vec![row(1001,"/last.txt")]}}),
            ))
        } else {
            None
        }
    });
    let b = f.backend("/");
    assert_eq!(b.list_directory("/").unwrap().len(), 1001);
    let api = Api::new("direct").unwrap();
    for url in [
        "https://baidu.com.attacker.invalid/file",
        "http://d.pcs.baidu.com/file",
        "https://user:pass@d.pcs.baidu.com/file",
        "https://d.pcs.baidu.com:8443/file",
    ] {
        assert!(!api.trusted_download(&Url::parse(url).unwrap()));
    }
    assert!(api.trusted_download(&Url::parse("https://d.pcs.baidu.com/file").unwrap()));
}
#[test]
fn downloads_validate_hexadecimal_content_md5() {
    for expected in [
        format!("{:x}", md5::Md5::digest(DATA)),
        format!("{:X}", md5::Md5::digest(DATA)),
    ] {
        let f = Fixture::new(move |u, _| {
            (u.path() == "/content").then(|| {
                Response::from_data(DATA.to_vec())
                    .with_header(Header::from_bytes("Content-MD5", expected.clone()).unwrap())
            })
        });
        let b = f.backend("/");
        let id = b.list_directory("/").unwrap()[0].remote_id.clone();
        assert_eq!(b.download(&id).unwrap(), DATA);
    }
}

#[test]
fn malformed_and_incorrect_content_md5_never_publish_success() {
    for expected in [
        "00000000000000000000000000000000",
        "AAAAAAAAAAAAAAAAAAAAAA==",
        "aW52YWxpZA==",
        "not-a-digest",
        "",
    ] {
        let f = Fixture::new(move |u, _| {
            (u.path() == "/content").then(|| {
                Response::from_data(DATA.to_vec())
                    .with_header(Header::from_bytes("Content-MD5", expected).unwrap())
            })
        });
        let b = f.backend("/");
        let id = b.list_directory("/").unwrap()[0].remote_id.clone();
        assert!(b.download(&id).is_err());
        assert!(auth::load(&f.state).is_ok());
    }
}

#[test]
fn changed_content_md5_and_metadata_cannot_publish_success() {
    let f = Fixture::new(|u, _| {
        if u.path() == "/content" {
            Some(Response::from_data(DATA.to_vec()).with_header(
                Header::from_bytes("Content-MD5", "AAAAAAAAAAAAAAAAAAAAAA==").unwrap(),
            ))
        } else {
            None
        }
    });
    let b = f.backend("/");
    let id = b.list_directory("/").unwrap()[0].remote_id.clone();
    assert!(
        b.download(&id)
            .unwrap_err()
            .to_string()
            .contains("校验失败")
    );
    let count = Arc::new(AtomicUsize::new(0));
    let n = count.clone();
    let changed = Fixture::new(move |u, base| {
        if u.path().ends_with("/multimedia") {
            let mut r = row(10, "/中文 #%.txt");
            if n.fetch_add(1, Ordering::SeqCst) > 0 {
                r["md5"] = json!("changed-version");
            }
            r["dlink"] = json!(format!("{base}/content"));
            Some(json_reply(json!({"errno":0,"list":[r]})))
        } else {
            None
        }
    });
    let b = changed.backend("/");
    let id = b.list_directory("/").unwrap()[0].remote_id.clone();
    assert!(
        b.download(&id)
            .unwrap_err()
            .to_string()
            .contains("期间改变")
    );
}
#[test]
fn error_bodies_and_credentials_never_escape_and_bad_download_hosts_are_rejected() {
    let f = Fixture::new(|u, _| {
        if u.path() == "/api/quota" {
            Some(json_reply(
                json!({"errno":6,"errmsg":"SECRET_access_token=do-not-return"}),
            ))
        } else if u.path().ends_with("/multimedia") {
            let mut r = row(10, "/中文 #%.txt");
            r["dlink"] = json!("https://attacker.invalid/stolen");
            Some(json_reply(json!({"errno":0,"list":[r]})))
        } else {
            None
        }
    });
    let b = f.backend("/");
    let error = b.storage_usage(&mut || Ok(())).unwrap_err().to_string();
    assert!(!error.contains("SECRET"));
    let id = b.list_directory("/").unwrap()[0].remote_id.clone();
    assert!(
        b.download(&id)
            .unwrap_err()
            .to_string()
            .contains("未受信任")
    );
}
#[test]
fn cancelled_reads_and_downloads_stop_without_erasing_login() {
    let f = Fixture::new(|_, _| None);
    let b = f.backend("/");
    let count = f.requests.lock().unwrap().len();
    assert!(
        b.list_directory_checked("/", &mut || anyhow::bail!("cancelled"))
            .is_err()
    );
    assert_eq!(f.requests.lock().unwrap().len(), count);
    let id = b.list_directory("/").unwrap()[0].remote_id.clone();
    let mut output = Vec::new();
    assert!(
        b.download_to(&id, &mut output, &mut |_| anyhow::bail!("cancelled"))
            .is_err()
    );
    assert!(output.is_empty());
    assert!(auth::load(&f.state).is_ok());
}
#[test]
fn refresh_rotation_is_serialized_and_stays_bound_to_same_account() {
    let refreshes = Arc::new(AtomicUsize::new(0));
    let n = refreshes.clone();
    let f = Fixture::new(move |u, _| {
        if u.path() == "/oauth/2.0/token" {
            n.fetch_add(1, Ordering::SeqCst);
            Some(json_reply(
                json!({"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600,"scope":"basic netdisk"}),
            ))
        } else {
            None
        }
    });
    let b = Arc::new(f.backend("/"));
    let mut s = auth::load(&f.state).unwrap();
    s.expires_at = 0;
    auth::save(&f.state, &s).unwrap();
    let jobs: Vec<_> = (0..3)
        .map(|_| {
            let b = b.clone();
            std::thread::spawn(move || b.storage_usage(&mut || Ok(())).unwrap())
        })
        .collect();
    for job in jobs {
        job.join().unwrap();
    }
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(auth::load(&f.state).unwrap().refresh_token, "new-refresh");
}
fn oauth(u: &Url) -> Option<Reply> {
    match u.path() {
        "/oauth/2.0/device/code" => Some(json_reply(
            json!({"device_code":"PRIVATE-DEVICE-CODE","user_code":"ABCD-EFGH","verification_url":"https://openapi.baidu.com/device","expires_in":1800,"interval":5}),
        )),
        "/oauth/2.0/token" => Some(json_reply(
            json!({"access_token":"private-OAUTH-access","refresh_token":"private-OAUTH-refresh","expires_in":3600,"scope":"basic,netdisk"}),
        )),
        _ => None,
    }
}
#[test]
fn graphical_device_flow_cancels_replay_and_preserves_old_login_until_validated() {
    let f = Fixture::new(|u, _| oauth(u));
    let args = json!({"app_key":"test-AppKey","secret_key":"test-SecretKey","root_path":"/","network_route":"direct"});
    let cancel = AtomicBool::new(false);
    let first =
        auth::action_with(&f.state, "configure", &args, &cancel, Some(f.api.clone())).unwrap();
    let public = first.to_string();
    for secret in ["test-SecretKey", "PRIVATE-DEVICE-CODE", "synthetic-ACCESS"] {
        assert!(!public.contains(secret));
    }
    assert_eq!(auth::load(&f.state).unwrap().generation, "login-a");
    let latest =
        auth::action_with(&f.state, "configure", &args, &cancel, Some(f.api.clone())).unwrap();
    assert!(
        auth::action_with(
            &f.state,
            "auth_cancel",
            &json!({"attempt":first["auth"]["attempt"]}),
            &cancel,
            Some(f.api.clone())
        )
        .is_err()
    );
    let done = auth::action_with(
        &f.state,
        "auth_reply",
        &json!({"attempt":latest["auth"]["attempt"],"answer":""}),
        &cancel,
        Some(f.api.clone()),
    )
    .unwrap();
    assert_eq!(done["authenticated"], true);
    assert!(!done.to_string().contains("private-OAUTH"));
    assert_ne!(auth::load(&f.state).unwrap().generation, "login-a");
    auth::logout(&f.state).unwrap();
    assert!(auth::load(&f.state).is_err());
}
#[test]
fn pending_device_authorization_obeys_poll_interval_and_cancellation() {
    let polls = Arc::new(AtomicUsize::new(0));
    let n = polls.clone();
    let f = Fixture::new(move |u, _| {
        if u.path() == "/oauth/2.0/token" {
            n.fetch_add(1, Ordering::SeqCst);
            Some(json_reply(
                json!({"error":"authorization_pending","error_description":"SECRET must not show"}),
            ))
        } else {
            oauth(u)
        }
    });
    let cancel = AtomicBool::new(false);
    let first = auth::action_with(
        &f.state,
        "configure",
        &json!({"app_key":"key","secret_key":"secret"}),
        &cancel,
        Some(f.api.clone()),
    )
    .unwrap();
    let second = auth::action_with(
        &f.state,
        "auth_reply",
        &json!({"attempt":first["auth"]["attempt"]}),
        &cancel,
        Some(f.api.clone()),
    )
    .unwrap();
    assert!(!second.to_string().contains("SECRET"));
    let third = auth::action_with(
        &f.state,
        "auth_reply",
        &json!({"attempt":second["auth"]["attempt"]}),
        &cancel,
        Some(f.api.clone()),
    )
    .unwrap();
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    auth::action_with(
        &f.state,
        "auth_cancel",
        &json!({"attempt":third["auth"]["attempt"]}),
        &cancel,
        Some(f.api.clone()),
    )
    .unwrap();
    assert_eq!(auth::load(&f.state).unwrap().generation, "login-a");
}

#[test]
fn rotated_token_survives_transient_identity_failure_and_retries_without_reauthorizing() {
    let refreshes = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicBool::new(false));
    let calls = refreshes.clone();
    let fail = failed.clone();
    let f = Fixture::new(move |u, _| {
        if u.path() == "/oauth/2.0/token" {
            calls.fetch_add(1, Ordering::SeqCst);
            Some(json_reply(
                json!({"access_token":"rotated-access","refresh_token":"rotated-refresh","expires_in":3600,"scope":"basic netdisk"}),
            ))
        } else if u.path().ends_with("/nas") && fail.swap(false, Ordering::SeqCst) {
            Some(json_reply(json!({"errno":2})))
        } else {
            None
        }
    });
    let b = f.backend("/");
    let mut session = auth::load(&f.state).unwrap();
    session.expires_at = 0;
    auth::save(&f.state, &session).unwrap();
    failed.store(true, Ordering::SeqCst);
    assert!(b.storage_usage(&mut || Ok(())).is_err());
    let saved = auth::load(&f.state).unwrap();
    assert_eq!(saved.refresh_token, "rotated-refresh");
    assert_eq!(
        saved.generation, "login-a",
        "a transient identity failure must not invalidate the retained connection"
    );
    assert!(saved.identity_pending);
    assert!(b.storage_usage(&mut || Ok(())).unwrap().is_some());
    assert!(!auth::load(&f.state).unwrap().identity_pending);
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
}

#[test]
fn changed_refresh_identity_is_rejected_before_file_access() {
    let wrong = Arc::new(AtomicBool::new(false));
    let flag = wrong.clone();
    let f = Fixture::new(move |u, _| {
        if u.path() == "/oauth/2.0/token" {
            Some(json_reply(
                json!({"access_token":"rotated-access","refresh_token":"rotated-refresh","expires_in":3600,"scope":"basic netdisk"}),
            ))
        } else if u.path().ends_with("/nas") && flag.load(Ordering::SeqCst) {
            Some(json_reply(json!({"errno":0,"uk":8})))
        } else {
            None
        }
    });
    let b = f.backend("/");
    let mut session = auth::load(&f.state).unwrap();
    session.expires_at = 0;
    auth::save(&f.state, &session).unwrap();
    wrong.store(true, Ordering::SeqCst);
    let before = f.requests.lock().unwrap().len();
    assert!(b.list_directory("/").is_err());
    assert!(b.list_directory("/").is_err());
    assert_ne!(auth::load(&f.state).unwrap().generation, "login-a");
    assert!(
        !f.requests.lock().unwrap()[before..]
            .iter()
            .any(|v| v.ends_with("/file"))
    );
}

#[test]
fn hexadecimal_content_md5_is_checked_before_mount_range_reads() {
    for valid in [true, false] {
        let f = Fixture::new(move |u, _| {
            (u.path() == "/content").then(|| {
                Response::from_data(DATA.to_vec()).with_header(
                    Header::from_bytes(
                        "Content-MD5",
                        if valid {
                            format!("{:x}", md5::Md5::digest(DATA))
                        } else {
                            "00000000000000000000000000000000".into()
                        },
                    )
                    .unwrap(),
                )
            })
        });
        let cache = tempfile::tempdir().unwrap();
        let bridge = crate::readonly_mount::Bridge::start(
            crate::provider::Backend::new(f.backend("/")),
            cache.path(),
        )
        .unwrap();
        let c = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let head = c
            .head(format!(
                "http://{}/%E4%B8%AD%E6%96%87%20%23%25.txt",
                bridge.address
            ))
            .bearer_auth(&bridge.token)
            .send()
            .unwrap();
        assert_eq!(head.status(), 200);
        assert!(
            !f.requests
                .lock()
                .unwrap()
                .iter()
                .any(|path| path == "/content"),
            "Baidu mount metadata must not fetch the file body"
        );
        let response = c
            .get(format!(
                "http://{}/%E4%B8%AD%E6%96%87%20%23%25.txt",
                bridge.address
            ))
            .bearer_auth(&bridge.token)
            .header("Range", "bytes=2-9")
            .send()
            .unwrap();
        if valid {
            assert_eq!(response.status(), 206);
            assert_eq!(&response.bytes().unwrap()[..], &DATA[2..10]);
        } else {
            assert!(!response.status().is_success());
        }
    }
}

#[test]
fn baidu_adapter_feeds_authenticated_mount_bridge_with_quota_and_ranges() {
    let f = Fixture::new(|_, _| None);
    let cache = tempfile::tempdir().unwrap();
    let bridge = crate::readonly_mount::Bridge::start(
        crate::provider::Backend::new(f.backend("/")),
        cache.path(),
    )
    .unwrap();
    let c = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let root = format!("http://{}/", bridge.address);
    assert_eq!(c.get(&root).send().unwrap().status(), 401);
    let xml = c
        .request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), &root)
        .bearer_auth(&bridge.token)
        .header("Depth", "1")
        .send()
        .unwrap();
    assert_eq!(xml.status(), 207);
    let xml = xml.text().unwrap();
    let doc = roxmltree::Document::parse(&xml).unwrap();
    assert!(
        doc.descendants()
            .filter(|n| n.has_tag_name(("DAV:", "href")))
            .any(|n| {
                percent_encoding::percent_decode_str(n.text().unwrap_or_default())
                    .decode_utf8()
                    .is_ok_and(|v| v == "/中文 #%.txt")
            })
    );
    let url = format!(
        "{root}{}",
        percent_encoding::utf8_percent_encode("中文 #%.txt", percent_encoding::NON_ALPHANUMERIC)
    );
    let result = c
        .get(url)
        .bearer_auth(&bridge.token)
        .header("Range", "bytes=0-7")
        .send()
        .unwrap();
    assert_eq!(result.status(), 206);
    assert_eq!(&result.bytes().unwrap()[..], &DATA[..8]);
    assert_eq!(
        c.put(&root)
            .bearer_auth(&bridge.token)
            .body("forbidden")
            .send()
            .unwrap()
            .status(),
        403
    );
    let quota = c.request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), &root).bearer_auth(&bridge.token).header("Depth","0").body("<D:propfind xmlns:D=\"DAV:\"><D:prop><D:quota-used-bytes/><D:quota-available-bytes/></D:prop></D:propfind>").send().unwrap().text().unwrap();
    assert!(quota.contains(">200<") && quota.contains(">800<"));
}
