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
    listings: Arc<AtomicUsize>,
    fail_listing: Arc<AtomicBool>,
    hold_listing: Arc<AtomicBool>,
    empty_listing: Arc<AtomicBool>,
    invalid_listing: Arc<AtomicBool>,
    cancelled_listings: Arc<AtomicUsize>,
    seed_root: bool,
    child_directories: usize,
    named_directories: Vec<String>,
    metadata_reads: Arc<AtomicUsize>,
    change_during_read: Arc<AtomicBool>,
    hold_download: Arc<AtomicBool>,
    download_started: Arc<AtomicBool>,
    hold_quota: Arc<AtomicBool>,
    quota_reads: Arc<AtomicUsize>,
    fail_quota: Arc<AtomicBool>,
    quota_scale: u64,
    include_slow_file: bool,
    probe_metadata_delay: Duration,
    parent_browsing: bool,
    created_unix: Option<i64>,
}
impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        Self {
            data: Arc::new(Mutex::new((bytes.to_vec(), 1))),
            downloads: Arc::default(),
            listings: Arc::default(),
            fail_listing: Arc::default(),
            hold_listing: Arc::default(),
            empty_listing: Arc::default(),
            invalid_listing: Arc::default(),
            cancelled_listings: Arc::default(),
            seed_root: false,
            child_directories: 0,
            named_directories: Vec::new(),
            metadata_reads: Arc::default(),
            change_during_read: Arc::default(),
            hold_download: Arc::default(),
            download_started: Arc::default(),
            hold_quota: Arc::default(),
            quota_reads: Arc::default(),
            fail_quota: Arc::default(),
            quota_scale: 1,
            include_slow_file: false,
            probe_metadata_delay: Duration::ZERO,
            parent_browsing: false,
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
    fn child(&self, path: &str) -> Option<MetadataEntry> {
        if let Some(index) = self
            .named_directories
            .iter()
            .position(|name| path == format!("/{name}"))
        {
            return Some(MetadataEntry::new_dir(
                format!("named-folder-{index}"),
                path,
                1,
                "1",
            ));
        }
        let number = path.strip_prefix("/folder-")?.parse::<usize>().ok()?;
        (number < self.child_directories)
            .then(|| MetadataEntry::new_dir(format!("folder-object-{number}"), path, 1, "1"))
    }
    fn nested_file(&self, path: &str) -> Option<MetadataEntry> {
        let (parent, name) = path.rsplit_once('/')?;
        let index = self
            .named_directories
            .iter()
            .position(|dir| parent == format!("/{dir}"))?;
        (name == NAME).then(|| {
            MetadataEntry::new_file(
                format!("nested-object-{index}"),
                path,
                self.data.lock().unwrap().0.len() as u64,
                1,
                "1",
            )
        })
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
        for name in &self.named_directories {
            let entry = self.nested_file(&format!("/{name}/{NAME}")).unwrap();
            if entry.remote_id == id {
                return Ok(Some(entry));
            }
        }
        ensure!(id == "opaque-id-never-a-path" || id == "slow-object");
        let mut metadata = self.meta();
        if id == "slow-object" {
            metadata.path = "/slow.bin".into();
            metadata.name = "slow.bin".into();
            metadata.remote_id = id.into();
        }
        Ok(Some(metadata))
    }
    fn get_metadata_by_path(&self, path: &str) -> anyhow::Result<Option<MetadataEntry>> {
        self.metadata_reads.fetch_add(1, Ordering::SeqCst);
        if matches!(path, "/.Trash" | "/.Trash-1000") {
            thread::sleep(self.probe_metadata_delay);
        }
        Ok(if path == "/" {
            Some(MetadataEntry::new_dir("root", "/", 1, "root"))
        } else if path == format!("/{NAME}") {
            Some(self.meta())
        } else if path == "/slow.bin" {
            let mut metadata = self.meta();
            metadata.path = path.into();
            metadata.name = "slow.bin".into();
            metadata.remote_id = "slow-object".into();
            Some(metadata)
        } else {
            self.child(path).or_else(|| self.nested_file(path))
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
        ensure!(
            id == "opaque-id-never-a-path"
                || id == "slow-object"
                || id.starts_with("nested-object-")
        );
        self.downloads.fetch_add(1, Ordering::SeqCst);
        if id == "slow-object" {
            self.download_started.store(true, Ordering::SeqCst);
            while self.hold_download.load(Ordering::SeqCst) {
                progress(0)?;
                thread::sleep(Duration::from_millis(5));
            }
        }
        let data = self.data.lock().unwrap().0.clone();
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
#[test]
fn cold_root_display_probes_share_one_complete_listing_before_enumeration() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(b"contents");
    fixture.probe_metadata_delay = Duration::from_millis(600);
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    let c = client();
    let started = Instant::now();
    for path in ["/.Trash", "/.Trash-1000"] {
        assert_eq!(
            request(&c, &bridge, "HEAD", path).send().unwrap().status(),
            404
        );
    }
    assert_eq!(
        fixture.metadata_reads.load(Ordering::SeqCst),
        0,
        "cold display probes must not perform separate cloud stat calls before readdir"
    );
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(root_count(&c, &bridge), 1);
    assert_eq!(fixture.listings.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 0);
}

#[test]
fn failed_root_display_warmup_falls_back_to_live_metadata_and_never_authorizes_bytes() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(b"original");
    fixture.fail_listing.store(true, Ordering::SeqCst);
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    let c = client();
    assert_eq!(
        c.head(file_url(&bridge))
            .bearer_auth(&bridge.token)
            .send()
            .unwrap()
            .status(),
        200
    );
    assert!(fixture.metadata_reads.load(Ordering::SeqCst) > 0);
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 0);
    assert_eq!(
        request(&c, &bridge, "PROPFIND", "/")
            .header("Depth", "1")
            .send()
            .unwrap()
            .status(),
        503,
        "a failed listing cannot be treated as a complete empty root"
    );
    fixture.fail_listing.store(false, Ordering::SeqCst);
    assert_eq!(root_count(&c, &bridge), 1);
    fixture.change(b"fresh remote contents");
    assert_eq!(
        c.get(file_url(&bridge))
            .bearer_auth(&bridge.token)
            .send()
            .unwrap()
            .bytes()
            .unwrap(),
        b"fresh remote contents"[..]
    );
}

#[test]
fn directory_snapshot_avoids_per_file_probes_and_head_never_downloads() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(b"0123456789");
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    let c = client();
    assert_eq!(
        request(&c, &bridge, "PROPFIND", "/")
            .header("Depth", "1")
            .send()
            .unwrap()
            .status(),
        207
    );
    let before = fixture.metadata_reads.load(Ordering::SeqCst);
    for _ in 0..4 {
        let response = c
            .head(file_url(&bridge))
            .bearer_auth(&bridge.token)
            .send()
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-length"], "10");
        assert!(response.bytes().unwrap().is_empty());
        assert_eq!(
            request(&c, &bridge, "PROPFIND", "/.hidden")
                .header("Depth", "0")
                .send()
                .unwrap()
                .status(),
            404
        );
    }
    assert_eq!(
        fixture.downloads.load(Ordering::SeqCst),
        0,
        "HEAD must never materialize cloud file contents"
    );
    assert_eq!(
        fixture.metadata_reads.load(Ordering::SeqCst),
        before,
        "fresh complete directory metadata must cover existing and absent children"
    );
    assert_eq!(
        c.get(file_url(&bridge))
            .bearer_auth(&bridge.token)
            .send()
            .unwrap()
            .bytes()
            .unwrap(),
        b"0123456789"[..]
    );
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 1);
}
#[test]
fn stale_directory_snapshot_cannot_serve_old_file_bytes() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(b"old-content");
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    let c = client();
    assert_eq!(
        request(&c, &bridge, "PROPFIND", "/")
            .header("Depth", "1")
            .send()
            .unwrap()
            .status(),
        207
    );
    fixture.change(b"new-content-with-different-size");
    let first = c
        .get(file_url(&bridge))
        .bearer_auth(&bridge.token)
        .send()
        .unwrap();
    assert_eq!(first.headers()["content-length"], "31");
    assert_eq!(
        first.bytes().unwrap(),
        b"new-content-with-different-size"[..]
    );
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 1);
    assert_eq!(
        c.get(file_url(&bridge))
            .bearer_auth(&bridge.token)
            .send()
            .unwrap()
            .bytes()
            .unwrap(),
        b"new-content-with-different-size"[..]
    );
}
#[test]
fn get_rechecks_empty_file_size_before_publishing_headers() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(b"");
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    let c = client();
    assert_eq!(
        request(&c, &bridge, "PROPFIND", "/")
            .header("Depth", "1")
            .send()
            .unwrap()
            .status(),
        207
    );
    fixture.change(b"now not empty");
    let response = c
        .get(file_url(&bridge))
        .bearer_auth(&bridge.token)
        .send()
        .unwrap();
    assert_eq!(response.headers()["content-length"], "13");
    assert_eq!(response.bytes().unwrap(), b"now not empty"[..]);
}
impl Provider for Fixture {
    fn initial_directory_listing(&self) -> Option<(Instant, Vec<MetadataEntry>)> {
        self.seed_root.then(|| {
            (
                Instant::now(),
                if self.invalid_listing.load(Ordering::SeqCst) {
                    vec![self.meta(), self.meta()]
                } else {
                    vec![self.meta()]
                },
            )
        })
    }
    fn browse_parent_metadata(&self) -> bool {
        self.parent_browsing
    }
    fn storage_usage(
        &self,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Option<crate::provider::StorageUsage>> {
        self.quota_reads.fetch_add(1, Ordering::SeqCst);
        while self.hold_quota.load(Ordering::SeqCst) {
            check()?;
            thread::sleep(Duration::from_millis(5));
        }
        check()?;
        ensure!(
            !self.fail_quota.load(Ordering::SeqCst),
            "synthetic quota failure"
        );
        Ok(crate::provider::StorageUsage::account(
            Some(1000 * self.quota_scale),
            Some(10 * self.quota_scale),
            Some(990 * self.quota_scale),
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
        self.list_directory_checked(path, &mut || Ok(()))
    }
    fn list_directory_checked(
        &self,
        path: &str,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Vec<MetadataEntry>> {
        ensure!(path == "/" || self.child(path).is_some());
        self.listings.fetch_add(1, Ordering::SeqCst);
        loop {
            if let Err(error) = check() {
                self.cancelled_listings.fetch_add(1, Ordering::SeqCst);
                return Err(error);
            }
            if !self.hold_listing.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        ensure!(
            !self.fail_listing.load(Ordering::SeqCst),
            "synthetic listing failure"
        );
        Ok(if self.empty_listing.load(Ordering::SeqCst) {
            vec![]
        } else if path != "/" {
            self.nested_file(&format!("{path}/{NAME}"))
                .into_iter()
                .collect()
        } else if self.invalid_listing.load(Ordering::SeqCst) {
            vec![self.meta(), self.meta()]
        } else {
            let mut entries = vec![self.meta()];
            if self.include_slow_file {
                entries.push(self.get_metadata("slow-object")?.unwrap());
            }
            entries.extend(
                (0..self.child_directories)
                    .map(|number| self.child(&format!("/folder-{number}")).unwrap()),
            );
            entries.extend(
                self.named_directories
                    .iter()
                    .map(|name| self.child(&format!("/{name}")).unwrap()),
            );
            entries
        })
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
fn encoded_subdirectory_propfind_uses_the_same_decoded_path_as_the_handler() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(b"nested synthetic bytes");
    fixture.named_directories = [
        "中文 目录",
        "space folder",
        "hash#percent%",
        "literal%2F..%2F",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    let c = client();
    assert_eq!(root_count(&c, &bridge), 5);
    for name in &fixture.named_directories {
        let encoded =
            percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC);
        for suffix in ["", "/"] {
            let path = format!("/{encoded}{suffix}");
            let response = request(&c, &bridge, "PROPFIND", &path)
                .header("Depth", "1")
                .send()
                .unwrap();
            assert_eq!(response.status(), 207, "encoded directory: {name}");
            let xml = response.text().unwrap();
            let document = roxmltree::Document::parse(&xml).unwrap();
            let hrefs: Vec<_> = document
                .descendants()
                .filter(|node| node.has_tag_name(("DAV:", "href")))
                .map(|node| {
                    percent_encoding::percent_decode_str(node.text().unwrap())
                        .decode_utf8()
                        .unwrap()
                        .into_owned()
                })
                .collect();
            assert_eq!(hrefs.len(), 2);
            assert!(hrefs.contains(&format!("/{name}/{NAME}")));
        }
    }
    assert_eq!(
        fixture.downloads.load(Ordering::SeqCst),
        0,
        "listing must not read file content"
    );
    for name in &fixture.named_directories {
        let path = format!("/{name}/{NAME}");
        let encoded = path
            .split('/')
            .map(|segment| {
                percent_encoding::utf8_percent_encode(segment, percent_encoding::NON_ALPHANUMERIC)
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("/");
        let response = request(&c, &bridge, "GET", &encoded).send().unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.bytes().unwrap().as_ref(),
            b"nested synthetic bytes"
        );
    }
}

#[test]
fn directory_preflight_rejects_encoded_separators_and_invalid_paths() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(b"synthetic bytes");
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    for path in ["/a%2Fb", "/%00", "/%FF", "/%2e%2e/private"] {
        let response = request(&client(), &bridge, "PROPFIND", path)
            .header("Depth", "1")
            .send()
            .unwrap();
        assert!(!response.status().is_success());
    }
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 0);
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
fn a_slow_file_cannot_block_an_unrelated_download() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(b"verified bytes");
    fixture.hold_download.store(true, Ordering::SeqCst);
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    let slow_url = format!("http://{}/slow.bin", bridge.address);
    let fast_url = file_url(&bridge);
    let token = bridge.token.clone();
    let slow = thread::spawn(move || client().get(slow_url).bearer_auth(token).send());
    wait_until(|| fixture.download_started.load(Ordering::SeqCst));
    let (send, receive) = mpsc::channel();
    let token = bridge.token.clone();
    let fast = thread::spawn(move || {
        let response = client().get(fast_url).bearer_auth(token).send();
        send.send(response.and_then(|r| r.error_for_status()?.bytes()))
            .unwrap();
    });
    let result = receive.recv_timeout(Duration::from_secs(2));
    // Always release and join the synthetic slow request before asserting.
    fixture.hold_download.store(false, Ordering::SeqCst);
    assert_eq!(
        slow.join().unwrap().unwrap().bytes().unwrap(),
        b"verified bytes"[..]
    );
    fast.join().unwrap();
    assert_eq!(
        result
            .expect("unrelated GET waited behind another file download")
            .unwrap(),
        b"verified bytes"[..]
    );
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 2);
}

#[test]
fn cached_get_uses_one_live_metadata_lookup_per_http_request() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(b"verified bytes");
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    let c = client();
    let get = || {
        c.get(file_url(&bridge))
            .bearer_auth(&bridge.token)
            .send()
            .unwrap()
            .bytes()
            .unwrap()
    };
    assert_eq!(get(), b"verified bytes"[..]);
    fixture.metadata_reads.store(0, Ordering::SeqCst);
    assert_eq!(get(), b"verified bytes"[..]);
    assert_eq!(
        fixture.metadata_reads.load(Ordering::SeqCst),
        1,
        "one GET must not repeat remote path lookups before reusing verified bytes"
    );
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 1);
    fixture.change(b"new remote version");
    assert_eq!(get(), b"new remote version"[..]);
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 2);
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
}

#[test]
fn failed_directory_read_is_not_reported_as_a_successful_empty_directory() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(b"contents");
    fixture.fail_listing.store(true, Ordering::SeqCst);
    let bridge = Bridge::start(Backend::new(fixture), root.path()).unwrap();
    let response = request(&client(), &bridge, "PROPFIND", "/")
        .header("Depth", "1")
        .send()
        .unwrap();
    assert_eq!(
        response.status(),
        503,
        "failed enumeration must be visible before DAV sends a success header"
    );
}

