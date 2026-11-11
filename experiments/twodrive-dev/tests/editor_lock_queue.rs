#[path = "support/opaque_provider.rs"]
mod opaque;
use opaque::OpaqueProvider;
use reqwest::{Method, blocking::Client};
use std::time::{Duration, Instant};
use twodrive_dev::{model::CloudBackend, provider::Backend, readonly_mount::Bridge};

fn wait(mut ready: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < until, "background queue did not settle");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn call(
    c: &Client,
    bridge: &Bridge,
    method: Method,
    path: &str,
    body: &str,
) -> reqwest::blocking::Response {
    c.request(method, format!("http://{}{path}", bridge.address))
        .bearer_auth(&bridge.token)
        .body(body.to_owned())
        .send()
        .unwrap()
}
fn listing(c: &Client, bridge: &Bridge, path: &str) -> String {
    let response = c
        .request(
            Method::from_bytes(b"PROPFIND").unwrap(),
            format!("http://{}{path}", bridge.address),
        )
        .bearer_auth(&bridge.token)
        .header("Depth", "1")
        .send()
        .unwrap();
    assert_eq!(response.status(), 207);
    response.text().unwrap()
}
#[test]
fn indexed_baidu_mode_keeps_office_lock_mutations_synchronous() {
    let root = tempfile::tempdir().unwrap();
    let mut p = OpaqueProvider::default();
    p.upload_delay = Duration::from_millis(400);
    p.delete_delay = Duration::from_millis(400);
    let spool = root.path().join("recovery");
    let bridge =
        Bridge::start_indexed(Backend::new(p.clone()), root.path(), Some(&spool), false).unwrap();
    let c = Client::builder().no_proxy().build().unwrap();
    let path = "/.~lock.book.xlsx%23";
    let start = Instant::now();
    assert_eq!(
        call(&c, &bridge, Method::PUT, path, "synchronous lock").status(),
        201
    );
    assert!(start.elapsed() >= Duration::from_millis(400));
    assert!(!spool.join("editor-locks/queue.json").exists());
    assert!(
        p.get_metadata_by_path("/.~lock.book.xlsx#")
            .unwrap()
            .is_some()
    );
    let start = Instant::now();
    assert_eq!(call(&c, &bridge, Method::DELETE, path, "").status(), 204);
    assert!(start.elapsed() >= Duration::from_millis(400));
}

