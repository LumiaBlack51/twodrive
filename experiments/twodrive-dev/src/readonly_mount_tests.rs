use super::*;
use crate::{
    model::DeltaResult,
    provider::{Capabilities, Progress, Provider},
};
use std::sync::atomic::{AtomicUsize, Ordering};
const NAME: &str = "你好 & <tag>.txt";
#[derive(Clone)]
struct Fixture {
    data: Arc<Mutex<(Vec<u8>, u64)>>,
    downloads: Arc<AtomicUsize>,
    change_during_read: Arc<AtomicBool>,
    slow_started: Arc<AtomicBool>,
    release_slow: Arc<AtomicBool>,
    block_first: bool,
    metadata_calls: Arc<AtomicUsize>,
    browse_ttl: Duration,
    created_unix: Option<i64>,
}
impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        Self {
            data: Arc::new(Mutex::new((bytes.to_vec(), 1))),
            downloads: Arc::default(),
            change_during_read: Arc::default(),
            slow_started: Arc::default(),
            release_slow: Arc::default(),
            block_first: false,
            metadata_calls: Arc::default(),
            browse_ttl: Duration::ZERO,
            created_unix: None,
        }
    }
    fn meta(&self) -> MetadataEntry {
        let data = self.data.lock().unwrap();
        let mut entry = MetadataEntry::new_file(
            "opaque-id-never-a-path",
            format!("/{NAME}"),
            data.0.len() as u64,
            1,
            data.1.to_string(),
        );
        entry.created_unix = self.created_unix;
        entry
    }
    fn change(&self, bytes: &[u8]) {
        let mut data = self.data.lock().unwrap();
        data.0 = bytes.to_vec();
        data.1 += 1;
    }
    fn other_meta(&self) -> MetadataEntry {
        let data = self.data.lock().unwrap();
        MetadataEntry::new_file(
            "other-object",
            "/other.txt",
            data.0.len() as u64,
            1,
            data.1.to_string(),
        )
    }
}
impl CloudBackend for Fixture {
    fn is_conflict_error(&self, _: &anyhow::Error) -> bool {
        false
    }
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        panic!("whole-drive scans forbidden")
    }
    fn list_delta(&self, _: Option<&str>) -> anyhow::Result<DeltaResult> {
        panic!("delta not needed")
    }
    fn get_metadata(&self, id: &str) -> anyhow::Result<Option<MetadataEntry>> {
        ensure!(matches!(id, "opaque-id-never-a-path" | "other-object"));
        Ok(Some(if id == "other-object" {
            self.other_meta()
        } else {
            self.meta()
        }))
    }
    fn get_metadata_by_path(&self, path: &str) -> anyhow::Result<Option<MetadataEntry>> {
        self.metadata_calls.fetch_add(1, Ordering::SeqCst);
        Ok(if path == "/" {
            Some(MetadataEntry::new_dir("root", "/", 1, "root"))
        } else if path == format!("/{NAME}") {
            Some(self.meta())
        } else if path == "/other.txt" {
            Some(self.other_meta())
        } else {
            None
        })
    }
    fn download(&self, _: &str) -> anyhow::Result<Vec<u8>> {
        panic!("must stream")
    }
    fn download_to(
        &self,
        id: &str,
        writer: &mut dyn Write,
        progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        ensure!(matches!(id, "opaque-id-never-a-path" | "other-object"));
        self.downloads.fetch_add(1, Ordering::SeqCst);
        let data = self.data.lock().unwrap().0.clone();
        if self.block_first && id == "opaque-id-never-a-path" {
            self.slow_started.store(true, Ordering::SeqCst);
            let started = Instant::now();
            while !self.release_slow.load(Ordering::SeqCst) {
                ensure!(
                    started.elapsed() < Duration::from_secs(5),
                    "synthetic slow read was not released"
                );
                progress(0)?;
                thread::sleep(Duration::from_millis(5));
            }
        }
        progress(0)?;
        writer.write_all(&data)?;
        if self.change_during_read.swap(false, Ordering::SeqCst) {
            self.change(b"changed while reading");
        }
        progress(data.len() as u64)?;
        Ok(data.len() as u64)
    }
    fn upload(&self, _: &str, _: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        panic!("mount must never write")
    }
    fn upload_with_etag(
        &self,
        _: &str,
        _: Vec<u8>,
        _: Option<&str>,
    ) -> anyhow::Result<MetadataEntry> {
        panic!("mount must never write")
    }
    fn upload_file_with_version(
        &self,
        _: &str,
        _: &Path,
        _: Option<&str>,
        _: Option<&str>,
        _: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        panic!("mount must never write")
    }
    fn create_folder(&self, _: &str) -> anyhow::Result<MetadataEntry> {
        panic!("mount must never write")
    }
    fn rename(&self, _: &str, _: &str) -> anyhow::Result<MetadataEntry> {
        panic!("mount must never write")
    }
    fn delete(&self, _: &str) -> anyhow::Result<()> {
        panic!("mount must never write")
    }
}
impl Provider for Fixture {
    fn browse_cache_ttl(&self) -> Duration {
        self.browse_ttl
    }
    fn storage_usage(
        &self,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Option<crate::provider::StorageUsage>> {
        check()?;
        Ok(crate::provider::StorageUsage::account(
            Some(1000),
            Some(10),
            Some(990),
        ))
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            upload: true,
            conditional_upload: true,
            mkdir: true,
            rename: true,
            delete: true,
        }
    }
    fn list_directory(&self, path: &str) -> anyhow::Result<Vec<MetadataEntry>> {
        ensure!(path == "/");
        Ok(vec![self.meta()])
    }
    fn upload_controlled(
        &self,
        _: &str,
        _: &Path,
        _: Option<&str>,
        _: Option<&str>,
        _: Progress,
    ) -> anyhow::Result<MetadataEntry> {
        panic!("mount must never write")
    }
}
fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}
fn file_url(bridge: &Bridge) -> String {
    format!(
        "http://{}/{}",
        bridge.address,
        percent_encoding::utf8_percent_encode(NAME, percent_encoding::NON_ALPHANUMERIC)
    )
}
fn request(
    c: &reqwest::blocking::Client,
    bridge: &Bridge,
    method: &str,
    path: &str,
) -> reqwest::blocking::RequestBuilder {
    c.request(
        reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
        format!("http://{}{path}", bridge.address),
    )
    .bearer_auth(&bridge.token)
}
#[test]
fn loopback_auth_readonly_methods_depth_and_xml_escaping() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(b"0123456789");
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    let c = client();
    assert!(bridge.address.ip().is_loopback());
    assert_eq!(c.get(file_url(&bridge)).send().unwrap().status(), 401);
    for method in [
        "PUT",
        "DELETE",
        "MOVE",
        "MKCOL",
        "COPY",
        "LOCK",
        "PROPPATCH",
        "POST",
    ] {
        assert_eq!(
            request(&c, &bridge, method, "/").send().unwrap().status(),
            403
        );
    }
    assert_eq!(
        request(&c, &bridge, "PROPFIND", "/")
            .header("Depth", "infinity")
            .send()
            .unwrap()
            .status(),
        403
    );
    let response = request(&c, &bridge, "PROPFIND", "/")
        .header("Depth", "1")
        .send()
        .unwrap();
    assert_eq!(response.status(), 207);
    let xml = response.text().unwrap();
    let document = roxmltree::Document::parse(&xml).unwrap();
    assert!(
        document
            .descendants()
            .filter(|n| n.has_tag_name(("DAV:", "href")))
            .any(|n| {
                percent_encoding::percent_decode_str(n.text().unwrap_or_default())
                    .decode_utf8()
                    .is_ok_and(|v| v == format!("/{NAME}"))
            })
    );
    assert!(!document.descendants().any(|n| n.has_tag_name("tag")));
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 0);
}
#[test]
fn head_metadata_and_ranges_do_not_hydrate_file_content() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(b"0123456789");
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    let c = client();
    let head = c
        .head(file_url(&bridge))
        .bearer_auth(&bridge.token)
        .send()
        .unwrap();
    assert_eq!(head.status(), 200);
    assert_eq!(head.headers()["Content-Length"], "10");
    let etag = head.headers()["ETag"].clone();
    let range = c
        .head(file_url(&bridge))
        .bearer_auth(&bridge.token)
        .header("Range", "bytes=2-5")
        .send()
        .unwrap();
    assert_eq!(range.status(), 206);
    assert_eq!(range.headers()["Content-Length"], "4");
    let unchanged = c
        .head(file_url(&bridge))
        .bearer_auth(&bridge.token)
        .header("If-None-Match", etag)
        .send()
        .unwrap();
    assert_eq!(unchanged.status(), 304);
    assert_eq!(
        fixture.downloads.load(Ordering::SeqCst),
        0,
        "file information must not download the entire cloud object"
    );
}