#[test]
fn repeated_directory_display_reuses_a_complete_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(b"contents");
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    let c = client();
    for _ in 0..4 {
        let response = request(&c, &bridge, "PROPFIND", "/")
            .header("Depth", "1")
            .send()
            .unwrap();
        assert_eq!(response.status(), 207);
        let xml = response.text().unwrap();
        assert!(!xml.contains("opaque-id-never-a-path"));
        let document = roxmltree::Document::parse(&xml).unwrap();
        assert_eq!(
            document
                .descendants()
                .filter(|n| n.has_tag_name(("DAV:", "response")))
                .count(),
            2
        );
    }
    assert_eq!(
        fixture.listings.load(Ordering::SeqCst),
        1,
        "a fresh directory must not queue a new cloud operation on each display"
    );
}

#[test]
fn editable_navigation_reuses_display_without_weakening_live_reads() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(b"contents");
    let bridge = Bridge::start_writable(
        Backend::new(fixture.clone()),
        root.path(),
        &root.path().join("recovery"),
    )
    .unwrap();
    let c = client();
    for _ in 0..4 {
        let response = request(&c, &bridge, "PROPFIND", "/")
            .header("Depth", "1")
            .send()
            .unwrap();
        assert_eq!(response.status(), 207);
        response.text().unwrap();
    }
    assert_eq!(
        fixture.listings.load(Ordering::SeqCst),
        1,
        "editable navigation must retain the display snapshot optimization"
    );
    let before = fixture.metadata_reads.load(Ordering::SeqCst);
    let response = request(
        &c,
        &bridge,
        "HEAD",
        &format!(
            "/{}",
            percent_encoding::utf8_percent_encode(NAME, percent_encoding::NON_ALPHANUMERIC)
        ),
    )
    .send()
    .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(fixture.metadata_reads.load(Ordering::SeqCst), before);
    fixture.change(b"external version");
    assert_eq!(
        request(
            &c,
            &bridge,
            "GET",
            &format!(
                "/{}",
                percent_encoding::utf8_percent_encode(NAME, percent_encoding::NON_ALPHANUMERIC)
            )
        )
        .send()
        .unwrap()
        .bytes()
        .unwrap(),
        &b"external version"[..]
    );
    assert!(fixture.metadata_reads.load(Ordering::SeqCst) > before);
}

