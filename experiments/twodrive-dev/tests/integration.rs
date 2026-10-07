use base64::{Engine, engine::general_purpose::STANDARD};
use iroh::{EndpointAddr, SecretKey};
use reqwest::{StatusCode, blocking::Client};
use serde_json::json;
use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    time::Duration,
};
use tempfile::TempDir;
use tokio::{net::TcpListener, task::JoinHandle};
use twodrive_dev::model::CloudBackend;
use twodrive_dev::{
    peer::{self, Network, PeerClient},
    state::{Credentials, Invitation, Share, State, now},
    webdav::WebDavBackend,
};

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    address: SocketAddr,
    credentials: Credentials,
    backend: WebDavBackend,
    task: JoinHandle<anyhow::Result<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn local(writable: bool, limit: u64) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("share");
        fs::create_dir(&root).unwrap();
        let state = State::new(temp.path().join("state")).unwrap();
        let credentials = state.credentials().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_root = root.clone();
        let auth = credentials.clone();
        let task = tokio::spawn(async move {
            peer::local_webdav(listener, auth, &server_root, writable, limit).await
        });
        let url = format!("http://{address}/");
        // Blocking reqwest runtimes must be constructed outside an async context.
        let backend_auth = credentials.clone();
        let backend = tokio::task::spawn_blocking(move || {
            WebDavBackend::new(&url, Some(backend_auth), true).unwrap()
        })
        .await
        .unwrap();
        Self {
            _temp: temp,
            root,
            address,
            credentials,
            backend,
            task,
        }
    }
    fn url(&self) -> String {
        format!("http://{}/", self.address)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn webdav_contract_unicode_versions_moves_and_snapshot_deletions() {
    let fixture = Fixture::local(true, 1024 * 1024).await;
    let backend = fixture.backend.clone();
    let root = fixture.root.clone();
    tokio::task::spawn_blocking(move || {
        assert!(backend.list_all().unwrap().is_empty());
        backend.create_folder("/Documents").unwrap();
        let created = backend
            .upload("/Documents/你好 #%.txt", b"first".to_vec())
            .unwrap();
        assert!(!created.etag.is_empty());
        assert_eq!(backend.download(&created.remote_id).unwrap(), b"first");
        assert_eq!(
            fs::read(root.join("Documents/你好 #%.txt")).unwrap(),
            b"first"
        );
        let before = backend.list_delta(None).unwrap();
        assert_eq!(before.entries.len(), 2);
        let conflict = backend
            .upload("/Documents/你好 #%.txt", b"unversioned overwrite".to_vec())
            .unwrap_err();
        assert!(backend.is_conflict_error(&conflict));
        let updated = backend
            .upload_with_etag(&created.path, b"second".to_vec(), Some(&created.etag))
            .unwrap();
        assert_ne!(created.etag, updated.etag);
        let stale = backend
            .upload_with_etag(&created.path, b"stale".to_vec(), Some(&created.etag))
            .unwrap_err();
        assert!(backend.is_conflict_error(&stale));
        assert_eq!(backend.download(&created.path).unwrap(), b"second");
        let moved = backend
            .rename(&created.remote_id, "/Documents/moved.txt")
            .unwrap();
        assert_eq!(backend.download(&moved.remote_id).unwrap(), b"second");
        backend.delete(&moved.remote_id).unwrap();
        let after = backend.list_delta(before.delta_link.as_deref()).unwrap();
        assert_eq!(after.deleted_remote_ids, [created.remote_id]);
        assert_eq!(after.entries.len(), 1);
        backend.delete("/Documents").unwrap();
        assert!(backend.list_all().unwrap().is_empty());
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streaming_download_cancellation_and_full_file_upload() {
    let fixture = Fixture::local(true, 16 * 1024 * 1024).await;
    let backend = fixture.backend.clone();
    let root = fixture.root.clone();
    let source = fixture._temp.path().join("source.bin");
    tokio::task::spawn_blocking(move || {
        let data: Vec<u8> = (0..5 * 1024 * 1024).map(|n| (n % 251) as u8).collect();
        fs::write(&source, &data).unwrap();
        backend
            .upload_file_with_version("/large.bin", &source, None, None, &mut |_, _| Ok(()))
            .unwrap();
        assert_eq!(fs::read(root.join("large.bin")).unwrap(), data);
        let mut downloaded = Vec::new();
        let mut steps = Vec::new();
        let size = backend
            .download_to("/large.bin", &mut downloaded, &mut |n| {
                steps.push(n);
                Ok(())
            })
            .unwrap();
        assert_eq!(downloaded, data);
        assert_eq!(size, data.len() as u64);
        assert!(steps.len() > 1);
        let mut cancelled = Vec::new();
        assert!(
            backend
                .download_to("/large.bin", &mut cancelled, &mut |_| anyhow::bail!(
                    "cancelled"
                ))
                .is_err()
        );
        assert!(cancelled.len() < data.len());
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn authentication_readonly_and_downgrade_are_rejected() {
    let fixture = Fixture::local(false, 1024).await;
    fs::write(fixture.root.join("visible.txt"), b"visible").unwrap();
    let url = fixture.url();
    let backend = fixture.backend.clone();
    let auth = fixture.credentials.clone();
    tokio::task::spawn_blocking(move || {
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        assert_eq!(
            client.get(&url).send().unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            client
                .get(&url)
                .basic_auth("twodrive", Some("wrong"))
                .send()
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            client
                .get(&url)
                .basic_auth(&auth.username, Some(&auth.password))
                .header("Host", "evil.example")
                .send()
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            client
                .get(&url)
                .basic_auth(&auth.username, Some(&auth.password))
                .header("Origin", "http://evil.example")
                .send()
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(backend.download("/visible.txt").unwrap(), b"visible");
        assert!(backend.upload("/denied.txt", vec![]).is_err());
        assert!(WebDavBackend::new("http://example.org/dav/", None, false).is_err());
        assert!(WebDavBackend::new("http://localhost/dav/", None, false).is_err());
        assert!(WebDavBackend::new("https://user:secret@example.org/dav/", None, false).is_err());
        let readonly = WebDavBackend::new(&url, Some(auth), false).unwrap();
        assert!(readonly.upload("/denied.txt", vec![]).is_err());
    })
    .await
    .unwrap();
    assert!(!fixture.root.join("denied.txt").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupted_and_oversized_put_preserve_original_content() {
    let fixture = Fixture::local(true, 64).await;
    fs::write(fixture.root.join("original.txt"), b"original").unwrap();
    let address = fixture.address;
    let auth = fixture.credentials.clone();
    let root = fixture.root.clone();
    let backend = fixture.backend.clone();
    tokio::task::spawn_blocking(move || {
        let mut socket = TcpStream::connect(address).unwrap();
        let basic = STANDARD.encode(format!("{}:{}", auth.username, auth.password));
        write!(socket, "PUT /original.txt HTTP/1.1\r\nHost: {address}\r\nAuthorization: Basic {basic}\r\nContent-Length: 50\r\n\r\npartial").unwrap();
        socket.flush().unwrap();
        drop(socket);
        for _ in 0..100 { if fs::read_dir(&root).unwrap().count() == 1 { break; } std::thread::sleep(Duration::from_millis(10)); }
        assert_eq!(fs::read(root.join("original.txt")).unwrap(), b"original");
        let entry = backend.get_metadata_by_path("/original.txt").unwrap().unwrap();
        assert!(backend.upload_with_etag("/original.txt", vec![1; 128], Some(&entry.etag)).is_err());
        assert_eq!(fs::read(root.join("original.txt")).unwrap(), b"original");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    }).await.unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn capability_scope_rejects_symlink_escapes_and_special_files() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::local(true, 1024).await;
    let outside = fixture._temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret"), b"must stay private").unwrap();
    symlink(&outside, fixture.root.join("escape")).unwrap();
    symlink(outside.join("secret"), fixture.root.join("link")).unwrap();
    symlink("/dev/zero", fixture.root.join("device")).unwrap();
    let fifo =
        std::ffi::CString::new(fixture.root.join("pipe").as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let backend = fixture.backend.clone();
    tokio::task::spawn_blocking(move || {
        assert!(backend.list_all().unwrap().is_empty());
        for path in [
            "/link",
            "/escape/secret",
            "/device",
            "/pipe",
            "/../outside/secret",
            "/.twodrive-upload-secret",
        ] {
            assert!(backend.download(path).is_err(), "accepted {path}");
        }
        assert!(backend.upload("/escape/created", b"bad".to_vec()).is_err());
        assert!(backend.rename("/link", "/escape/replaced").is_err());
    })
    .await
    .unwrap();
    assert_eq!(
        fs::read(outside.join("secret")).unwrap(),
        b"must stay private"
    );
    assert!(!outside.join("created").exists());
}

#[test]
fn invalid_conditional_tokens_fail_before_network_io() {
    let backend = WebDavBackend::new("http://127.0.0.1:9/", None, true).unwrap();
    for etag in ["*", "bare", "W/\"weak\"", "\"a\",\"b\"", "\"line\nfeed\""] {
        assert!(
            backend
                .upload_with_etag("/file", vec![], Some(etag))
                .unwrap_err()
                .to_string()
                .contains("quoted strong ETag")
        );
    }
}

#[test]
fn single_use_expiry_concurrent_pairing_and_private_state() {
    let temp = tempfile::tempdir().unwrap();
    let state = State::new(temp.path().join("state")).unwrap();
    let root = temp.path().join("share");
    fs::create_dir(&root).unwrap();
    state
        .save(
            "share.json",
            &Share {
                root,
                writable: false,
                address: EndpointAddr::new(SecretKey::generate().public()),
            },
        )
        .unwrap();
    let output = temp.path().join("invite.json");
    state.invite(&output).unwrap();
    let invite: Invitation = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    assert!(!state.authorize("unknown", Some("wrong")).unwrap());
    let one = state.clone();
    let two = state.clone();
    let secret1 = invite.secret.clone();
    let secret2 = invite.secret.clone();
    let first = std::thread::spawn(move || one.authorize("one", Some(&secret1)).unwrap());
    let second = std::thread::spawn(move || two.authorize("two", Some(&secret2)).unwrap());
    assert_ne!(first.join().unwrap(), second.join().unwrap());
    assert_eq!(state.control(|control| Ok(control.peers.len())).unwrap(), 1);
    assert!(!state.authorize("third", Some(&invite.secret)).unwrap());
    let output2 = temp.path().join("invite2.json");
    state.invite(&output2).unwrap();
    let second: Invitation = serde_json::from_slice(&fs::read(output2).unwrap()).unwrap();
    let mut control: serde_json::Value = state.read("control.json").unwrap();
    control["invitation"]["expires"] = json!(now() - 1);
    state.save("control.json", &control).unwrap();
    assert!(!state.authorize("third", Some(&second.secret)).unwrap());
    let key = state.identity().unwrap();
    assert_eq!(
        key.public(),
        State::new(state.dir.clone())
            .unwrap()
            .identity()
            .unwrap()
            .public()
    );
    let _guard = state.run_lock().unwrap();
    assert!(state.run_lock().is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in ["identity.json", "control.json", "share.json"] {
            assert_eq!(
                fs::metadata(state.dir.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert_eq!(
            fs::metadata(&state.dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(output).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let persisted = fs::read_to_string(state.dir.join("control.json")).unwrap();
    assert!(!persisted.contains(&second.secret));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_quic_pairing_streaming_webdav_and_revocation_on_existing_connection() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("share");
    fs::create_dir(&root).unwrap();
    let server_state = State::new(temp.path().join("server")).unwrap();
    let client_state = State::new(temp.path().join("client")).unwrap();
    let stranger_state = State::new(temp.path().join("stranger")).unwrap();
    let network = Network {
        no_relay: true,
        ..Network::default()
    };
    let server = peer::endpoint(&server_state, &network).await.unwrap();
    peer::prepare_share(&server, &server_state, &root, true).unwrap();
    assert!(peer::prepare_share(&server, &server_state, &root, false).is_err());
    assert!(
        server_state
            .invite(&root.join("leaked-invite.json"))
            .is_err()
    );
    let output = temp.path().join("invite.json");
    server_state.invite(&output).unwrap();
    let invitation: Invitation = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    let server_endpoint = server.clone();
    let state = server_state.clone();
    let server_root = root.clone();
    let task = tokio::spawn(async move {
        peer::serve(server_endpoint, state, &server_root, true, 16 * 1024 * 1024).await
    });
    let stranger_endpoint = peer::endpoint(&stranger_state, &network).await.unwrap();
    let stranger = PeerClient::new(stranger_endpoint.clone(), invitation.address.clone());
    assert!(stranger.authenticate(None).await.is_err());
    assert!(
        stranger
            .authenticate(Some("wrong invitation".into()))
            .await
            .is_err()
    );
    let endpoint = peer::endpoint(&client_state, &network).await.unwrap();
    let client = PeerClient::new(endpoint.clone(), invitation.address.clone());
    client.pair(&client_state, Some(invitation)).await.unwrap();
    assert!(stranger.authenticate(None).await.is_err());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = listener.local_addr().unwrap();
    let credentials = client_state.credentials().unwrap();
    let gateway_auth = credentials.clone();
    let gateway = tokio::spawn(async move { peer::gateway(listener, gateway_auth, client).await });
    let backend = tokio::task::spawn_blocking(move || {
        WebDavBackend::new(&format!("http://{local}/"), Some(credentials), true).unwrap()
    })
    .await
    .unwrap();
    let root_clone = root.clone();
    let working = backend.clone();
    tokio::task::spawn_blocking(move || {
        let data = vec![0xa5; 3 * 1024 * 1024];
        working.create_folder("/files").unwrap();
        working.upload("/files/content.bin", data.clone()).unwrap();
        assert_eq!(working.download("/files/content.bin").unwrap(), data);
        assert_eq!(
            fs::read(root_clone.join("files/content.bin")).unwrap(),
            data
        );
        working
            .rename("/files/content.bin", "/files/renamed.bin")
            .unwrap();
        assert_eq!(working.list_all().unwrap().len(), 2);
    })
    .await
    .unwrap();
    let id = endpoint.id().to_string();
    server_state
        .control(|control| {
            assert!(control.peers.remove(&id));
            Ok(())
        })
        .unwrap();
    tokio::task::spawn_blocking(move || {
        assert!(backend.download("/files/renamed.bin").is_err());
        assert!(backend.upload("/denied", vec![]).is_err());
    })
    .await
    .unwrap();
    assert!(!root.join("denied").exists());
    endpoint.close().await;
    stranger_endpoint.close().await;
    server.close().await;
    gateway.abort();
    task.abort();
}

// Deliberately malicious HTTP fixture, independent of the server implementation.
fn fixture_response(response: String) -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let thread = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = [0; 8192];
        let _ = socket.read(&mut request).unwrap();
        socket.write_all(response.as_bytes()).unwrap();
    });
    (format!("http://{address}/dav/"), thread)
}
fn xml_response(href: &str, namespace: &str, properties: &str) -> String {
    let xml = format!(
        "<d:multistatus xmlns:d=\"{namespace}\"><d:response><d:href>{href}</d:href><d:propstat><d:prop>{properties}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"
    );
    format!(
        "HTTP/1.1 207 Multi-Status\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{xml}",
        xml.len()
    )
}
#[test]
fn malicious_dav_metadata_is_rejected_without_false_deletions() {
    for (href, namespace, props) in [
        (
            "http://attacker.invalid/dav/file",
            "DAV:",
            "<d:resourcetype/><d:getcontentlength>1</d:getcontentlength>",
        ),
        (
            "/outside/file",
            "DAV:",
            "<d:resourcetype/><d:getcontentlength>1</d:getcontentlength>",
        ),
        (
            "/dav/escaped%2Ffile",
            "DAV:",
            "<d:resourcetype/><d:getcontentlength>1</d:getcontentlength>",
        ),
        (
            "/dav/file",
            "urn:fake",
            "<d:resourcetype/><d:getcontentlength>1</d:getcontentlength>",
        ),
        (
            "/dav/file",
            "DAV:",
            "<d:getcontentlength>1</d:getcontentlength>",
        ),
    ] {
        let (url, server) = fixture_response(xml_response(href, namespace, props));
        let backend = WebDavBackend::new(&url, None, false).unwrap();
        assert!(
            backend
                .list_delta(Some("dav-snapshot-v1:[\"/previous\"]"))
                .is_err()
        );
        server.join().unwrap();
    }
    let (url, server) = fixture_response("HTTP/1.1 302 Found\r\nLocation: http://attacker.invalid/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into());
    let backend = WebDavBackend::new(
        &url,
        Some(Credentials {
            username: "u".into(),
            password: "s".into(),
        }),
        false,
    )
    .unwrap();
    assert!(
        backend
            .list_all()
            .unwrap_err()
            .to_string()
            .contains("HTTP 302")
    );
    server.join().unwrap();
}