#[test]
fn slow_read_does_not_serialize_another_files_read_or_metadata() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture {
        block_first: true,
        ..Fixture::new(b"0123456789")
    };
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    let c = client();
    thread::scope(|scope| {
        let slow = scope.spawn(|| {
            c.get(file_url(&bridge))
                .bearer_auth(&bridge.token)
                .send()
                .unwrap()
                .bytes()
                .unwrap()
        });
        struct Release(Arc<AtomicBool>);
        impl Drop for Release {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let release = Release(fixture.release_slow.clone());
        let started = Instant::now();
        while !fixture.slow_started.load(Ordering::SeqCst) {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "slow request never started"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let metadata = c
            .head(file_url(&bridge))
            .bearer_auth(&bridge.token)
            .timeout(Duration::from_secs(1))
            .send();
        assert!(
            metadata.is_ok_and(|r| r.status().is_success()),
            "metadata waited for a content download"
        );
        let fast = c
            .get(format!("http://{}/other.txt", bridge.address))
            .bearer_auth(&bridge.token)
            .timeout(Duration::from_secs(1))
            .send();
        assert_eq!(
            fast.expect("another file waited for the slow cache lock")
                .bytes()
                .unwrap()
                .as_ref(),
            b"0123456789"
        );
        drop(release);
        assert_eq!(slow.join().unwrap().as_ref(), b"0123456789");
    });
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 2);
}