#[test]
fn editable_mutation_epoch_rejects_an_older_inflight_listing() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(b"contents");
    fixture.hold_listing.store(true, Ordering::SeqCst);
    let bridge = Bridge::start_writable(
        Backend::new(fixture.clone()),
        root.path(),
        &root.path().join("recovery"),
    )
    .unwrap();
    let fs = bridge.fs.clone();
    let read = thread::spawn(move || fs.fetch_directory("/"));
    wait_until(|| fixture.listings.load(Ordering::SeqCst) == 1);
    bridge.fs.invalidate_display();
    fixture.hold_listing.store(false, Ordering::SeqCst);
    assert!(read.join().unwrap().is_err());
    assert!(bridge.fs.0.directories.lock().unwrap().samples.is_empty());
    assert_eq!(root_count(&client(), &bridge), 1);
}

#[test]
fn browsing_many_children_keeps_the_active_parent_directory_cached() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(b"contents");
    fixture.child_directories = 96;
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    let c = client();
    assert_eq!(root_count(&c, &bridge), 97);
    for number in 0..fixture.child_directories {
        assert_eq!(
            request(&c, &bridge, "PROPFIND", &format!("/folder-{number}"))
                .header("Depth", "1")
                .send()
                .unwrap()
                .status(),
            207
        );
    }
    let before = fixture.listings.load(Ordering::SeqCst);
    fixture.hold_listing.store(true, Ordering::SeqCst);
    let returned = request(&c, &bridge, "PROPFIND", "/")
        .header("Depth", "1")
        .timeout(Duration::from_millis(700))
        .send();
    fixture.hold_listing.store(false, Ordering::SeqCst);
    assert_eq!(
        returned
            .expect("returning to the active parent refetched a slow cloud listing")
            .status(),
        207
    );
    assert_eq!(
        fixture.listings.load(Ordering::SeqCst),
        before,
        "child-count probes must not evict and reload their active parent"
    );
    assert!(bridge.fs.0.directories.lock().unwrap().samples.len() <= 64);
}

