#[path = "support/opaque_provider.rs"]
mod opaque;
use opaque::OpaqueProvider;
use reqwest::{Method, blocking::Client};
use std::{io::Write, sync::Arc};
use twodrive_dev::{
    model::CloudBackend,
    provider::{Backend, Capabilities},
    readonly_mount::Bridge,
};

fn call(
    client: &Client,
    bridge: &Bridge,
    method: Method,
    path: &str,
    data: &str,
) -> reqwest::blocking::Response {
    client
        .request(method, format!("http://{}{path}", bridge.address))
        .bearer_auth(&bridge.token)
        .body(data.to_owned())
        .send()
        .unwrap()
}
#[test]
fn editable_bridge_creates_updates_moves_deletes_and_isolates_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let provider = OpaqueProvider::default();
    provider.upload("/file.txt", b"before".to_vec()).unwrap();
    let other = OpaqueProvider::default();
    other
        .upload("/file.txt", b"other account".to_vec())
        .unwrap();
    let bridge = Bridge::start_writable(
        Backend::new(provider.clone()),
        dir.path(),
        &dir.path().join("recovery"),
    )
    .unwrap();
    let c = Client::builder().no_proxy().build().unwrap();
    assert_eq!(
        call(&c, &bridge, Method::GET, "/file.txt", "")
            .text()
            .unwrap(),
        "before"
    );
    assert_eq!(
        call(&c, &bridge, Method::PUT, "/file.txt", "after").status(),
        201
    );
    assert_eq!(
        call(&c, &bridge, Method::GET, "/file.txt", "")
            .text()
            .unwrap(),
        "after"
    );
    assert_eq!(
        call(&c, &bridge, Method::PUT, "/new.txt", "new").status(),
        201
    );
    assert_eq!(
        call(
            &c,
            &bridge,
            Method::from_bytes(b"MKCOL").unwrap(),
            "/folder",
            ""
        )
        .status(),
        204
    );
    let moved = c
        .request(
            Method::from_bytes(b"MOVE").unwrap(),
            format!("http://{}/new.txt", bridge.address),
        )
        .bearer_auth(&bridge.token)
        .header(
            "Destination",
            format!("http://{}/renamed.txt", bridge.address),
        )
        .header("Overwrite", "F")
        .send()
        .unwrap();
    assert_eq!(moved.status(), 204);
    assert!(provider.get_metadata_by_path("/new.txt").unwrap().is_none());
    assert_eq!(
        call(&c, &bridge, Method::DELETE, "/renamed.txt", "").status(),
        204
    );
    assert_eq!(
        other
            .download(
                &other
                    .get_metadata_by_path("/file.txt")
                    .unwrap()
                    .unwrap()
                    .remote_id
            )
            .unwrap(),
        b"other account"
    );
    assert_eq!(
        twodrive_dev::mount_write::recovery_status(&dir.path().join("recovery")).unwrap()["files"],
        0
    );
}
#[test]
fn editor_snapshot_survives_restart_and_conflicts_preserve_exact_local_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let spool = dir.path().join("recovery");
    let p = OpaqueProvider::default();
    let old = p.upload("/file.txt", b"before".to_vec()).unwrap();
    let c = Client::builder().no_proxy().build().unwrap();
    {
        let bridge = Bridge::start_writable(Backend::new(p.clone()), dir.path(), &spool).unwrap();
        assert_eq!(
            call(&c, &bridge, Method::GET, "/file.txt", "")
                .text()
                .unwrap(),
            "before"
        );
    }
    p.upload_with_etag("/file.txt", b"external".to_vec(), Some(&old.etag))
        .unwrap();
    let bridge = Bridge::start_writable(Backend::new(p.clone()), dir.path(), &spool).unwrap();
    assert_eq!(
        call(&c, &bridge, Method::HEAD, "/file.txt", "").status(),
        200
    );
    assert_eq!(
        call(&c, &bridge, Method::PUT, "/file.txt", "my unsaved edit").status(),
        409
    );
    let now = p.get_metadata_by_path("/file.txt").unwrap().unwrap();
    assert_eq!(p.download(&now.remote_id).unwrap(), b"external");
    let contents = std::fs::read_dir(&spool)
        .unwrap()
        .filter_map(Result::ok)
        .find(|p| p.path().extension().is_some_and(|s| s == "content"))
        .unwrap();
    assert_eq!(std::fs::read(contents.path()).unwrap(), b"my unsaved edit");
}
#[test]
fn browse_snapshot_is_invalidated_by_mutations_and_cannot_mask_external_write_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let p = OpaqueProvider::default().with_browse_ttl(std::time::Duration::from_secs(2));
    let old = p.upload("/file.txt", b"before".to_vec()).unwrap();
    let bridge = Bridge::start_writable(
        Backend::new(p.clone()),
        dir.path(),
        &dir.path().join("recovery"),
    )
    .unwrap();
    let c = Client::builder().no_proxy().build().unwrap();
    assert_eq!(
        call(&c, &bridge, Method::HEAD, "/file.txt", "").status(),
        200
    );
    p.upload_with_etag("/file.txt", b"external".to_vec(), Some(&old.etag))
        .unwrap();
    assert_eq!(
        call(&c, &bridge, Method::PUT, "/file.txt", "unsaved edit").status(),
        409
    );
    let now = p.get_metadata_by_path("/file.txt").unwrap().unwrap();
    assert_eq!(p.download(&now.remote_id).unwrap(), b"external");
    let stat = |path: &str| {
        c.request(
            Method::from_bytes(b"PROPFIND").unwrap(),
            format!("http://{}{path}", bridge.address),
        )
        .bearer_auth(&bridge.token)
        .header("Depth", "0")
        .send()
        .unwrap()
        .status()
    };
    assert_eq!(stat("/folder"), 404);
    assert_eq!(
        call(
            &c,
            &bridge,
            Method::from_bytes(b"MKCOL").unwrap(),
            "/folder",
            ""
        )
        .status(),
        204
    );
    assert_eq!(stat("/folder"), 207);
    let moved = c
        .request(
            Method::from_bytes(b"MOVE").unwrap(),
            format!("http://{}/folder", bridge.address),
        )
        .bearer_auth(&bridge.token)
        .header("Destination", format!("http://{}/renamed", bridge.address))
        .header("Overwrite", "F")
        .send()
        .unwrap();
    assert_eq!(moved.status(), 204);
    assert_eq!(stat("/folder"), 404);
    assert_eq!(stat("/renamed"), 207);
    assert_eq!(
        call(&c, &bridge, Method::DELETE, "/renamed", "").status(),
        204
    );
    assert_eq!(stat("/renamed"), 404);
}
#[test]
fn readonly_and_auth_guards_cannot_be_bypassed_by_write_verbs() {
    let dir = tempfile::tempdir().unwrap();
    let p = OpaqueProvider::default();
    let c = Client::builder().no_proxy().build().unwrap();
    let bridge = Bridge::start(Backend::new(p.clone()), dir.path()).unwrap();
    assert_eq!(
        call(&c, &bridge, Method::PUT, "/new", "denied").status(),
        403
    );
    drop(bridge);
    let bridge = Bridge::start_writable(
        Backend::new(p.clone()),
        dir.path(),
        &dir.path().join("recovery"),
    )
    .unwrap();
    assert_eq!(
        c.put(format!("http://{}/new", bridge.address))
            .body("no auth")
            .send()
            .unwrap()
            .status(),
        401
    );
    assert_eq!(call(&c, &bridge, Method::DELETE, "/", "").status(), 409);
    assert_eq!(
        call(&c, &bridge, Method::PUT, "/new?token=no", "denied").status(),
        400
    );
    assert!(p.list_all().unwrap().is_empty());
    let unsupported = OpaqueProvider::default().with_caps(Capabilities::default());
    assert!(
        Bridge::start_writable(
            Backend::new(unsupported),
            dir.path(),
            &dir.path().join("bad")
        )
        .is_err()
    );
}
#[test]
fn interrupted_upload_never_publishes_partial_content() {
    let dir = tempfile::tempdir().unwrap();
    let p = OpaqueProvider::default();
    let spool = dir.path().join("recovery");
    let bridge =
        Arc::new(Bridge::start_writable(Backend::new(p.clone()), dir.path(), &spool).unwrap());
    let mut socket = std::net::TcpStream::connect(bridge.address).unwrap();
    write!(socket,"PUT /new HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Length: 100\r\n\r\npartial",bridge.address,bridge.token).unwrap();
    socket.shutdown(std::net::Shutdown::Write).unwrap();
    let start = std::time::Instant::now();
    loop {
        let found = std::fs::read_dir(&spool)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|s| s == "content"))
            .any(|e| std::fs::read(e.path()).unwrap() == b"partial");
        if found {
            break;
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(3));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(p.get_metadata_by_path("/new").unwrap().is_none());
}