#[test]
fn managed_lock_transitions_keep_cached_directory_listings() {
    let root = tempfile::tempdir().unwrap();
    let mut p = OpaqueProvider::default();
    p.upload_delay = Duration::from_millis(600);
    p.create_folder("/other").unwrap();
    p.upload("/other/unchanged.txt", b"keep me".to_vec())
        .unwrap();
    p.upload("/.~lock.occupied.xlsx#", b"another editor".to_vec())
        .unwrap();
    let spool = root.path().join("recovery");
    let bridge =
        Bridge::start_writable_editor_locks(Backend::new(p.clone()), root.path(), &spool).unwrap();
    let c = Client::builder().no_proxy().build().unwrap();
    assert!(listing(&c, &bridge, "/").contains("other"));
    assert!(listing(&c, &bridge, "/other").contains("unchanged.txt"));
    let warm_count = p.directory_lookups.lock().unwrap().len();
    assert_eq!(warm_count, 2);
    let lock = "/.~lock.book.xlsx%23";
    assert_eq!(call(&c, &bridge, Method::PUT, lock, "mine").status(), 201);
    wait(|| {
        let jobs: serde_json::Value =
            serde_json::from_slice(&std::fs::read(spool.join("editor-locks/queue.json")).unwrap())
                .unwrap();
        jobs.as_array()
            .unwrap()
            .iter()
            .any(|j| j["phase"] == "Uploading")
    });
    // Cancel another lock before the occupied worker can start its upload.
    let cancelled = "/.~lock.cancelled.xlsx%23";
    assert_eq!(
        call(&c, &bridge, Method::PUT, cancelled, "cancel").status(),
        201
    );
    assert_eq!(
        call(&c, &bridge, Method::DELETE, cancelled, "").status(),
        204
    );
    assert!(listing(&c, &bridge, "/").contains(".~lock.book.xlsx"));
    assert!(!listing(&c, &bridge, "/").contains("cancelled.xlsx"));
    assert_eq!(p.directory_lookups.lock().unwrap().len(), warm_count);
    wait(|| twodrive_dev::mount_write::recovery_status(&spool).unwrap()["editor_pending"] == 0);
    assert!(listing(&c, &bridge, "/").contains(".~lock.book.xlsx"));
    assert!(listing(&c, &bridge, "/other").contains("unchanged.txt"));
    assert_eq!(p.directory_lookups.lock().unwrap().len(), warm_count);
    assert_eq!(call(&c, &bridge, Method::DELETE, lock, "").status(), 204);
    wait(|| twodrive_dev::mount_write::recovery_status(&spool).unwrap()["editor_pending"] == 0);
    assert!(!listing(&c, &bridge, "/").contains(".~lock.book.xlsx"));
    assert!(listing(&c, &bridge, "/other").contains("unchanged.txt"));
    assert_eq!(p.directory_lookups.lock().unwrap().len(), warm_count);

    // A normal mutation must still invalidate display snapshots. While this
    // clears the cache, also capture the uploaded lock in a real cloud listing
    // so its later precise removal is tested, not just an empty root cache.
    assert_eq!(call(&c, &bridge, Method::PUT, lock, "again").status(), 201);
    wait(|| twodrive_dev::mount_write::recovery_status(&spool).unwrap()["editor_pending"] == 0);
    assert_eq!(
        call(&c, &bridge, Method::PUT, "/ordinary.txt", "ordinary").status(),
        201
    );
    assert!(listing(&c, &bridge, "/").contains("ordinary.txt"));
    assert!(listing(&c, &bridge, "/other").contains("unchanged.txt"));
    let refreshed_count = p.directory_lookups.lock().unwrap().len();
    assert_eq!(refreshed_count, warm_count + 2);
    assert_eq!(call(&c, &bridge, Method::DELETE, lock, "").status(), 204);
    wait(|| twodrive_dev::mount_write::recovery_status(&spool).unwrap()["editor_pending"] == 0);
    assert!(!listing(&c, &bridge, "/").contains(".~lock.book.xlsx"));
    assert!(listing(&c, &bridge, "/other").contains("unchanged.txt"));
    assert_eq!(p.directory_lookups.lock().unwrap().len(), refreshed_count);

    // A rejected write to an unowned existing cloud lock retains the original
    // conservative invalidation semantics, rather than trusting its filename.
    assert_eq!(
        call(&c, &bridge, Method::PUT, "/.~lock.occupied.xlsx%23", "mine").status(),
        409
    );
    assert!(listing(&c, &bridge, "/").contains("occupied.xlsx"));
    assert_eq!(
        p.directory_lookups.lock().unwrap().len(),
        refreshed_count + 1
    );
}
#[test]
fn editor_close_and_same_name_reopen_do_not_wait_for_slow_cloud_mutations() {
    let root = tempfile::tempdir().unwrap();
    let mut p = OpaqueProvider::default();
    p.upload_delay = Duration::from_millis(600);
    p.delete_delay = Duration::from_millis(600);
    p.upload("/book.xlsx", b"original workbook".to_vec())
        .unwrap();
    let spool = root.path().join("recovery");
    let bridge =
        Bridge::start_writable_editor_locks(Backend::new(p.clone()), root.path(), &spool).unwrap();
    let c = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let lock = "/.~lock.book.xlsx%23";
    let started = Instant::now();
    assert_eq!(
        call(&c, &bridge, Method::PUT, lock, "first editor lock").status(),
        201
    );
    assert!(started.elapsed() < Duration::from_millis(400));
    wait(|| {
        let jobs: serde_json::Value =
            serde_json::from_slice(&std::fs::read(spool.join("editor-locks/queue.json")).unwrap())
                .unwrap();
        jobs.as_array()
            .unwrap()
            .iter()
            .any(|j| j["phase"] == "Uploading")
    });
    let started = Instant::now();
    assert_eq!(
        call(&c, &bridge, Method::GET, lock, "").text().unwrap(),
        "first editor lock"
    );
    assert_eq!(call(&c, &bridge, Method::DELETE, lock, "").status(), 204);
    assert_eq!(call(&c, &bridge, Method::HEAD, lock, "").status(), 404);
    assert_eq!(
        call(&c, &bridge, Method::PUT, lock, "second editor lock").status(),
        201
    );
    assert_eq!(
        call(&c, &bridge, Method::GET, lock, "").text().unwrap(),
        "second editor lock"
    );
    assert!(
        started.elapsed() < Duration::from_millis(400),
        "close/read/reopen waited for the cloud"
    );
    wait(|| {
        p.get_metadata_by_path("/.~lock.book.xlsx#")
            .unwrap()
            .is_some_and(|m| p.download(&m.remote_id).unwrap() == b"second editor lock")
    });
    let started = Instant::now();
    assert_eq!(call(&c, &bridge, Method::DELETE, lock, "").status(), 204);
    assert!(started.elapsed() < Duration::from_millis(400));
    wait(|| twodrive_dev::mount_write::recovery_status(&spool).unwrap()["editor_pending"] == 0);
    assert!(
        p.get_metadata_by_path("/.~lock.book.xlsx#")
            .unwrap()
            .is_none()
    );
    assert_eq!(call(&c, &bridge, Method::HEAD, lock, "").status(), 404);
    assert_eq!(
        p.download(
            &p.get_metadata_by_path("/book.xlsx")
                .unwrap()
                .unwrap()
                .remote_id
        )
        .unwrap(),
        b"original workbook"
    );
    // Ordinary contents retain synchronous confirmation and their version guard.
    let started = Instant::now();
    assert_eq!(
        call(&c, &bridge, Method::PUT, "/ordinary.txt", "ordinary data").status(),
        201
    );
    assert!(started.elapsed() >= Duration::from_millis(600));
}
#[test]
fn existing_cloud_locks_and_large_lookalikes_are_not_adopted() {
    let root = tempfile::tempdir().unwrap();
    let p = OpaqueProvider::default();
    let mut locked_caps = p.caps;
    locked_caps.upload = false;
    assert!(
        Bridge::start_writable_editor_locks(
            Backend::new(p.clone().with_caps(locked_caps)),
            root.path(),
            &root.path().join("locked-recovery")
        )
        .is_err()
    );
    p.upload("/.~lock.occupied.xlsx#", b"another editor".to_vec())
        .unwrap();
    let bridge = Bridge::start_writable_editor_locks(
        Backend::new(p.clone()),
        root.path(),
        &root.path().join("recovery"),
    )
    .unwrap();
    let c = Client::builder().no_proxy().build().unwrap();
    assert_eq!(
        call(&c, &bridge, Method::PUT, "/.~lock.occupied.xlsx%23", "mine").status(),
        409
    );
    assert_eq!(
        call(&c, &bridge, Method::GET, "/.~lock.occupied.xlsx%23", "")
            .text()
            .unwrap(),
        "another editor"
    );
    assert_eq!(
        call(
            &c,
            &bridge,
            Method::PUT,
            "/.~lock.large.xlsx%23",
            &"x".repeat(16385)
        )
        .status(),
        409
    );
    assert!(
        p.get_metadata_by_path("/.~lock.large.xlsx#")
            .unwrap()
            .is_none()
    );
    // Normal lookalikes are still ordinary cloud files.
    assert_eq!(
        call(&c, &bridge, Method::PUT, "/not-an-office-lock", "regular").status(),
        201
    );
    assert!(
        p.get_metadata_by_path("/not-an-office-lock")
            .unwrap()
            .is_some()
    );
}