fn expire_display(bridge: &Bridge, age: Duration) {
    let mut cache = bridge.fs.0.directories.lock().unwrap();
    let sample = cache.samples.iter_mut().find(|s| s.path == "/").unwrap();
    sample.checked = Instant::now() - age;
    sample.attempted = Instant::now() - age;
}
fn wait_until(mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "synthetic worker timed out"
        );
        thread::sleep(Duration::from_millis(5));
    }
}
fn root_count(c: &reqwest::blocking::Client, bridge: &Bridge) -> usize {
    let response = request(c, bridge, "PROPFIND", "/")
        .header("Depth", "1")
        .send()
        .unwrap();
    assert_eq!(response.status(), 207);
    let xml = response.text().unwrap();
    let doc = roxmltree::Document::parse(&xml).unwrap();
    doc.descendants()
        .filter(|n| n.has_tag_name(("DAV:", "response")))
        .count()
        - 1
}
#[test]
fn slow_background_refresh_does_not_block_display_and_shutdown_cancels_it() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(b"data");
    fixture.seed_root = true;
    fixture.hold_listing.store(true, Ordering::SeqCst);
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    let c = client();
    assert_eq!(root_count(&c, &bridge), 1);
    assert_eq!(
        fixture.listings.load(Ordering::SeqCst),
        0,
        "reuse the already validated connection listing"
    );
    expire_display(&bridge, DIRECTORY_FRESH + Duration::from_secs(1));
    assert_eq!(root_count(&c, &bridge), 1);
    wait_until(|| fixture.listings.load(Ordering::SeqCst) == 1);
    // The provider cannot finish until released. These must complete anyway.
    for _ in 0..8 {
        assert_eq!(root_count(&c, &bridge), 1);
    }
    assert_eq!(
        fixture.listings.load(Ordering::SeqCst),
        1,
        "one refresh in flight"
    );
    let start = Instant::now();
    drop(bridge);
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(fixture.cancelled_listings.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 0);
}
#[test]
fn refresh_failure_preserves_snapshot_but_a_verified_empty_result_replaces_it() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(b"data");
    fixture.seed_root = true;
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    let c = client();
    fixture.fail_listing.store(true, Ordering::SeqCst);
    expire_display(&bridge, DIRECTORY_FRESH + Duration::from_secs(1));
    assert_eq!(root_count(&c, &bridge), 1);
    wait_until(|| {
        fixture.listings.load(Ordering::SeqCst) == 1
            && bridge.fs.0.directories.lock().unwrap().pending.is_empty()
    });
    assert_eq!(root_count(&c, &bridge), 1);
    assert_eq!(
        fixture.listings.load(Ordering::SeqCst),
        1,
        "failed refresh uses retry backoff"
    );
    fixture.fail_listing.store(false, Ordering::SeqCst);
    fixture.empty_listing.store(true, Ordering::SeqCst);
    expire_display(&bridge, DIRECTORY_FRESH + Duration::from_secs(1));
    assert_eq!(root_count(&c, &bridge), 1);
    wait_until(|| {
        fixture.listings.load(Ordering::SeqCst) == 2
            && bridge.fs.0.directories.lock().unwrap().pending.is_empty()
    });
    assert_eq!(
        root_count(&c, &bridge),
        0,
        "a successful empty listing is authoritative"
    );
}
#[test]
fn indexed_subdirectory_stays_visible_after_expiry_and_parent_refresh_failure() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(b"current contents");
    fixture.named_directories = vec!["章节 空格%2F".into()];
    let bridge = Bridge::start_icloud(Backend::new(fixture.clone()), root.path(), None).unwrap();
    let path = "/章节 空格%2F";
    bridge.fs.directory_snapshot("/").unwrap();
    bridge.fs.directory_snapshot(path).unwrap();
    expire_display(&bridge, DIRECTORY_MAX_AGE + Duration::from_secs(60));
    {
        let mut cache = bridge.fs.0.directories.lock().unwrap();
        for sample in &mut cache.samples {
            sample.checked = Instant::now() - DIRECTORY_MAX_AGE - Duration::from_secs(60);
            sample.attempted = sample.checked;
        }
    }
    fixture.fail_listing.store(true, Ordering::SeqCst);
    const DISPLAY_PATH: &percent_encoding::AsciiSet =
        &percent_encoding::CONTROLS.add(b' ').add(b'%');
    let encoded = percent_encoding::utf8_percent_encode(path, DISPLAY_PATH).to_string();
    let c = client();
    let response = request(&c, &bridge, "PROPFIND", &encoded)
        .header("Depth", "1")
        .send()
        .unwrap();
    assert_eq!(
        response.status(),
        207,
        "a previously verified subdirectory must remain visible while offline"
    );
    let text = response.text().unwrap();
    let xml = roxmltree::Document::parse(&text).unwrap();
    assert_eq!(
        xml.descendants()
            .filter(|n| n.has_tag_name(("DAV:", "response")))
            .count(),
        2
    );
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 0);
}