#[test]
fn independent_range_cursors_cache_versions_and_changed_download_rejection() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(b"0123456789");
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    let c = client();
    let head = c
        .head(file_url(&bridge))
        .bearer_auth(&bridge.token)
        .send()
        .unwrap();
    assert_eq!(head.headers()["Content-Length"], "10");
    let one = |range: &str| {
        let response = c
            .get(file_url(&bridge))
            .bearer_auth(&bridge.token)
            .header("Range", range)
            .send()
            .unwrap();
        assert_eq!(response.status(), 206);
        response.bytes().unwrap().to_vec()
    };
    thread::scope(|s| {
        let a = s.spawn(|| one("bytes=0-3"));
        let b = s.spawn(|| one("bytes=4-9"));
        assert_eq!(a.join().unwrap(), b"0123");
        assert_eq!(b.join().unwrap(), b"456789");
    });
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 1);
    fixture.change(b"new-version");
    assert_eq!(one("bytes=0-2"), b"new");
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 2);
    fixture.change(b"about-to-change");
    fixture.change_during_read.store(true, Ordering::SeqCst);
    let failed = c
        .get(file_url(&bridge))
        .bearer_auth(&bridge.token)
        .send()
        .unwrap();
    assert_eq!(failed.status(), 500);
    assert!(
        !failed
            .bytes()
            .unwrap()
            .windows(15)
            .any(|v| v == b"about-to-change")
    );
    assert_eq!(
        c.get(file_url(&bridge))
            .bearer_auth(&bridge.token)
            .send()
            .unwrap()
            .bytes()
            .unwrap()
            .as_ref(),
        b"changed while reading"
    );
}
#[test]
fn equal_paths_remain_scoped_to_their_provider_and_shutdown_closes_listener() {
    let root = tempfile::tempdir().unwrap();
    let a = Bridge::start(Backend::new(Fixture::new(b"account-a")), root.path()).unwrap();
    let b = Bridge::start(Backend::new(Fixture::new(b"account-b")), root.path()).unwrap();
    let c = client();
    for (bridge, expected) in [(&a, b"account-a"), (&b, b"account-b")] {
        assert_eq!(
            c.get(file_url(bridge))
                .bearer_auth(&bridge.token)
                .send()
                .unwrap()
                .bytes()
                .unwrap()
                .as_ref(),
            expected
        );
    }
    assert_eq!(
        c.get(file_url(&a))
            .bearer_auth(&b.token)
            .send()
            .unwrap()
            .status(),
        401
    );
    let address = a.address;
    drop(a);
    assert!(std::net::TcpStream::connect_timeout(&address, Duration::from_secs(1)).is_err());
}