#[test]
fn weak_api_recovery_keeps_both_versions_if_confirmation_does_not_match() {
    let dir = tempfile::tempdir().unwrap();
    let p = OpaqueProvider::default();
    let old = p.upload("/original.txt", b"old content".to_vec()).unwrap();
    let source = dir.path().join("source");
    std::fs::write(&source, b"my edit").unwrap();
    let recovery = twodrive_dev::write_recovery::Recovery::prepare(
        &p,
        dir.path(),
        "/original.txt",
        Some(&source),
        Some(&old),
        &mut || Ok(()),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(recovery.state.dir.join("before.bin")).unwrap(),
        b"old content"
    );
    let wrong = p
        .upload_with_etag(
            "/original.txt",
            b"some other content".to_vec(),
            Some(&old.etag),
        )
        .unwrap();
    assert!(recovery.verify(&p, &wrong, &mut || Ok(())).is_err());
    assert_eq!(
        std::fs::read(recovery.edited.as_ref().unwrap()).unwrap(),
        b"my edit"
    );
    assert!(
        twodrive_dev::write_recovery::snapshot(
            &p,
            "/original.txt",
            Some(&old.remote_id),
            Some(&old.etag)
        )
        .is_err()
    );
}
#[test]
fn revoking_or_restarting_a_write_permit_blocks_every_mutation_but_allows_reads() {
    use twodrive_dev::state::State;
    let dir = tempfile::tempdir().unwrap();
    let state = State::new(dir.path().join("private")).unwrap();
    state.save("write-run.json", &"synthetic-run").unwrap();
    let lease = state.dir.join("remote-grant.json");
    let token = "synthetic-run:remote-grant";
    state.save("remote-grant.json", &token).unwrap();
    let p = OpaqueProvider::default();
    let backend = Backend::new(p.clone()).with_write_access(true, Some((lease, token.into())));
    let input = dir.path().join("source");
    std::fs::write(&input, b"edit").unwrap();
    let m = backend
        .upload_path("/file", &input, None, Arc::new(|_, _| Ok(())))
        .unwrap();
    state
        .save("write-run.json", &"renewed-remote-grant")
        .unwrap();
    assert!(
        backend
            .upload_path("/file", &input, Some(&m.etag), Arc::new(|_, _| Ok(())))
            .is_err()
    );
    assert!(backend.create_folder("/new").is_err());
    assert!(backend.rename(&m.remote_id, "/new").is_err());
    assert!(backend.delete(&m.remote_id).is_err());
    assert_eq!(backend.download(&m.remote_id).unwrap(), b"edit");
}