#[test]
fn indexed_nested_attribute_preflight_and_listing_share_shallow_parent_reads() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(b"contents");
    fixture.named_directories = vec!["章节 空格%2F".into()];
    let bridge =
        Bridge::start_indexed(Backend::new(fixture.clone()), root.path(), None, false).unwrap();
    const DISPLAY_PATH: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
        .add(b' ')
        .add(b'%')
        .add(b'&')
        .add(b'<')
        .add(b'>');
    let folder = "/章节 空格%2F";
    let file = percent_encoding::utf8_percent_encode(&format!("{folder}/{NAME}"), DISPLAY_PATH)
        .to_string();
    let encoded_folder = percent_encoding::utf8_percent_encode(folder, DISPLAY_PATH).to_string();
    let c = client();
    // Native navigation checks the directory before querying its children,
    // establishing its identity in the complete parent index.
    assert_eq!(
        request(&c, &bridge, "PROPFIND", &encoded_folder)
            .header("Depth", "0")
            .send()
            .unwrap()
            .status(),
        207
    );
    for _ in 0..3 {
        assert_eq!(
            request(&c, &bridge, "HEAD", &file).send().unwrap().status(),
            200
        );
        assert_eq!(
            request(&c, &bridge, "HEAD", &format!("{encoded_folder}/missing"))
                .send()
                .unwrap()
                .status(),
            404
        );
    }
    assert_eq!(fixture.listings.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.metadata_reads.load(Ordering::SeqCst), 0);
    let response = request(&c, &bridge, "PROPFIND", &encoded_folder)
        .header("Depth", "1")
        .send()
        .unwrap();
    assert_eq!(response.status(), 207);
    assert_eq!(
        fixture.listings.load(Ordering::SeqCst),
        2,
        "only root and the requested child may be listed"
    );
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 0);
    fixture.change(b"live nested contents after a remote edit");
    assert_eq!(
        request(&c, &bridge, "GET", &file)
            .send()
            .unwrap()
            .bytes()
            .unwrap(),
        b"live nested contents after a remote edit"[..]
    );
    assert!(fixture.metadata_reads.load(Ordering::SeqCst) > 0);
}

#[test]
fn verified_parent_removal_or_replacement_invalidates_retained_child_snapshots() {
    for replaced in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut fixture = Fixture::new(b"contents");
        fixture.named_directories = vec!["folder".into()];
        let bridge =
            Bridge::start_indexed(Backend::new(fixture.clone()), root.path(), None, false).unwrap();
        bridge.fs.directory_snapshot("/").unwrap();
        bridge.fs.directory_snapshot("/folder").unwrap();
        let entries = RemoteFs::validated_entries(
            "/",
            if replaced {
                vec![MetadataEntry::new_dir(
                    "replacement-directory",
                    "/folder",
                    1,
                    "new",
                )]
            } else {
                vec![]
            },
        )
        .unwrap();
        {
            let mut cache = bridge.fs.0.directories.lock().unwrap();
            let epoch = cache.generation;
            RemoteFs::remember(&mut cache, "/".into(), entries);
            assert_ne!(cache.generation, epoch);
            assert!(!cache.samples.iter().any(|s| s.path == "/folder"));
        }
        fixture.fail_listing.store(true, Ordering::SeqCst);
        assert_eq!(
            request(&client(), &bridge, "PROPFIND", "/folder")
                .header("Depth", "1")
                .send()
                .unwrap()
                .status(),
            if replaced { 503 } else { 404 }
        );
    }
}

#[test]
fn parent_refresh_fences_a_child_listing_already_in_flight() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(b"contents");
    fixture.named_directories = vec!["folder".into()];
    let bridge =
        Bridge::start_indexed(Backend::new(fixture.clone()), root.path(), None, false).unwrap();
    bridge.fs.directory_snapshot("/").unwrap();
    fixture.hold_listing.store(true, Ordering::SeqCst);
    let fs = bridge.fs.clone();
    let child = thread::spawn(move || fs.directory_snapshot("/folder"));
    wait_until(|| fixture.listings.load(Ordering::SeqCst) == 2);
    {
        let mut cache = bridge.fs.0.directories.lock().unwrap();
        RemoteFs::remember(
            &mut cache,
            "/".into(),
            RemoteFs::validated_entries("/", vec![]).unwrap(),
        );
    }
    fixture.hold_listing.store(false, Ordering::SeqCst);
    assert!(
        child.join().unwrap().is_err(),
        "an old child response cannot undo a verified parent removal"
    );
    assert!(
        !bridge
            .fs
            .0
            .directories
            .lock()
            .unwrap()
            .samples
            .iter()
            .any(|s| s.path == "/folder")
    );
}

#[test]
fn a_verified_parent_without_old_lineage_evicts_orphaned_cached_descendants() {
    let mut cache = DirectoryCache {
        retain_directories: true,
        ..DirectoryCache::default()
    };
    RemoteFs::remember(
        &mut cache,
        "/parent/child/grandchild".into(),
        Arc::new(BTreeMap::new()),
    );
    let parent = RemoteFs::validated_entries(
        "/parent",
        vec![MetadataEntry::new_dir(
            "unknown-child-id",
            "/parent/child",
            1,
            "1",
        )],
    )
    .unwrap();
    RemoteFs::remember(&mut cache, "/parent".into(), parent);
    assert!(
        !cache
            .samples
            .iter()
            .any(|s| s.path == "/parent/child/grandchild")
    );
}

