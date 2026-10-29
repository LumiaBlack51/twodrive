//! Real encrypted local QUIC, with the same backend used by Linux mounts and
//! Windows desktop uploads. No public relay, account, installed service or mount.
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Method, StatusCode, blocking::Client};
use std::{
    fs,
    io::{Cursor, Read, Write},
    net::{Shutdown, TcpStream},
    sync::Arc,
};
use twodrive_dev::{
    delta::{self, Signature},
    model::CloudBackend,
    peer::{self, Network},
    state::{Invitation, State},
    webdav::{UploadKind, UploadMode, WebDavBackend},
};

fn data(seed: u64) -> Vec<u8> {
    let mut x = seed;
    (0..2 * 1024 * 1024)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conditional_delta_full_fallback_corruption_locks_and_interruption_over_quic() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("share");
    fs::create_dir(&root).unwrap();
    let state = State::new(temp.path().join("server")).unwrap();
    let client_state = State::new(temp.path().join("client")).unwrap();
    let network = Network {
        no_relay: true,
        ..Network::default()
    };
    let server = peer::endpoint(&state, &network).await.unwrap();
    peer::prepare_share(&server, &state, &root, true).unwrap();
    let ticket = temp.path().join("invite.json");
    state.invite(&ticket).unwrap();
    let invitation: Invitation = serde_json::from_slice(&fs::read(ticket).unwrap()).unwrap();
    let endpoint = server.clone();
    let exported = root.clone();
    let task = tokio::spawn(async move {
        peer::serve(endpoint, state, &exported, true, 4 * 1024 * 1024).await
    });
    let (endpoint, client) =
        peer::connect_client(&client_state, &network, invitation.address.clone())
            .await
            .unwrap();
    client.pair(&client_state, Some(invitation)).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let credentials = client_state.credentials().unwrap();
    let auth = credentials.clone();
    let gateway = tokio::spawn(async move { peer::gateway(listener, auth, client).await });
    let source = temp.path().join("source");
    tokio::task::spawn_blocking(move || {
        let url = format!("http://{address}");
        let backend = WebDavBackend::new(&url, Some(credentials.clone()), true).unwrap();
        let original = data(0x123456789abc);
        let mut edited = original.clone();
        edited.splice(20000..20000, b"new annotation".iter().copied());
        edited.drain(400000..402000); edited[800000] ^= 31;
        fs::write(root.join("文档.pdf"), &original).unwrap();
        fs::write(&source, &edited).unwrap();
        let entry = backend.get_metadata_by_path("/文档.pdf").unwrap().unwrap();
        let outcome = backend.upload_controlled_with_mode("/文档.pdf", &source, Some(&entry.etag), UploadMode::Auto, Arc::new(|_, _| Ok(()))).unwrap();
        assert_eq!(outcome.kind, UploadKind::Delta);
        assert!(outcome.reused_bytes > edited.len() as u64 * 9 / 10);
        assert!(outcome.uploaded_bytes + outcome.signature_bytes < edited.len() as u64 / 10);
        assert_eq!(fs::read(root.join("文档.pdf")).unwrap(), edited);
        assert!(backend.upload_controlled("/文档.pdf", &source, Some(&entry.etag), Arc::new(|_, _| Ok(()))).is_err());
        assert_eq!(fs::read(root.join("文档.pdf")).unwrap(), edited);
        let full = backend.upload_controlled_with_mode("/文档.pdf", &source, Some(&outcome.entry.etag), UploadMode::Full, Arc::new(|_, _| Ok(()))).unwrap();
        assert_eq!(full.kind, UploadKind::Full);
        assert_eq!(full.uploaded_bytes, edited.len() as u64);
        fs::write(&source, data(0xabcdef)).unwrap();
        let changed = backend.upload_controlled_with_mode("/文档.pdf", &source, Some(&full.entry.etag), UploadMode::Auto, Arc::new(|_, _| Ok(()))).unwrap();
        assert_eq!(changed.kind, UploadKind::Full);
        assert!(changed.signature_bytes > 0);
        let created = backend.upload_controlled_with_mode("/new.pptx", &source, None, UploadMode::Auto, Arc::new(|_, _| Ok(()))).unwrap();
        assert_eq!(created.kind, UploadKind::Full);

        let client = Client::new();
        let file_url = format!("{url}/new.pptx");
        let request = |method: &str| client.request(Method::from_bytes(method.as_bytes()).unwrap(), &file_url).basic_auth(&credentials.username, Some(&credentials.password));
        let signature: Signature = request(delta::SIGNATURE_METHOD).header("If-Match", &created.entry.etag).send().unwrap().error_for_status().unwrap().json().unwrap();
        for path in ["/.twodrive-upload-hidden", "/"] {
            assert_eq!(client.request(Method::from_bytes(delta::SIGNATURE_METHOD.as_bytes()).unwrap(), format!("{url}{path}"))
                .basic_auth(&credentials.username, Some(&credentials.password)).header("If-Match", &created.entry.etag).send().unwrap().status(), StatusCode::FORBIDDEN);
        }
        let current = fs::read(root.join("new.pptx")).unwrap();
        let mut target = current.clone(); target[100000..100050].fill(9);
        let mut patch = Cursor::new(Vec::new());
        delta::encode(&mut target.as_slice(), &signature, &mut patch).unwrap();
        let patch = patch.into_inner();
        let send_patch = |bytes: Vec<u8>, version: &str| request("PATCH").header("If-Match", version).header("Content-Type", delta::MEDIA_TYPE).body(bytes).send().unwrap();
        let mut wrong_base = patch.clone(); wrong_base[8] ^= 1;
        assert_eq!(send_patch(wrong_base, &created.entry.etag).status(), StatusCode::CONFLICT);
        let mut corrupt = patch.clone(); corrupt[48] ^= 1;
        assert_eq!(send_patch(corrupt, &created.entry.etag).status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(send_patch(patch.clone(), "\"stale\"").status(), StatusCode::PRECONDITION_FAILED);
        let mut excessive = patch.clone(); excessive[40..48].copy_from_slice(&(5u64 * 1024 * 1024).to_be_bytes());
        assert_eq!(send_patch(excessive, &created.entry.etag).status(), StatusCode::UNPROCESSABLE_ENTITY);
        // Header-only preflight has no concurrent body writer and must preserve
        // the exact 413 response through the QUIC gateway.
        let authorization = STANDARD.encode(format!("{}:{}", credentials.username, credentials.password));
        let mut preflight = TcpStream::connect(address).unwrap();
        preflight.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        write!(preflight, "PATCH /new.pptx HTTP/1.1\r\nHost: {address}\r\nAuthorization: Basic {authorization}\r\nIf-Match: {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", created.entry.etag, delta::MEDIA_TYPE, 6 * 1024 * 1024).unwrap();
        let mut head = [0; 512];
        let count = preflight.read(&mut head).unwrap();
        assert!(std::str::from_utf8(&head[..count]).unwrap().starts_with("HTTP/1.1 413 "));
        drop(preflight);
        // A sender already writing a rejected oversized body may observe the
        // HTTP rejection, QUIC STOP_SENDING as gateway 502, or the loopback
        // writer's broken pipe. All must reject
        // the mutation; check disk bytes independently below.
        for _ in 0..3 {
            let rejected = request("PATCH").header("If-Match", &created.entry.etag)
                .header("Content-Type", delta::MEDIA_TYPE).body(vec![0; 6 * 1024 * 1024]).send();
            match rejected {
                Ok(response) => assert!(matches!(response.status(), StatusCode::PAYLOAD_TOO_LARGE | StatusCode::BAD_GATEWAY)),
                Err(error) => {
                    // The loopback HTTP hop can also close while reqwest is
                    // writing. Do not accept unrelated connect/timeout errors.
                    let mut source = std::error::Error::source(&error);
                    let mut closed = false;
                    while let Some(cause) = source {
                        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
                            closed |= matches!(io.kind(), std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted);
                        }
                        source = cause.source();
                    }
                    assert!(closed, "unexpected oversized-upload failure: {error}");
                }
            }
            assert_eq!(fs::read(root.join("new.pptx")).unwrap(), current);
        }
        assert_eq!(fs::read(root.join("new.pptx")).unwrap(), current);
        // Interrupted chunked PATCH cannot reach DAV's atomic replacement.
        let authorization = STANDARD.encode(format!("{}:{}", credentials.username, credentials.password));
        let mut socket = TcpStream::connect(address).unwrap();
        socket.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        write!(socket, "PATCH /new.pptx HTTP/1.1\r\nHost: {address}\r\nAuthorization: Basic {authorization}\r\nIf-Match: {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", created.entry.etag, delta::MEDIA_TYPE, patch.len() + 10).unwrap();
        socket.write_all(&patch).unwrap(); socket.shutdown(Shutdown::Write).unwrap();
        let _ = socket.read_to_end(&mut Vec::new());
        assert_eq!(fs::read(root.join("new.pptx")).unwrap(), current);

        let locked = request("LOCK").header("Content-Type", "application/xml").body("<d:lockinfo xmlns:d=\"DAV:\"><d:lockscope><d:exclusive/></d:lockscope><d:locktype><d:write/></d:locktype></d:lockinfo>").send().unwrap().error_for_status().unwrap();
        let token = locked.headers().get("Lock-Token").unwrap().clone(); drop(locked);
        assert_eq!(send_patch(patch.clone(), &created.entry.etag).status().as_u16(), 423);
        assert_eq!(fs::read(root.join("new.pptx")).unwrap(), current);
        request("UNLOCK").header("Lock-Token", token).send().unwrap().error_for_status().unwrap();
        let result = send_patch(patch, &created.entry.etag).error_for_status().unwrap();
        assert_eq!(result.headers()["X-TwoDrive-Upload"], "delta-v1");
        assert_eq!(fs::read(root.join("new.pptx")).unwrap(), target);
        // A cancelled preparation never starts a mutation, including after a
        // completed initial read; a frontend supplies a stable source snapshot.
        fs::write(&source, &target).unwrap();
        let entry = backend.get_metadata_by_path("/new.pptx").unwrap().unwrap();
        assert!(backend.upload_controlled("/new.pptx", &source, Some(&entry.etag), Arc::new(|done, _| { anyhow::ensure!(done < 65536, "cancelled"); Ok(()) })).is_err());
        assert_eq!(fs::read(root.join("new.pptx")).unwrap(), target);
        assert!(!fs::read_dir(&root).unwrap().any(|entry| entry.unwrap().file_name().to_string_lossy().starts_with(".twodrive-upload-")));
    }).await.unwrap();
    endpoint.close().await;
    server.close().await;
    gateway.abort();
    task.abort();
}