#[test]
fn background_cleanup_keeps_a_cloud_lock_changed_by_another_editor() {
    let root = tempfile::tempdir().unwrap();
    let p = OpaqueProvider::default();
    let spool = root.path().join("recovery");
    let bridge =
        Bridge::start_writable_editor_locks(Backend::new(p.clone()), root.path(), &spool).unwrap();
    let c = Client::builder().no_proxy().build().unwrap();
    let path = "/.~lock.book.xlsx#";
    assert_eq!(
        call(&c, &bridge, Method::PUT, "/.~lock.book.xlsx%23", "mine").status(),
        201
    );
    wait(|| {
        p.get_metadata_by_path(path).unwrap().is_some()
            && twodrive_dev::mount_write::recovery_status(&spool).unwrap()["editor_pending"] == 0
    });
    assert!(listing(&c, &bridge, "/").contains(".~lock.book.xlsx"));
    let warm_count = p.directory_lookups.lock().unwrap().len();
    let before = p.get_metadata_by_path(path).unwrap().unwrap();
    let other = p
        .upload_with_etag(path, b"another editor".to_vec(), Some(&before.etag))
        .unwrap();
    assert_eq!(
        call(&c, &bridge, Method::DELETE, "/.~lock.book.xlsx%23", "").status(),
        204
    );
    wait(|| {
        let jobs: serde_json::Value =
            serde_json::from_slice(&std::fs::read(spool.join("editor-locks/queue.json")).unwrap())
                .unwrap();
        jobs.as_array()
            .unwrap()
            .iter()
            .any(|j| j["phase"] == "Failed")
    });
    assert_eq!(p.download(&other.remote_id).unwrap(), b"another editor");
    assert!(!listing(&c, &bridge, "/").contains(".~lock.book.xlsx"));
    assert_eq!(p.directory_lookups.lock().unwrap().len(), warm_count);
    assert_eq!(
        twodrive_dev::mount_write::recovery_status(&spool).unwrap()["files"],
        1
    );
    assert!(
        std::fs::read_dir(spool.join("editor-locks"))
            .unwrap()
            .any(|e| e.unwrap().path().extension().is_some_and(|v| v == "data"))
    );
}