#[test]
fn icloud_root_index_survives_idle_expiry_and_slow_refresh_without_blocking() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(b"contents");
    fixture.seed_root = true;
    fixture.hold_listing.store(true, Ordering::SeqCst);
    let bridge = Bridge::start_icloud(Backend::new(fixture.clone()), root.path(), None).unwrap();
    let c = client();
    expire_display(&bridge, DIRECTORY_MAX_AGE + Duration::from_secs(60));
    let start = Instant::now();
    assert_eq!(root_count(&c, &bridge), 1);
    wait_until(|| fixture.listings.load(Ordering::SeqCst) == 1);
    for _ in 0..4 {
        assert_eq!(root_count(&c, &bridge), 1);
        assert_eq!(
            request(&c, &bridge, "HEAD", "/.Trash")
                .send()
                .unwrap()
                .status(),
            404
        );
    }
    assert!(start.elapsed() < Duration::from_millis(500));
    assert_eq!(fixture.listings.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 0);
    let start = Instant::now();
    drop(bridge);
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(fixture.cancelled_listings.load(Ordering::SeqCst), 1);
}

#[test]
fn icloud_root_refresh_failure_preserves_names_but_live_bytes_and_verified_empty_win() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(b"original");
    fixture.seed_root = true;
    fixture.fail_listing.store(true, Ordering::SeqCst);
    let bridge = Bridge::start_icloud(Backend::new(fixture.clone()), root.path(), None).unwrap();
    let c = client();
    expire_display(&bridge, DIRECTORY_MAX_AGE + Duration::from_secs(60));
    assert_eq!(root_count(&c, &bridge), 1);
    wait_until(|| {
        fixture.listings.load(Ordering::SeqCst) == 1
            && bridge.fs.0.directories.lock().unwrap().pending.is_empty()
    });
    assert_eq!(root_count(&c, &bridge), 1);
    assert_eq!(fixture.listings.load(Ordering::SeqCst), 1);
    fixture.change(b"live contents");
    assert_eq!(
        c.get(file_url(&bridge))
            .bearer_auth(&bridge.token)
            .send()
            .unwrap()
            .bytes()
            .unwrap(),
        b"live contents"[..]
    );
    fixture.fail_listing.store(false, Ordering::SeqCst);
    fixture.invalid_listing.store(true, Ordering::SeqCst);
    expire_display(&bridge, DIRECTORY_MAX_AGE + Duration::from_secs(60));
    assert_eq!(root_count(&c, &bridge), 1);
    wait_until(|| {
        fixture.listings.load(Ordering::SeqCst) == 2
            && bridge.fs.0.directories.lock().unwrap().pending.is_empty()
    });
    assert_eq!(root_count(&c, &bridge), 1);
    fixture.invalid_listing.store(false, Ordering::SeqCst);
    fixture.empty_listing.store(true, Ordering::SeqCst);
    expire_display(&bridge, DIRECTORY_MAX_AGE + Duration::from_secs(60));
    assert_eq!(root_count(&c, &bridge), 1);
    wait_until(|| {
        fixture.listings.load(Ordering::SeqCst) == 3
            && bridge.fs.0.directories.lock().unwrap().pending.is_empty()
    });
    assert_eq!(root_count(&c, &bridge), 0);
}

#[test]
fn icloud_root_index_keeps_budget_and_invalidation_after_browsing_many_children() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(b"contents");
    fixture.seed_root = true;
    fixture.child_directories = DIRECTORY_CACHE_LIMIT + 12;
    let bridge = Bridge::start_icloud(Backend::new(fixture.clone()), root.path(), None).unwrap();
    for index in 0..fixture.child_directories {
        bridge
            .fs
            .directory_snapshot(&format!("/folder-{index}"))
            .unwrap();
    }
    {
        let cache = bridge.fs.0.directories.lock().unwrap();
        assert!(cache.samples.iter().any(|s| s.path == "/"));
        assert!(cache.samples.len() <= DIRECTORY_CACHE_LIMIT);
        assert!(cache.samples.iter().map(|s| s.entries.len()).sum::<usize>() <= 100000);
    }
    bridge.fs.invalidate_display();
    fixture.fail_listing.store(true, Ordering::SeqCst);
    assert_eq!(
        request(&client(), &bridge, "PROPFIND", "/")
            .header("Depth", "1")
            .send()
            .unwrap()
            .status(),
        503
    );
}

#[test]
fn expired_or_invalid_listings_are_not_exposed_as_complete_directory_contents() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(b"data");
    fixture.seed_root = true;
    fixture.fail_listing.store(true, Ordering::SeqCst);
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    expire_display(&bridge, DIRECTORY_MAX_AGE + Duration::from_secs(1));
    assert_eq!(
        request(&client(), &bridge, "PROPFIND", "/")
            .header("Depth", "1")
            .send()
            .unwrap()
            .status(),
        503
    );
    fixture.fail_listing.store(false, Ordering::SeqCst);
    fixture.invalid_listing.store(true, Ordering::SeqCst);
    assert_eq!(
        request(&client(), &bridge, "PROPFIND", "/")
            .header("Depth", "1")
            .send()
            .unwrap()
            .status(),
        503
    );
    assert!(
        Bridge::start(
            Backend::new(Fixture {
                seed_root: true,
                ..fixture
            }),
            root.path()
        )
        .is_err()
    );
}