#[test]
#[ignore = "requires the pinned official local Rclone executable; no cloud account"]
fn official_rclone_reads_the_private_provider_bridge() {
    let exe = std::env::var_os("TWODRIVE_TEST_RCLONE").expect("TWODRIVE_TEST_RCLONE required");
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(b"0123456789");
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    let run = |args: &[&str]| {
        let mut command = std::process::Command::new(&exe);
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("RCLONE_")
            {
                command.env_remove(key);
            }
        }
        command
            .args(args)
            .arg(if cfg!(windows) {
                "--config=NUL"
            } else {
                "--config=/dev/null"
            })
            .args(["--ask-password=false", "--log-level=ERROR"])
            .env("RCLONE_CONFIG_TWODRIVE_TYPE", "webdav")
            .env(
                "RCLONE_CONFIG_TWODRIVE_URL",
                format!("http://{}/", bridge.address),
            )
            .env("RCLONE_CONFIG_TWODRIVE_BEARER_TOKEN", &bridge.token)
            .env("NO_PROXY", "127.0.0.1,localhost");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "official component failed with code {:?}",
            output.status.code()
        );
        output.stdout
    };
    let listing: serde_json::Value =
        serde_json::from_slice(&run(&["lsjson", "TWODRIVE:"])).unwrap();
    assert_eq!(listing[0]["Name"], NAME);
    let quota: serde_json::Value =
        serde_json::from_slice(&run(&["about", "TWODRIVE:", "--json"])).unwrap();
    assert_eq!(quota["used"], 10);
    assert_eq!(quota["total"], 1000);
    let stat: serde_json::Value =
        serde_json::from_slice(&run(&["lsjson", &format!("TWODRIVE:{NAME}"), "--stat"])).unwrap();
    assert_eq!(stat["Size"], 10);
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 0);
    assert_eq!(
        run(&[
            "cat",
            &format!("TWODRIVE:{NAME}"),
            "--offset=3",
            "--count=4"
        ]),
        b"3456"
    );
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 1);
}

#[test]
fn known_creation_time_is_published_in_dav_metadata() {
    let mut entry = MetadataEntry::new_dir("known-id", "/new-dir", 1760000000, "v");
    entry.created_unix = Some(1750000000);
    assert_eq!(
        Meta(entry).created().unwrap(),
        UNIX_EPOCH + Duration::from_secs(1750000000)
    );
    let temp = tempfile::tempdir().unwrap();
    let fixture = Fixture {
        created_unix: Some(1750000000),
        ..Fixture::new(b"test")
    };
    let bridge = Bridge::start(Backend::new(fixture), temp.path()).unwrap();
    let xml = client()
        .request(
            reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
            file_url(&bridge),
        )
        .bearer_auth(&bridge.token)
        .header("Depth", "0")
        .body("<d:propfind xmlns:d=\"DAV:\"><d:prop><d:creationdate/></d:prop></d:propfind>")
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert!(xml.contains("2025-06-15T15:06:40"), "{xml}");
}
#[test]
fn repeated_head_metadata_queries_reuse_short_browse_snapshot_without_downloading() {
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(b"payload");
    fixture.browse_ttl = Duration::from_secs(2);
    let bridge = Bridge::start(Backend::new(fixture.clone()), temp.path()).unwrap();
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    for _ in 0..8 {
        let response = client
            .head(format!("http://{}/other.txt", bridge.address))
            .bearer_auth(&bridge.token)
            .send()
            .unwrap();
        assert!(response.status().is_success());
    }
    assert_eq!(fixture.metadata_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 0);
}