const QUOTA_PROPERTIES: &str = "<D:propfind xmlns:D=\"DAV:\"><D:prop><D:quota-used-bytes/><D:quota-available-bytes/></D:prop></D:propfind>";
fn quota_request(
    c: &reqwest::blocking::Client,
    bridge: &Bridge,
) -> reqwest::blocking::RequestBuilder {
    request(c, bridge, "PROPFIND", "/")
        .header("Depth", "0")
        .body(QUOTA_PROPERTIES)
}
#[test]
fn cold_slow_quota_does_not_block_filesystem_attributes() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(b"contents");
    fixture.hold_quota.store(true, Ordering::SeqCst);
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    let c = client();
    let response = quota_request(&c, &bridge)
        .timeout(Duration::from_millis(700))
        .send();
    // Always release the fixture before asserting, including on old-code failure.
    fixture.hold_quota.store(false, Ordering::SeqCst);
    let response = response.expect("filesystem attributes waited for cloud quota");
    assert_eq!(response.status(), 207);
    assert!(
        !response.text().unwrap().contains(">990<"),
        "cold unknown quota must not invent capacity"
    );
    wait_until(|| fixture.quota_reads.load(Ordering::SeqCst) != 0);
}

#[test]
fn quota_refresh_is_coalesced_retains_last_success_and_retries_without_blocking() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(b"contents");
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    wait_until(|| bridge.fs.0.quota.lock().unwrap().sample.is_some());
    fixture.hold_quota.store(true, Ordering::SeqCst);
    {
        let mut cache = bridge.fs.0.quota.lock().unwrap();
        cache.sample.as_mut().unwrap().checked = Instant::now() - QUOTA_FRESH;
        cache.attempted = None;
    }
    for _ in 0..20 {
        assert_eq!(bridge.fs.quota_snapshot().unwrap(), (10, Some(1000)));
    }
    wait_until(|| fixture.quota_reads.load(Ordering::SeqCst) == 2);
    assert_eq!(fixture.quota_reads.load(Ordering::SeqCst), 2);
    fixture.fail_quota.store(true, Ordering::SeqCst);
    fixture.hold_quota.store(false, Ordering::SeqCst);
    wait_until(|| !bridge.fs.0.quota.lock().unwrap().pending);
    assert_eq!(bridge.fs.quota_snapshot().unwrap(), (10, Some(1000)));
    assert_eq!(
        fixture.quota_reads.load(Ordering::SeqCst),
        2,
        "failed refresh must back off"
    );
    {
        let mut cache = bridge.fs.0.quota.lock().unwrap();
        cache.sample.as_mut().unwrap().checked = Instant::now() - QUOTA_MAX_AGE;
    }
    assert!(
        matches!(bridge.fs.quota_snapshot(), Err(FsError::NotImplemented)),
        "over-age display capacity must expire"
    );
    fixture.fail_quota.store(false, Ordering::SeqCst);
    bridge.fs.0.quota.lock().unwrap().attempted = None;
    let _ = bridge.fs.quota_snapshot();
    wait_until(|| {
        fixture.quota_reads.load(Ordering::SeqCst) == 3
            && !bridge.fs.0.quota.lock().unwrap().pending
    });
    assert_eq!(bridge.fs.quota_snapshot().unwrap(), (10, Some(1000)));
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 0);
}

#[test]
fn shutdown_cancels_a_pending_quota_refresh_and_joins_its_worker() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(b"contents");
    fixture.hold_quota.store(true, Ordering::SeqCst);
    let bridge = Bridge::start(Backend::new(fixture.clone()), root.path()).unwrap();
    wait_until(|| fixture.quota_reads.load(Ordering::SeqCst) == 1);
    let start = Instant::now();
    drop(bridge);
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(
        fixture.hold_quota.load(Ordering::SeqCst),
        "cancellation must not need the provider gate released"
    );
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires Linux /dev/fuse and TWODRIVE_TEST_RCLONE; synthetic quota only"]
fn native_statfs_and_gio_return_while_cloud_quota_is_stalled() {
    use std::process::{Child, Command, Stdio};
    struct Mounted {
        target: std::path::PathBuf,
        process: Child,
    }
    impl Drop for Mounted {
        fn drop(&mut self) {
            let _ = Command::new("fusermount3")
                .arg("-u")
                .arg(&self.target)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = self.process.kill();
            let _ = self.process.wait();
        }
    }
    let root = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(b"synthetic mount bytes");
    fixture.quota_scale = 1024 * 1024;
    fixture.include_slow_file = true;
    fixture.named_directories = [
        "中文 目录",
        "space folder",
        "hash#percent%",
        "literal%2F..%2F",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    fixture.hold_quota.store(true, Ordering::SeqCst);
    let bridge =
        Bridge::start_indexed(Backend::new(fixture.clone()), root.path(), None, false).unwrap();
    let target = root.path().join("mount");
    std::fs::create_dir(&target).unwrap();
    let mut command = Command::new(
        std::env::var_os("TWODRIVE_TEST_RCLONE").expect("explicit pinned component required"),
    );
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("RCLONE_")
        {
            command.env_remove(key);
        }
    }
    let process = command
        .args(["mount", "TWODRIVE:"])
        .arg(&target)
        .args([
            "--config=/dev/null",
            "--ask-password=false",
            "--read-only",
            "--vfs-cache-mode=off",
            "--dir-cache-time=100ms",
            "--log-level=ERROR",
            "--retries=1",
            "--low-level-retries=1",
        ])
        .env("RCLONE_CONFIG_TWODRIVE_TYPE", "webdav")
        .env(
            "RCLONE_CONFIG_TWODRIVE_URL",
            format!("http://{}/", bridge.address),
        )
        .env("RCLONE_CONFIG_TWODRIVE_BEARER_TOKEN", &bridge.token)
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut mount = Mounted {
        target: target.clone(),
        process,
    };
    wait_until(|| {
        assert!(mount.process.try_wait().unwrap().is_none());
        std::fs::read_to_string("/proc/self/mountinfo")
            .unwrap()
            .lines()
            .any(|line| line.split_whitespace().nth(4) == target.to_str())
    });
    // Include GIO's filesystem classification, not just readdir. This was the
    // missing native boundary in the earlier directory navigation regression.
    let mut probe = Command::new("/usr/bin/python3").args(["-c",
        "import os,sys; from gi.repository import Gio; p=sys.argv[1]; assert len(os.listdir(p)) == 6; os.statvfs(p); Gio.File.new_for_path(p).query_filesystem_info('filesystem::remote,filesystem::type',None)"])
        .arg(&target).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let start = Instant::now();
    let timely = loop {
        if let Some(status) = probe.try_wait().unwrap() {
            break status.success();
        }
        if start.elapsed() >= Duration::from_millis(1500) {
            break false;
        }
        thread::sleep(Duration::from_millis(10));
    };
    // Release before reaping even on regression: killing a FUSE caller can
    // itself wait for the synthetic server operation to finish.
    fixture.hold_quota.store(false, Ordering::SeqCst);
    let status = probe.wait().unwrap();
    assert!(
        timely && status.success(),
        "native filesystem classification waited for cloud quota"
    );
    wait_until(|| bridge.fs.0.quota.lock().unwrap().sample.is_some());
    thread::sleep(Duration::from_millis(150));
    let quota = Command::new("/usr/bin/python3").args(["-c", "import os,sys; s=os.statvfs(sys.argv[1]); assert s.f_blocks*s.f_frsize == 1000*1024*1024; assert s.f_bavail*s.f_frsize == 990*1024*1024"])
        .arg(&target).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
    assert!(
        quota.success(),
        "native quota must eventually replace the unknown fallback"
    );
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 0);
    for name in &fixture.named_directories {
        let directory = target.join(name);
        let names = std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, vec![std::ffi::OsString::from(NAME)]);
        assert_eq!(
            std::fs::read(directory.join(NAME)).unwrap(),
            b"synthetic mount bytes"
        );
    }
    {
        let mut cache = bridge.fs.0.directories.lock().unwrap();
        for sample in &mut cache.samples {
            sample.checked = Instant::now() - DIRECTORY_MAX_AGE - Duration::from_secs(60);
            sample.attempted = sample.checked;
        }
    }
    fixture.hold_listing.store(true, Ordering::SeqCst);
    thread::sleep(Duration::from_millis(150));
    let mut indexed = Command::new("/usr/bin/python3")
        .args([
            "-c",
            "import os,sys; assert all(len(os.listdir(p))==1 for p in sys.argv[1:])",
        ])
        .args(
            fixture
                .named_directories
                .iter()
                .map(|name| target.join(name)),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let timely = loop {
        if let Some(status) = indexed.try_wait().unwrap() {
            break status.success();
        }
        if start.elapsed() >= Duration::from_millis(1500) {
            break false;
        }
        thread::sleep(Duration::from_millis(10));
    };
    fixture.hold_listing.store(false, Ordering::SeqCst);
    assert!(
        indexed.wait().unwrap().success() && timely,
        "cached native subdirectories waited for a stalled cloud refresh after expiry"
    );
    wait_until(|| bridge.fs.0.directories.lock().unwrap().pending.is_empty());
    fixture.hold_download.store(true, Ordering::SeqCst);
    let mut reader = Command::new("/usr/bin/python3").args(["-c", "import pathlib,sys; assert pathlib.Path(sys.argv[1]).read_bytes() == b'synthetic mount bytes'"])
        .arg(target.join("slow.bin")).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    wait_until(|| fixture.download_started.load(Ordering::SeqCst));
    thread::sleep(Duration::from_millis(150));
    let mut listing = Command::new("/usr/bin/python3")
        .args([
            "-c",
            "import os,sys; assert len(os.listdir(sys.argv[1])) == 6",
        ])
        .arg(&target)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let timely = loop {
        if let Some(status) = listing.try_wait().unwrap() {
            break status.success();
        }
        if start.elapsed() >= Duration::from_millis(1500) {
            break false;
        }
        thread::sleep(Duration::from_millis(10));
    };
    fixture.hold_download.store(false, Ordering::SeqCst);
    assert!(reader.wait().unwrap().success());
    let listed = listing.wait().unwrap();
    assert!(
        timely && listed.success(),
        "native directory navigation waited for an unrelated held content read"
    );
}

#[test]
fn linux_office_owner_and_regex_exclusions_are_durable_local_files_without_cloud_transfers() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(b"cloud content stays unchanged");
    let policy: crate::sync_policy::SyncPolicy =
        serde_json::from_value(serde_json::json!({"exclude_name_regexes":["(?i)\\.bak$"]}))
            .unwrap();
    let data = vec![42u8; 128 * 1024];
    let spool = root.path().join("recovery");
    let backend = || Backend::new(fixture.clone()).with_sync_policy(policy.clone());
    let c = client();
    let bridge = Bridge::start_writable(backend(), root.path(), &spool).unwrap();
    for path in ["/~%24report.docx", "/notes.BAK"] {
        assert_eq!(
            request(&c, &bridge, "PUT", path)
                .body(data.clone())
                .send()
                .unwrap()
                .status(),
            201
        );
        let response = request(&c, &bridge, "GET", path).send().unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.bytes().unwrap().as_ref(), data.as_slice());
    }
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 0);
    assert_eq!(
        crate::mount_write::recovery_status(&spool).unwrap()["files"],
        0
    );
    assert_eq!(root_count(&c, &bridge), 3);
    drop(bridge);
    let bridge = Bridge::start_writable(backend(), root.path(), &spool).unwrap();
    for path in ["/~%24report.docx", "/notes.BAK"] {
        assert_eq!(
            request(&c, &bridge, "GET", path)
                .send()
                .unwrap()
                .bytes()
                .unwrap()
                .as_ref(),
            data.as_slice()
        );
        assert_eq!(
            request(&c, &bridge, "DELETE", path)
                .send()
                .unwrap()
                .status(),
            204
        );
        assert_eq!(
            request(&c, &bridge, "GET", path).send().unwrap().status(),
            404
        );
    }
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 0);
    assert_eq!(
        fixture.data.lock().unwrap().0,
        b"cloud content stays unchanged"
    );
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
fn repeated_head_metadata_queries_share_parent_listing_without_downloading() {
    let temp = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(b"payload");
    fixture.parent_browsing = true;
    let bridge = Bridge::start(Backend::new(fixture.clone()), temp.path()).unwrap();
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    for _ in 0..8 {
        let response = client
            .head(file_url(&bridge))
            .bearer_auth(&bridge.token)
            .send()
            .unwrap();
        assert!(response.status().is_success());
    }
    assert_eq!(fixture.listings.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.metadata_reads.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.downloads.load(Ordering::SeqCst), 0);
}
