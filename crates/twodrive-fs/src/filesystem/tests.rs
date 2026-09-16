use fuser::{FileType, MountOption};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use twodrive_backend::{CloudBackend, MockBackend};
use twodrive_core::now_unix;
use twodrive_core::{Database, FileState, MetadataEntry};

use crate::cache_io::write_slice;
use crate::filesystem::{ROOT_INO, TwoDriveFs, filesystem_stats};
use crate::hydration::{hydrate_generation, hydrate_pending_pins, hydrate_record};
use crate::inodes::{InodeTable, unix_time};
use crate::metadata::{sync_delta_metadata, sync_metadata};
use crate::probes::is_gio_probe_open;
use crate::recovery::{
    recover_dirty_record, recover_dirty_uploads, recover_interrupted_writes,
    recover_pending_deletes, recover_pending_metadata_operations, recover_pending_metadata_record,
};
use crate::upload_queue::{UploadCommand, UploadQueue, UploadRun};

use twodrive_backend::DeltaResult;

#[derive(Debug)]
struct FailingBackend {
    inner: MockBackend,
    fail_upload: bool,
    fail_delete: bool,
}

impl FailingBackend {
    fn new(fail_upload: bool, fail_delete: bool) -> Self {
        Self {
            inner: MockBackend::new(),
            fail_upload,
            fail_delete,
        }
    }
}

impl CloudBackend for FailingBackend {
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        self.inner.list_all()
    }

    fn list_delta(&self, delta_link: Option<&str>) -> anyhow::Result<DeltaResult> {
        self.inner.list_delta(delta_link)
    }

    fn download(&self, remote_id: &str) -> anyhow::Result<Vec<u8>> {
        self.inner.download(remote_id)
    }

    fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        if self.fail_upload {
            anyhow::bail!("simulated upload outage");
        }
        self.inner.upload(path, content)
    }

    fn upload_with_etag(
        &self,
        path: &str,
        content: Vec<u8>,
        if_match: Option<&str>,
    ) -> anyhow::Result<MetadataEntry> {
        if self.fail_upload {
            anyhow::bail!("simulated upload outage");
        }
        self.inner.upload_with_etag(path, content, if_match)
    }

    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        self.inner.create_folder(path)
    }

    fn rename(&self, remote_id: &str, new_path: &str) -> anyhow::Result<MetadataEntry> {
        self.inner.rename(remote_id, new_path)
    }

    fn delete(&self, remote_id: &str) -> anyhow::Result<()> {
        if self.fail_delete {
            anyhow::bail!("simulated delete outage");
        }
        self.inner.delete(remote_id)
    }
}

#[derive(Debug)]
struct ConcurrentBackend {
    inner: MockBackend,
    active_uploads: AtomicUsize,
    max_active_uploads: AtomicUsize,
}

impl ConcurrentBackend {
    fn new() -> Self {
        Self {
            inner: MockBackend::new(),
            active_uploads: AtomicUsize::new(0),
            max_active_uploads: AtomicUsize::new(0),
        }
    }
}

impl CloudBackend for ConcurrentBackend {
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        self.inner.list_all()
    }

    fn download(&self, remote_id: &str) -> anyhow::Result<Vec<u8>> {
        self.inner.download(remote_id)
    }

    fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        self.inner.upload(path, content)
    }

    fn upload_file_with_version(
        &self,
        path: &str,
        source_path: &Path,
        remote_id: Option<&str>,
        if_match: Option<&str>,
        on_progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        let active = self.active_uploads.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active_uploads.fetch_max(active, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(100));
        let result = self.inner.upload_file_with_version(
            path,
            source_path,
            remote_id,
            if_match,
            on_progress,
        );
        self.active_uploads.fetch_sub(1, Ordering::SeqCst);
        result
    }

    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        self.inner.create_folder(path)
    }

    fn rename(&self, remote_id: &str, new_path: &str) -> anyhow::Result<MetadataEntry> {
        self.inner.rename(remote_id, new_path)
    }

    fn delete(&self, remote_id: &str) -> anyhow::Result<()> {
        self.inner.delete(remote_id)
    }
}

#[derive(Debug)]
struct ConflictBackend {
    inner: MockBackend,
}

impl ConflictBackend {
    fn new() -> Self {
        Self {
            inner: MockBackend::new(),
        }
    }
}

impl CloudBackend for ConflictBackend {
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        self.inner.list_all()
    }

    fn get_metadata(&self, remote_id: &str) -> anyhow::Result<Option<MetadataEntry>> {
        if remote_id == "file-readme" {
            return Ok(Some(MetadataEntry::new_file(
                "file-readme",
                "/README-cloud.txt",
                42,
                1_719_000_100,
                "etag-cloud-winner",
            )));
        }
        self.inner.get_metadata(remote_id)
    }

    fn download(&self, remote_id: &str) -> anyhow::Result<Vec<u8>> {
        self.inner.download(remote_id)
    }

    fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        self.inner.upload(path, content)
    }

    fn upload_with_etag(
        &self,
        path: &str,
        content: Vec<u8>,
        if_match: Option<&str>,
    ) -> anyhow::Result<MetadataEntry> {
        if if_match.is_some() {
            anyhow::bail!("Graph request failed with HTTP 412 Precondition Failed");
        }
        self.inner.upload(path, content)
    }

    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        self.inner.create_folder(path)
    }

    fn rename(&self, remote_id: &str, new_path: &str) -> anyhow::Result<MetadataEntry> {
        self.inner.rename(remote_id, new_path)
    }

    fn delete(&self, remote_id: &str) -> anyhow::Result<()> {
        self.inner.delete(remote_id)
    }
}

#[derive(Debug)]
struct LostCreateAckBackend {
    inner: MockBackend,
}

impl CloudBackend for LostCreateAckBackend {
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        self.inner.list_all()
    }

    fn download(&self, remote_id: &str) -> anyhow::Result<Vec<u8>> {
        self.inner.download(remote_id)
    }

    fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        self.inner.upload(path, content)
    }

    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        self.inner.create_folder(path)?;
        anyhow::bail!("simulated lost create acknowledgement")
    }

    fn rename(&self, remote_id: &str, new_path: &str) -> anyhow::Result<MetadataEntry> {
        self.inner.rename(remote_id, new_path)
    }

    fn delete(&self, remote_id: &str) -> anyhow::Result<()> {
        self.inner.delete(remote_id)
    }
}

struct TestFs {
    fs: TwoDriveFs<MockBackend>,
    root: PathBuf,
}

impl TestFs {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "twodrive-fs-{name}-{}-{}",
            std::process::id(),
            now_unix()
        ));
        fs::create_dir_all(&root).unwrap();
        let db = Database::new(root.join("test.sqlite3"));
        db.init().unwrap();
        let backend = MockBackend::new();
        sync_metadata(&db, &backend).unwrap();
        let fs = TwoDriveFs::new(db, root.join("cache"), backend).expect("create test filesystem");
        Self { fs, root }
    }
}

impl Drop for TestFs {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn test_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "twodrive-fs-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[derive(Debug)]
struct SlowDownloadBackend {
    inner: MockBackend,
    started: Sender<()>,
}

impl CloudBackend for SlowDownloadBackend {
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        self.inner.list_all()
    }
    fn download(&self, remote_id: &str) -> anyhow::Result<Vec<u8>> {
        let _ = self.started.send(());
        thread::sleep(Duration::from_secs(3));
        self.inner.download(remote_id)
    }
    fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        self.inner.upload(path, content)
    }
    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        self.inner.create_folder(path)
    }
    fn rename(&self, remote_id: &str, path: &str) -> anyhow::Result<MetadataEntry> {
        self.inner.rename(remote_id, path)
    }
    fn delete(&self, remote_id: &str) -> anyhow::Result<()> {
        self.inner.delete(remote_id)
    }
}

#[test]
#[ignore = "requires /dev/fuse and fusermount3; uses an isolated temporary mount"]
fn mounted_large_listing_and_copy_remain_responsive_during_download() {
    let root = test_root("mounted-responsiveness");
    let mount = root.join("mount");
    fs::create_dir_all(&mount).unwrap();
    let db = Database::new(root.join("test.sqlite3"));
    db.init().unwrap();
    let (started, waiting) = mpsc::channel();
    let backend = SlowDownloadBackend {
        inner: MockBackend::new(),
        started,
    };
    sync_metadata(&db, &backend).unwrap();
    let mut entries = vec![MetadataEntry::new_dir("listing", "/listing", 0, "etag")];
    entries.extend((0..10_000).map(|index| {
        MetadataEntry::new_file(
            format!("list-{index}"),
            format!("/listing/file-{index:05}"),
            0,
            0,
            "etag",
        )
    }));
    db.upsert_metadata_batch(&entries).unwrap();
    let filesystem = TwoDriveFs::new(db, root.join("cache"), backend).unwrap();
    let session = fuser::spawn_mount2(filesystem, &mount, &[]).unwrap();
    let cloud_file = mount.join("README-cloud.txt");
    let reader = thread::spawn(move || fs::read(cloud_file).unwrap());
    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    let start = std::time::Instant::now();
    let names: Vec<_> = fs::read_dir(mount.join("listing"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 10_000);
    assert_eq!(names.iter().collect::<HashSet<_>>().len(), 10_000);
    let destination = mount.join("large-copy.bin");
    let bytes = vec![0x51; 16 * 1024 * 1024];
    fs::write(&destination, &bytes).unwrap();
    let foreground = start.elapsed();
    assert!(
        foreground < Duration::from_secs(2),
        "foreground blocked: {foreground:?}"
    );
    assert_eq!(fs::read(destination).unwrap(), bytes);
    assert!(!reader.join().unwrap().is_empty());
    eprintln!("10,000-entry listing + 16 MiB copy during delayed download: {foreground:?}");
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[derive(Debug)]
struct StreamingBackend {
    inner: MockBackend,
    started: Sender<()>,
    bytes_sent: AtomicUsize,
}

impl CloudBackend for StreamingBackend {
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        self.inner.list_all()
    }
    fn download(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        self.inner.download(id)
    }
    fn upload(&self, path: &str, bytes: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        self.inner.upload(path, bytes)
    }
    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        self.inner.create_folder(path)
    }
    fn rename(&self, id: &str, path: &str) -> anyhow::Result<MetadataEntry> {
        self.inner.rename(id, path)
    }
    fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.inner.delete(id)
    }
    fn download_sized_to(
        &self,
        _id: &str,
        size: u64,
        writer: &mut dyn Write,
        progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        self.started.send(()).unwrap();
        let mut done = 0;
        while done < size {
            thread::sleep(Duration::from_millis(2));
            let bytes = vec![0x51; (size - done).min(4096) as usize];
            writer.write_all(&bytes)?;
            done += bytes.len() as u64;
            self.bytes_sent.fetch_add(bytes.len(), Ordering::Relaxed);
            progress(done)?;
        }
        Ok(done)
    }
}

#[test]
fn release_stops_streaming_download_cleans_partial_and_allows_new_open() {
    check_release_stops_streaming_download(false);
}

#[test]
fn release_stops_pinned_streaming_download() {
    check_release_stops_streaming_download(true);
}

fn check_release_stops_streaming_download(pinned: bool) {
    let root = test_root(if pinned {
        "cancel-pinned-stream"
    } else {
        "cancel-stream"
    });
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("test.sqlite3"));
    db.init().unwrap();
    let entry = MetadataEntry::new_file("stream", "/stream.bin", 1_048_576, 0, "etag");
    db.upsert_metadata(&entry).unwrap();
    if pinned {
        db.set_explicit_pin("stream", true).unwrap();
    }
    let record = db.get_by_remote_id("stream").unwrap().unwrap();
    let old_generation = db.download_generation("stream").unwrap();
    let (started, ready) = mpsc::channel();
    let backend = StreamingBackend {
        inner: MockBackend::new(),
        started,
        bytes_sent: AtomicUsize::new(0),
    };
    let cache = root.join("cache");
    thread::scope(|scope| {
        let work = scope.spawn(|| hydrate_record(&db, &cache, &backend, &record));
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let independent_db = Database::new(db.path().to_path_buf());
        assert_eq!(independent_db.release_path("/stream.bin").unwrap(), 1);
        let err = work.join().unwrap().unwrap_err();
        assert_eq!(
            err.downcast_ref::<io::Error>().unwrap().raw_os_error(),
            Some(libc::ECANCELED)
        );
    });
    let sent = backend.bytes_sent.load(Ordering::Relaxed);
    assert!(sent < entry.size as usize);
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 0);
    assert!(hydrate_generation(&db, &cache, &backend, &record, old_generation).is_err());
    assert_eq!(backend.bytes_sent.load(Ordering::Relaxed), sent);
    let path = hydrate_record(&db, &cache, &backend, &record).unwrap();
    assert_eq!(fs::read(path).unwrap(), vec![0x51; entry.size as usize]);
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires FUSE and TWODRIVE_TEST_CLI pointing to the newly built CLI"]
fn mounted_release_cancels_download_and_old_handles() {
    let cli = std::env::var("TWODRIVE_TEST_CLI")
        .expect("set TWODRIVE_TEST_CLI to the built twodrive binary");
    let root = test_root("mounted-cancel");
    let mount = root.join("mount");
    fs::create_dir_all(&mount).unwrap();
    fs::create_dir_all(root.join("data/twodrive")).unwrap();
    let db = Database::new(root.join("data/twodrive/twodrive.sqlite3"));
    db.init().unwrap();
    db.upsert_metadata(&MetadataEntry::new_file(
        "stream",
        "/stream.bin",
        1_048_576,
        0,
        "etag",
    ))
    .unwrap();
    let (started, ready) = mpsc::channel();
    let backend = StreamingBackend {
        inner: MockBackend::new(),
        started,
        bytes_sent: AtomicUsize::new(0),
    };
    let fs = TwoDriveFs::new(db, root.join("data/twodrive/cache"), backend).unwrap();
    let session = fuser::spawn_mount2(fs, &mount, &[]).unwrap();
    let path = mount.join("stream.bin");
    let mut older = fs::File::open(&path).unwrap();
    let reading_path = path.clone();
    let reading = thread::spawn(move || fs::read(reading_path));
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let result = Command::new(cli)
        .args(["release", "/stream.bin"])
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("TWODRIVE_MOUNT_DIR", &mount)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    // Linux buffered FUSE reads may normalize ECANCELED to EIO in the page cache.
    assert!(matches!(
        reading.join().unwrap().unwrap_err().raw_os_error(),
        Some(libc::ECANCELED) | Some(libc::EIO)
    ));
    assert!(matches!(
        older.read(&mut [0; 64]).unwrap_err().raw_os_error(),
        Some(libc::ECANCELED) | Some(libc::EIO)
    ));
    drop(older);
    assert_eq!(fs::read(&path).unwrap(), vec![0x51; 1_048_576]);
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn gio_probe_detection_does_not_block_normal_reads_or_other_tools() {
    assert!(is_gio_probe_open(
        libc::O_RDONLY | libc::O_NOATIME,
        OsStr::new("nautilus")
    ));
    assert!(is_gio_probe_open(
        libc::O_RDONLY | libc::O_NOATIME,
        OsStr::new("gio")
    ));
    assert!(!is_gio_probe_open(libc::O_RDONLY, OsStr::new("gio")));
    assert!(!is_gio_probe_open(
        libc::O_RDONLY | libc::O_NOATIME,
        OsStr::new("cp")
    ));
    assert!(!is_gio_probe_open(
        libc::O_WRONLY | libc::O_NOATIME,
        OsStr::new("nautilus")
    ));
}

#[test]
#[ignore = "requires /dev/fuse, fusermount3, python3 and cc; uses an isolated temporary mount"]
fn compiled_program_and_chmod_survive_remount() {
    let root = test_root("executable-mode");
    let mount = root.join("mount");
    fs::create_dir_all(&mount).unwrap();
    let db = Database::new(root.join("test.sqlite3"));
    db.init().unwrap();
    for phase in ["create", "remount"] {
        let filesystem =
            TwoDriveFs::new(db.clone(), root.join("cache"), MockBackend::new()).unwrap();
        let session =
            fuser::spawn_mount2(filesystem, &mount, &[MountOption::DefaultPermissions]).unwrap();
        let result = Command::new("python3")
            .arg("-c")
            .arg(
                r#"
import os, pathlib, subprocess, sys
root = pathlib.Path(sys.argv[1])
program = root / 'hello'
if sys.argv[2] == 'create':
    os.umask(0o027)
    descriptor = os.open(root / 'created-executable', os.O_CREAT | os.O_WRONLY, 0o777)
    os.close(descriptor)
    assert (root / 'created-executable').stat().st_mode & 0o777 == 0o750
    (root / 'hello.c').write_text('int main(void) { return 23; }\n')
    subprocess.run(['cc', str(root / 'hello.c'), '-o', str(program)], check=True)
    assert program.stat().st_mode & 0o777 == 0o750, oct(program.stat().st_mode)
    assert subprocess.run([str(program)]).returncode == 23
    program.chmod(0o640)
    try:
        subprocess.run([str(program)])
    except PermissionError:
        pass
    else:
        raise AssertionError('execution allowed after removing execute bits')
    program.chmod(0o000)
    try:
        program.read_bytes()
    except PermissionError:
        pass
    else:
        raise AssertionError('read allowed after removing read bits')
    program.chmod(0o751)
    assert subprocess.run([str(program)]).returncode == 23
    (root / 'private').mkdir(mode=0o777)
    assert (root / 'private').stat().st_mode & 0o777 == 0o750
    (root / 'private').chmod(0o700)
    program.rename(root / 'renamed')
else:
    program = root / 'renamed'
    assert program.stat().st_mode & 0o777 == 0o751
    assert (root / 'private').stat().st_mode & 0o777 == 0o700
    assert subprocess.run([str(program)]).returncode == 23
"#,
            )
            .arg(&mount)
            .arg(phase)
            .output()
            .unwrap();
        drop(session);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires /dev/fuse, fusermount3 and python3; uses an isolated temporary mount"]
fn deferred_reader_rechecks_indexer_before_downloading() {
    let root = test_root("deferred-reader");
    let mount = root.join("mount");
    fs::create_dir_all(&mount).unwrap();
    let db = Database::new(root.join("test.sqlite3"));
    db.init().unwrap();
    let (started, downloads) = mpsc::channel();
    let backend = SlowDownloadBackend {
        inner: MockBackend::new(),
        started,
    };
    let entry = backend
        .inner
        .upload("/actual.txt", b"explicit read".to_vec())
        .unwrap();
    db.upsert_metadata(&entry).unwrap();
    let filesystem = TwoDriveFs::new(db.clone(), root.join("cache"), backend).unwrap();
    let session = fuser::spawn_mount2(filesystem, &mount, &[]).unwrap();
    let script = root.join("reader.py");
    fs::write(
        &script,
        r#"
import ctypes, errno, os, sys
fd = os.open(sys.argv[1], os.O_RDONLY)
libc = ctypes.CDLL(None)
assert libc.prctl(15, b'tracker-extract', 0, 0, 0) == 0
try:
    os.read(fd, 13)
except OSError as error:
    # Buffered FUSE reads may translate ENODATA to EIO.
    assert error.errno in (errno.ENODATA, errno.EIO), error
else:
    raise AssertionError('background reader downloaded the file')
assert libc.prctl(15, b'python3', 0, 0, 0) == 0
assert os.read(fd, 13) == b'explicit read'
os.close(fd)
"#,
    )
    .unwrap();
    let result = Command::new("python3")
        .arg(&script)
        .arg(mount.join("actual.txt"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        downloads.try_iter().count(),
        1,
        "only the explicit read should download"
    );
    drop(session);
    let _ = fs::remove_dir_all(root);
}

#[test]
#[ignore = "requires /dev/fuse, fusermount3 and gio; uses an isolated temporary mount"]
fn gio_directory_metadata_does_not_download_unknown_large_files() {
    let root = test_root("gio-metadata");
    let mount = root.join("mount");
    fs::create_dir_all(&mount).unwrap();
    let db = Database::new(root.join("test.sqlite3"));
    db.init().unwrap();
    let (started, downloads) = mpsc::channel();
    let backend = SlowDownloadBackend {
        inner: MockBackend::new(),
        started,
    };
    let actual = backend
        .inner
        .upload("/actual.part.09", b"explicit copy content".to_vec())
        .unwrap();
    db.upsert_metadata(&actual).unwrap();
    let entries: Vec<_> = (0..30)
        .map(|i| {
            MetadataEntry::new_file(
                format!("large-{i}"),
                format!("/archive.part.{i:02}"),
                4_000_000_000,
                0,
                "etag",
            )
        })
        .collect();
    db.upsert_metadata_batch(&entries).unwrap();
    let filesystem = TwoDriveFs::new(db.clone(), root.join("cache"), backend).unwrap();
    let session = fuser::spawn_mount2(filesystem, &mount, &[]).unwrap();
    let start = std::time::Instant::now();
    let result = Command::new("gio")
        .args(["list", "-a", "standard::*,access::*"])
        .arg(&mount)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "GIO waited for file content"
    );
    assert_eq!(String::from_utf8_lossy(&result.stdout).lines().count(), 31);
    assert!(
        downloads.try_recv().is_err(),
        "metadata query downloaded content"
    );
    assert!(
        db.all_records()
            .unwrap()
            .iter()
            .all(|r| r.cache_path.is_none())
    );
    eprintln!(
        "GIO full metadata for 30 virtual 4 GB files: {:?}, zero downloads",
        start.elapsed()
    );
    let destination = root.join("copied.part.09");
    let copied = Command::new("gio")
        .arg("copy")
        .arg(mount.join("actual.part.09"))
        .arg(&destination)
        .output()
        .unwrap();
    assert!(
        copied.status.success(),
        "{}",
        String::from_utf8_lossy(&copied.stderr)
    );
    assert_eq!(fs::read(destination).unwrap(), b"explicit copy content");
    assert!(
        downloads.try_recv().is_ok(),
        "explicit copy did not download content"
    );
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn interrupted_open_write_is_recovered_with_actual_cache_size() {
    let mut test = TestFs::new("restart-open-write");
    let (_, fh, _) = test
        .fs
        .create_upload(ROOT_INO, OsStr::new("interrupted.bin"), 0)
        .unwrap();
    let handle = test.fs.write_handles.get(&fh).unwrap();
    let id = handle.record_remote_id.clone();
    fs::write(&handle.cache_path, b"durable bytes before shutdown").unwrap();
    test.fs.write_handles.clear(); // Simulated lost process, without release/queue.
    recover_interrupted_writes(&test.fs.db).unwrap();
    let record = test.fs.db.get_by_remote_id(&id).unwrap().unwrap();
    assert_eq!(record.state, FileState::Dirty);
    assert_eq!(record.metadata.size, 29);
    assert_eq!(
        recover_dirty_uploads(&test.fs.db, test.fs.backend.as_ref()).unwrap(),
        1
    );
    assert_eq!(
        test.fs.db.get_by_remote_id(&id).unwrap().unwrap().state,
        FileState::Cached
    );
}

#[test]
fn recovery_queue_coalesces_repeated_scans_and_retains_work() {
    let (sender, receiver) = mpsc::channel();
    let queue = UploadQueue {
        sender,
        pending: Arc::new(Mutex::new(HashMap::new())),
    };
    for _ in 0..100 {
        queue.send(UploadCommand::Upload("same".into())).unwrap();
        queue.send(UploadCommand::Delete("same".into())).unwrap();
    }
    assert_eq!(receiver.try_iter().count(), 2);
}

#[test]
fn running_upload_coalesces_retries_without_occupying_other_workers() {
    let (sender, receiver) = mpsc::channel();
    let queue = UploadQueue {
        sender,
        pending: Arc::new(Mutex::new(HashMap::new())),
    };
    queue.send(UploadCommand::Upload("large".into())).unwrap();
    let command = receiver.recv().unwrap();
    let running = UploadRun::start(&queue, &command).unwrap();
    for _ in 0..100 {
        queue.send(UploadCommand::Upload("large".into())).unwrap();
    }
    assert!(receiver.try_recv().is_err());
    queue.send(UploadCommand::Upload("small".into())).unwrap();
    assert!(matches!(receiver.try_recv().unwrap(), UploadCommand::Upload(id) if id == "small"));
    drop(running);
    assert!(matches!(receiver.try_recv().unwrap(), UploadCommand::Upload(id) if id == "large"));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn large_directory_refresh_preserves_inodes_and_removes_stale_names() {
    let test = TestFs::new("directory-refresh");
    let template = test
        .fs
        .db
        .all_records()
        .unwrap()
        .into_iter()
        .find(|r| !r.metadata.is_dir)
        .unwrap();
    let records: Vec<_> = (0..10_000)
        .map(|index| {
            let mut record = template.clone();
            record.metadata = MetadataEntry::new_file(
                format!("id-{index}"),
                format!("/file-{index:05}"),
                index,
                0,
                "etag",
            );
            record
        })
        .collect();
    let mut table = InodeTable::new(&[]);
    table.refresh_children("/", records.clone());
    let stable = table.ino_for_path("/file-00001").unwrap();
    table.refresh_children("/", records[1..].to_vec());
    assert_eq!(table.ino_for_path("/file-00001"), Some(stable));
    assert_eq!(table.ino_for_path("/file-00000"), None);
    table.set_size(stable, 12345);
    let children = table.children_for_ino(ROOT_INO);
    assert_eq!(children.len(), 9999);
    assert_eq!(children[0].1.metadata.size, 12345);
}

#[test]
fn filesystem_stats_report_real_backing_store_capacity() {
    let root = test_root("statfs");
    fs::create_dir_all(&root).unwrap();

    let stats = filesystem_stats(&root).unwrap();

    assert!(stats.blocks > 0);
    assert!(stats.blocks_available > 0);
    assert!(stats.block_size > 0);
    assert!(stats.fragment_size > 0);
    assert!(stats.name_length >= 255);
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires /dev/fuse, fusermount3 and python3; uses an isolated temporary mount"]
fn zip_extraction_into_pending_folder_on_mount() {
    let root = test_root("zip-extraction");
    let mount = root.join("mount");
    fs::create_dir_all(&mount).unwrap();
    let db = Database::new(root.join("test.sqlite3"));
    db.init().unwrap();
    // Do not enqueue this folder: reproduce extraction before cloud creation settles.
    db.create_local_directory("local-upload-archive", "/archive")
        .unwrap();
    let filesystem = TwoDriveFs::new(db.clone(), root.join("cache"), MockBackend::new()).unwrap();
    let session = fuser::spawn_mount2(filesystem, &mount, &[]).unwrap();
    let result = Command::new("python3")
        .arg("-c")
        .arg(
            r#"
import io, pathlib, sys, zipfile
entries = {'__MACOSX/._TINY_compiler_fixed': b'AppleDouble metadata',
           'TINY_compiler_fixed/main.c': b'int main(void) { return 0; }',
           'TINY_compiler_fixed/empty': b''}
archive = io.BytesIO()
with zipfile.ZipFile(archive, 'w') as z:
    for name, data in entries.items():
        z.writestr(name, data)
archive.seek(0)
with zipfile.ZipFile(archive) as z:
    z.extractall(sys.argv[1])
for name, data in entries.items():
    assert (pathlib.Path(sys.argv[1]) / name).read_bytes() == data
"#,
        )
        .arg(mount.join("archive"))
        .output()
        .unwrap();
    drop(session);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        db.pending_metadata_operation("local-upload-archive")
            .unwrap()
            .is_some()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn archive_files_in_pending_nested_folders_survive_until_upload() {
    let mut test = TestFs::new("archive-pending-folders");
    let db = test.fs.db.clone();
    db.create_local_directory("local-upload-archive", "/archive")
        .unwrap();
    let nested = db
        .create_local_directory("local-upload-macos", "/archive/__MACOSX")
        .unwrap();
    let parent = test.fs.inodes.insert_or_update(nested);
    for name in ["._TINY_compiler_fixed", "ordinary.txt"] {
        let (ino, fh, _) = test
            .fs
            .create_upload(parent, OsStr::new(name), libc::O_CREAT | libc::O_EXCL)
            .unwrap();
        let handle = &test.fs.write_handles[&fh];
        let id = handle.record_remote_id.clone();
        let cache = handle.cache_path.clone();
        assert_eq!(
            db.get_by_remote_id(&id).unwrap().unwrap().state,
            FileState::Writing
        );
        write_slice(&cache, 0, b"archive content").unwrap();
        test.fs.inodes.set_size(ino, 15);
        test.fs.sync_handle(fh, false).unwrap();
        test.fs.write_handles.remove(&fh);
        db.mark_dirty_with_size(&id, 15).unwrap();
        let record = db.get_by_remote_id(&id).unwrap().unwrap();
        assert!(
            !recover_dirty_record(
                &db,
                test.fs.backend.as_ref(),
                record.clone(),
                &mut |_, _| Ok(())
            )
            .unwrap()
        );
        // Stale delta entries must still be ignored while the parent is pending.
        db.upsert_metadata(&MetadataEntry::new_file(
            "stale-cloud",
            &record.metadata.path,
            0,
            0,
            "stale",
        ))
        .unwrap();
        assert_eq!(fs::read(cache).unwrap(), b"archive content");
        assert_eq!(
            db.get_by_path(&record.metadata.path)
                .unwrap()
                .unwrap()
                .metadata
                .remote_id,
            id
        );
    }
    recover_pending_metadata_operations(&db, test.fs.backend.as_ref()).unwrap();
    assert!(db.pending_metadata_operations().unwrap().is_empty());
    for name in ["._TINY_compiler_fixed", "ordinary.txt"] {
        let path = format!("/archive/__MACOSX/{name}");
        let record = db.get_by_path(&path).unwrap().unwrap();
        assert!(
            recover_dirty_record(&db, test.fs.backend.as_ref(), record, &mut |_, _| Ok(()))
                .unwrap()
        );
        let remote = test
            .fs
            .backend
            .get_metadata_by_path(&path)
            .unwrap()
            .unwrap();
        assert_eq!(
            test.fs.backend.download(&remote.remote_id).unwrap(),
            b"archive content"
        );
    }
}

#[test]
fn local_fsync_preserves_random_offset_writes_before_upload() {
    let mut test = TestFs::new("fsync");
    let (ino, fh, _) = test
        .fs
        .create_upload(ROOT_INO, OsStr::new("random.bin"), libc::O_CREAT)
        .unwrap();
    let cache_path = test.fs.write_handles[&fh].cache_path.clone();

    write_slice(&cache_path, 4, b"EFGH").unwrap();
    write_slice(&cache_path, 0, b"ABCD").unwrap();
    test.fs.inodes.set_size(ino, 8);
    test.fs.sync_handle(fh, false).unwrap();

    assert_eq!(fs::read(cache_path).unwrap(), b"ABCDEFGH");
    let record = test
        .fs
        .db
        .get_by_remote_id(&test.fs.write_handles[&fh].record_remote_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.state, FileState::Writing);
    assert_eq!(
        hydrate_record(
            &test.fs.db,
            &test.fs.cache_dir,
            test.fs.backend.as_ref(),
            &record
        )
        .unwrap(),
        test.fs.write_handles[&fh].cache_path
    );
}

#[test]
fn path_truncate_is_local_and_queues_an_upload() {
    let mut test = TestFs::new("path-truncate");
    let ino = test.fs.inodes.ino_for_path("/README-cloud.txt").unwrap();

    test.fs.truncate_without_handle(ino, 0).unwrap();

    let local = test
        .fs
        .db
        .get_by_path("/README-cloud.txt")
        .unwrap()
        .unwrap();
    assert_eq!(local.metadata.size, 0);
    assert!(matches!(
        local.state,
        FileState::Dirty | FileState::Uploading | FileState::Cached
    ));
    assert_eq!(fs::metadata(local.cache_path.unwrap()).unwrap().len(), 0);
}

#[test]
fn child_sync_waits_for_parent_move_and_rejects_stale_jobs() {
    let root = test_root("child-sync-parent-move");
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("test.sqlite3"));
    db.init().unwrap();
    let backend = MockBackend::new();
    let parent = backend.create_folder("/Old").unwrap();
    db.upsert_metadata(&parent).unwrap();
    db.create_local_directory("local-upload-sub", "/Old/sub")
        .unwrap();
    let stale = db
        .pending_metadata_operation("local-upload-sub")
        .unwrap()
        .unwrap();
    let cache = root.join("paper.cache");
    fs::write(&cache, b"paper contents").unwrap();
    db.upsert_metadata(&MetadataEntry::new_file(
        "local-upload-paper",
        "/Old/paper.pdf",
        14,
        1,
        "",
    ))
    .unwrap();
    db.mark_cached("local-upload-paper", &cache).unwrap();
    db.mark_dirty_with_size("local-upload-paper", 14).unwrap();
    db.move_subtree_and_queue(&parent.remote_id, "/New")
        .unwrap();
    let record = db.get_by_remote_id("local-upload-paper").unwrap().unwrap();
    assert!(!db.begin_upload(&record).unwrap());
    assert!(!recover_dirty_record(&db, &backend, record.clone(), &mut |_, _| Ok(())).unwrap());
    assert!(!recover_pending_metadata_record(&db, &backend, &stale).unwrap());
    let child = db
        .pending_metadata_operation("local-upload-sub")
        .unwrap()
        .unwrap();
    assert!(!recover_pending_metadata_record(&db, &backend, &child).unwrap());
    assert!(
        backend
            .get_metadata_by_path("/New/paper.pdf")
            .unwrap()
            .is_none()
    );
    let parent_job = db
        .pending_metadata_operation(&parent.remote_id)
        .unwrap()
        .unwrap();
    assert!(recover_pending_metadata_record(&db, &backend, &parent_job).unwrap());
    assert!(recover_pending_metadata_record(&db, &backend, &child).unwrap());
    assert!(recover_dirty_record(&db, &backend, record, &mut |_, _| Ok(())).unwrap());
    let uploaded = backend
        .get_metadata_by_path("/New/paper.pdf")
        .unwrap()
        .unwrap();
    assert_eq!(
        backend.download(&uploaded.remote_id).unwrap(),
        b"paper contents"
    );
    assert!(backend.get_metadata_by_path("/Old/sub").unwrap().is_none());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn folder_create_recovery_rebinds_after_a_lost_remote_acknowledgement() {
    let root = test_root("lost-folder-create-ack");
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("test.sqlite3"));
    db.init().unwrap();
    let backend = LostCreateAckBackend {
        inner: MockBackend::new(),
    };
    db.create_local_directory("local-folder", "/Recovered folder")
        .unwrap();

    assert_eq!(
        recover_pending_metadata_operations(&db, &backend).unwrap(),
        1
    );
    let record = db.get_by_remote_id("local-folder").unwrap().unwrap();
    assert_eq!(record.metadata.path, "/Recovered folder");
    assert!(record.cloud_remote_id.is_some());
    assert!(db.pending_metadata_operations().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn backup_save_waits_for_cloud_move_or_delete_before_reusing_path() {
    for delete_backup in [false, true] {
        let mut test = TestFs::new(if delete_backup {
            "backup-delete"
        } else {
            "backup-move"
        });
        let path = "/README-cloud.txt";
        let backup = "/README-cloud.txt~";
        let old = test.fs.db.get_by_path(path).unwrap().unwrap();
        let original = test
            .fs
            .backend
            .download(old.cloud_remote_id.as_deref().unwrap())
            .unwrap();
        test.fs
            .db
            .move_subtree_and_queue(&old.metadata.remote_id, backup)
            .unwrap();
        let moved = test.fs.db.get_by_path(backup).unwrap().unwrap();
        let ino = test.fs.inodes.ino_for_path(path).unwrap();
        test.fs.inodes.replace_ino_record(ino, moved.clone());
        let (_, fh, _) = test
            .fs
            .create_upload(ROOT_INO, OsStr::new("README-cloud.txt"), libc::O_CREAT)
            .unwrap();
        let handle = &test.fs.write_handles[&fh];
        fs::write(&handle.cache_path, b"new saved content").unwrap();
        test.fs
            .db
            .mark_dirty_with_size(&handle.record_remote_id, 17)
            .unwrap();
        let pending = test.fs.db.get_by_path(path).unwrap().unwrap();
        if delete_backup {
            test.fs.db.queue_pending_delete(&moved).unwrap();
        }

        assert!(
            !recover_dirty_record(
                &test.fs.db,
                test.fs.backend.as_ref(),
                pending.clone(),
                &mut |_, _| Ok(())
            )
            .unwrap()
        );
        assert_eq!(
            test.fs
                .backend
                .download(old.cloud_remote_id.as_deref().unwrap())
                .unwrap(),
            original
        );
        assert!(
            test.fs
                .db
                .get_by_path(path)
                .unwrap()
                .unwrap()
                .cloud_remote_id
                .is_none()
        );

        if delete_backup {
            recover_pending_deletes(&test.fs.db, test.fs.backend.as_ref()).unwrap();
        } else {
            recover_pending_metadata_operations(&test.fs.db, test.fs.backend.as_ref()).unwrap();
        }
        assert!(
            recover_dirty_record(
                &test.fs.db,
                test.fs.backend.as_ref(),
                pending,
                &mut |_, _| Ok(())
            )
            .unwrap()
        );
        if !delete_backup {
            test.fs.db.queue_pending_delete(&moved).unwrap();
            recover_pending_deletes(&test.fs.db, test.fs.backend.as_ref()).unwrap();
        }
        sync_delta_metadata(&test.fs.db, test.fs.backend.as_ref()).unwrap();
        let saved = test.fs.db.get_by_path(path).unwrap().unwrap();
        assert_eq!(saved.state, FileState::Cached);
        assert_eq!(
            test.fs
                .backend
                .download(saved.cloud_remote_id.as_deref().unwrap())
                .unwrap(),
            b"new saved content"
        );
        let (_, reopened, _) = test.fs.create_overwrite_upload(saved, false).unwrap();
        assert_eq!(
            fs::read(&test.fs.write_handles[&reopened].cache_path).unwrap(),
            b"new saved content"
        );
    }
}

#[test]
fn immediate_reopen_during_upload_keeps_the_local_record() {
    let root = test_root("immediate-reopen");
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("test.sqlite3"));
    db.init().unwrap();
    let backend = ConcurrentBackend::new();
    sync_metadata(&db, &backend).unwrap();
    let mut fuse =
        TwoDriveFs::new_with_upload_concurrency(db, root.join("cache"), backend, 2).unwrap();

    let (ino, first_fh, _) = fuse
        .create_upload(ROOT_INO, OsStr::new("rapid.pdf"), libc::O_CREAT)
        .unwrap();
    let local_id = fuse.write_handles[&first_fh].record_remote_id.clone();
    let cache_path = fuse.write_handles[&first_fh].cache_path.clone();
    write_slice(&cache_path, 0, b"first generation").unwrap();
    fuse.inodes.set_size(ino, 16);
    fuse.queue_upload_handle(first_fh).unwrap();
    fuse.write_handles.remove(&first_fh);
    for _ in 0..100 {
        if fuse.backend.active_uploads.load(Ordering::SeqCst) > 0 {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(fuse.backend.active_uploads.load(Ordering::SeqCst) > 0);

    let record = fuse.db.get_by_path("/rapid.pdf").unwrap().unwrap();
    let (_, second_fh, _) = fuse.create_overwrite_upload(record, true).unwrap();
    let second_cache = fuse.write_handles[&second_fh].cache_path.clone();
    write_slice(&second_cache, 0, b"second generation").unwrap();
    fuse.queue_upload_handle(second_fh).unwrap();
    fuse.write_handles.remove(&second_fh);

    for _ in 0..80 {
        let record = fuse.db.get_by_remote_id(&local_id).unwrap().unwrap();
        if record.state == FileState::Cached && record.cloud_remote_id.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let record = fuse.db.get_by_remote_id(&local_id).unwrap().unwrap();
    assert_eq!(record.metadata.remote_id, local_id);
    assert_eq!(record.state, FileState::Cached);
    assert_eq!(
        fs::read(record.cache_path.unwrap()).unwrap(),
        b"second generation"
    );
    drop(fuse);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn stale_empty_upload_cannot_replace_completed_download() {
    let test = TestFs::new("stale-download-placeholder");
    let db = &test.fs.db;
    let old_cache = test.root.join("empty.cache");
    let new_cache = test.root.join("download.cache");
    fs::write(&old_cache, b"").unwrap();
    fs::write(&new_cache, b"%PDF-complete download").unwrap();
    for (id, path, cache, size) in [
        ("local-upload-placeholder", "/download.pdf", &old_cache, 0),
        ("local-upload-part", "/download.part", &new_cache, 22),
    ] {
        db.upsert_metadata(&MetadataEntry::new_file(id, path, size, 1, ""))
            .unwrap();
        db.mark_cached(id, cache).unwrap();
        db.mark_dirty_with_size(id, size).unwrap();
    }
    // The worker read the empty destination before its network preflight.
    let stale = db.get_by_path("/download.pdf").unwrap().unwrap();
    db.replace_file_locally(
        "local-upload-part",
        "local-upload-placeholder",
        "/download.pdf",
    )
    .unwrap();
    assert!(
        !recover_dirty_record(db, test.fs.backend.as_ref(), stale, &mut |_, _| Ok(())).unwrap()
    );
    let current = db.get_by_path("/download.pdf").unwrap().unwrap();
    assert_eq!(current.cache_path.as_ref(), Some(&new_cache));
    assert_eq!(current.metadata.size, 22);
    assert_eq!(current.state, FileState::Dirty);
    assert!(
        recover_dirty_record(db, test.fs.backend.as_ref(), current, &mut |_, _| Ok(())).unwrap()
    );
    let saved = db.get_by_path("/download.pdf").unwrap().unwrap();
    assert_eq!(
        test.fs
            .backend
            .download(saved.cloud_remote_id.as_deref().unwrap())
            .unwrap(),
        b"%PDF-complete download"
    );
}

#[test]
fn repeated_atomic_replacements_during_initial_upload_keep_latest_generation() {
    let root = test_root("repeated-atomic-replace");
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("test.sqlite3"));
    db.init().unwrap();
    let backend = ConcurrentBackend::new();
    sync_metadata(&db, &backend).unwrap();
    let mut fuse =
        TwoDriveFs::new_with_upload_concurrency(db, root.join("cache"), backend, 2).unwrap();

    let (target_ino, target_fh, _) = fuse
        .create_upload(ROOT_INO, OsStr::new("rapid.pdf"), libc::O_CREAT)
        .unwrap();
    let target_id = fuse.write_handles[&target_fh].record_remote_id.clone();
    let target_cache = fuse.write_handles[&target_fh].cache_path.clone();
    write_slice(&target_cache, 0, b"initial generation").unwrap();
    fuse.inodes.set_size(target_ino, 18);
    fuse.queue_upload_handle(target_fh).unwrap();
    fuse.write_handles.remove(&target_fh);
    for _ in 0..100 {
        if fuse.backend.active_uploads.load(Ordering::SeqCst) > 0 {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(fuse.backend.active_uploads.load(Ordering::SeqCst) > 0);

    let mut temp_ids = Vec::new();
    for (name, content) in [
        (".goutputstream-first", b"first saved generation".as_slice()),
        (
            ".goutputstream-second",
            b"second saved generation".as_slice(),
        ),
    ] {
        let (temp_ino, temp_fh, _) = fuse
            .create_upload(ROOT_INO, OsStr::new(name), libc::O_CREAT | libc::O_EXCL)
            .unwrap();
        temp_ids.push(fuse.write_handles[&temp_fh].record_remote_id.clone());
        let temp_cache = fuse.write_handles[&temp_fh].cache_path.clone();
        write_slice(&temp_cache, 0, content).unwrap();
        fuse.inodes.set_size(temp_ino, content.len() as u64);
        fuse.rename_record(
            ROOT_INO,
            OsStr::new(name),
            ROOT_INO,
            OsStr::new("rapid.pdf"),
        )
        .unwrap();
        assert_eq!(fuse.write_handles[&temp_fh].record_remote_id, target_id);
        fuse.queue_upload_handle(temp_fh).unwrap();
        fuse.write_handles.remove(&temp_fh);
    }

    for _ in 0..120 {
        let record = fuse.db.get_by_remote_id(&target_id).unwrap().unwrap();
        if record.state == FileState::Cached
            && record.cloud_remote_id.is_some()
            && fuse
                .backend
                .download(record.cloud_remote_id.as_deref().unwrap())
                .ok()
                .as_deref()
                == Some(b"second saved generation")
        {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }

    let record = fuse.db.get_by_path("/rapid.pdf").unwrap().unwrap();
    assert_eq!(record.metadata.remote_id, target_id);
    assert_eq!(record.state, FileState::Cached);
    assert_eq!(
        fs::read(record.cache_path.as_ref().unwrap()).unwrap(),
        b"second saved generation"
    );
    assert_eq!(
        fuse.backend
            .download(record.cloud_remote_id.as_deref().unwrap())
            .unwrap(),
        b"second saved generation"
    );
    for temp_id in temp_ids {
        assert!(fuse.db.get_by_remote_id(&temp_id).unwrap().is_none());
    }
    drop(fuse);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn release_requested_during_upload_finishes_after_last_open_handle() {
    let mut test = TestFs::new("release-after-upload");
    let (ino, fh, _) = test
        .fs
        .create_upload(ROOT_INO, OsStr::new("release.txt"), libc::O_CREAT)
        .unwrap();
    let cache = test.fs.write_handles[&fh].cache_path.clone();
    write_slice(&cache, 0, b"safe upload").unwrap();
    test.fs.inodes.set_size(ino, 11);
    let read_fh = test.fs.open_read_handle(ino, cache.clone()).unwrap();
    assert_eq!(test.fs.db.release_path("/release.txt").unwrap(), 0);
    test.fs.queue_upload_handle(fh).unwrap();
    test.fs.write_handles.remove(&fh);
    for _ in 0..120 {
        if test
            .fs
            .db
            .get_by_path("/release.txt")
            .unwrap()
            .unwrap()
            .state
            == FileState::Cached
        {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let uploaded = test.fs.db.get_by_path("/release.txt").unwrap().unwrap();
    assert_eq!(uploaded.state, FileState::Cached);
    assert_eq!(
        test.fs
            .backend
            .download(uploaded.cloud_remote_id.as_deref().unwrap())
            .unwrap(),
        b"safe upload"
    );
    assert_eq!(
        test.fs.read_open_handle(read_fh, 0, 11).unwrap(),
        b"safe upload"
    );
    assert!(cache.exists());
    test.fs.release_read_handle(read_fh).unwrap();
    assert!(!cache.exists());
    assert_eq!(
        test.fs
            .db
            .get_by_path("/release.txt")
            .unwrap()
            .unwrap()
            .state,
        FileState::OnlineOnly
    );
}

#[test]
fn closed_write_publishes_mtime_before_reopen_and_directory_refresh() {
    let mut test = TestFs::new("closed-write-mtime");
    let record = test
        .fs
        .db
        .get_by_path("/README-cloud.txt")
        .unwrap()
        .unwrap();
    let (ino, fh, _) = test.fs.create_overwrite_upload(record, true).unwrap();
    write_slice(&test.fs.write_handles[&fh].cache_path, 0, b"edited").unwrap();
    test.fs.queue_upload_handle(fh).unwrap();
    let published = test.fs.attr_for_ino(ino).unwrap();
    let current = test
        .fs
        .db
        .get_by_path("/README-cloud.txt")
        .unwrap()
        .unwrap();
    assert_eq!(published.size, 6);
    assert_eq!(published.mtime, unix_time(current.metadata.modified_unix));
    test.fs.refresh_child_records("/");
    assert_eq!(test.fs.attr_for_ino(ino).unwrap().mtime, published.mtime);
}

#[test]
fn editor_style_temp_file_can_replace_an_existing_remote_file() {
    let mut test = TestFs::new("atomic-replace");
    let old_target = test
        .fs
        .db
        .get_by_path("/README-cloud.txt")
        .unwrap()
        .unwrap();
    let old_target_ino = test.fs.inodes.ino_for_path("/README-cloud.txt").unwrap();
    let old_target_cache = test.fs.ensure_cached(&old_target).unwrap();
    let old_target_fh = test
        .fs
        .open_read_handle(old_target_ino, old_target_cache.clone())
        .unwrap();
    let before_replace = test.fs.attr_for_ino(old_target_ino).unwrap();
    let (ino, fh, _) = test
        .fs
        .create_upload(
            ROOT_INO,
            OsStr::new(".README-cloud.txt.tmp"),
            libc::O_CREAT | libc::O_EXCL,
        )
        .unwrap();
    let cache_path = test.fs.write_handles[&fh].cache_path.clone();
    write_slice(&cache_path, 0, b"new atomically saved content\n").unwrap();
    test.fs.inodes.set_size(ino, 29);
    test.fs.sync_handle(fh, false).unwrap();
    test.fs.upload_handle(fh).unwrap();

    test.fs
        .rename_record(
            ROOT_INO,
            OsStr::new(".README-cloud.txt.tmp"),
            ROOT_INO,
            OsStr::new("README-cloud.txt"),
        )
        .unwrap();

    assert_eq!(
        test.fs.read_open_handle(old_target_fh, 0, 7).unwrap(),
        b"Welcome"
    );
    let old_attr = test.fs.attr_for_ino(old_target_ino).unwrap();
    assert_eq!(old_attr.ino, old_target_ino);
    assert_eq!(
        old_attr.size,
        fs::metadata(&old_target_cache).unwrap().len()
    );
    assert_eq!(old_attr.kind, FileType::RegularFile);
    assert_eq!(old_attr.nlink, 0);
    assert_eq!(old_attr.mtime, before_replace.mtime);
    assert!(old_target_cache.exists());
    test.fs.release_read_handle(old_target_fh).unwrap();
    assert!(test.fs.attr_for_ino(old_target_ino).is_none());
    assert!(!old_target_cache.exists());

    for _ in 0..40 {
        let current = test
            .fs
            .db
            .get_by_path("/README-cloud.txt")
            .unwrap()
            .unwrap();
        if test
            .fs
            .backend
            .download(current.cloud_remote_id.as_deref().unwrap())
            .ok()
            .as_deref()
            == Some(b"new atomically saved content\n")
        {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let record = test
        .fs
        .db
        .get_by_path("/README-cloud.txt")
        .unwrap()
        .unwrap();
    assert_eq!(
        test.fs
            .backend
            .download(record.cloud_remote_id.as_deref().unwrap())
            .unwrap(),
        b"new atomically saved content\n"
    );
    assert!(
        test.fs
            .db
            .get_by_path("/.README-cloud.txt.tmp")
            .unwrap()
            .is_none()
    );
}

#[test]
fn read_only_handle_survives_unlink_until_the_last_release() {
    let mut test = TestFs::new("read-open-unlink");
    let record = test
        .fs
        .db
        .get_by_path("/README-cloud.txt")
        .unwrap()
        .unwrap();
    let ino = test.fs.inodes.ino_for_path("/README-cloud.txt").unwrap();
    let cache_path = test.fs.ensure_cached(&record).unwrap();
    let fh = test.fs.open_read_handle(ino, cache_path.clone()).unwrap();

    assert!(
        test.fs
            .unlink_record_with_open_handles(ino, &record)
            .unwrap()
    );
    assert!(
        test.fs
            .db
            .get_by_path("/README-cloud.txt")
            .unwrap()
            .is_none()
    );
    assert_eq!(test.fs.read_open_handle(fh, 0, 7).unwrap(), b"Welcome");
    assert!(cache_path.exists());

    test.fs.release_read_handle(fh).unwrap();
    assert!(!cache_path.exists());
}

#[test]
fn dirty_cache_is_replayed_after_a_simulated_restart() {
    let mut test = TestFs::new("dirty-recovery");
    let (_ino, fh, _) = test
        .fs
        .create_upload(ROOT_INO, OsStr::new("recover-me.txt"), libc::O_CREAT)
        .unwrap();
    let cache_path = test.fs.write_handles[&fh].cache_path.clone();
    write_slice(&cache_path, 0, b"durable before restart").unwrap();
    test.fs.sync_handle(fh, false).unwrap();
    let remote_id = test.fs.write_handles[&fh].record_remote_id.clone();
    test.fs.db.mark_state(&remote_id, FileState::Dirty).unwrap();

    assert_eq!(
        recover_dirty_uploads(&test.fs.db, test.fs.backend.as_ref()).unwrap(),
        1
    );
    let recovered = test.fs.db.get_by_path("/recover-me.txt").unwrap().unwrap();
    assert_eq!(recovered.state, FileState::Cached);
    assert_eq!(
        test.fs
            .backend
            .download(recovered.cloud_remote_id.as_deref().unwrap())
            .unwrap(),
        b"durable before restart"
    );
}

#[test]
fn incomplete_writing_cache_is_not_uploaded_after_restart() {
    let mut test = TestFs::new("writing-not-recovered");
    let (_ino, fh, _) = test
        .fs
        .create_upload(ROOT_INO, OsStr::new("partial.txt"), libc::O_CREAT)
        .unwrap();
    let remote_id = test.fs.write_handles[&fh].record_remote_id.clone();
    let cache_path = test.fs.write_handles[&fh].cache_path.clone();
    write_slice(&cache_path, 0, b"partial data").unwrap();
    test.fs.sync_handle(fh, false).unwrap();

    assert_eq!(
        recover_dirty_uploads(&test.fs.db, test.fs.backend.as_ref()).unwrap(),
        0
    );
    let record = test.fs.db.get_by_remote_id(&remote_id).unwrap().unwrap();
    assert_eq!(record.state, FileState::Writing);
    assert!(cache_path.exists());
    assert!(test.fs.backend.download(&remote_id).is_err());
}

#[test]
fn closed_small_files_upload_concurrently() {
    let root = test_root("concurrent-uploads");
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("test.sqlite3"));
    db.init().unwrap();
    let backend = ConcurrentBackend::new();
    sync_metadata(&db, &backend).unwrap();
    let mut fuse =
        TwoDriveFs::new_with_upload_concurrency(db, root.join("cache"), backend, 4).unwrap();
    let mut stale_record = None;

    for index in 0..4 {
        let name = format!("small-{index}.txt");
        let (_ino, fh, _) = fuse
            .create_upload(ROOT_INO, OsStr::new(&name), libc::O_CREAT)
            .unwrap();
        let cache_path = fuse.write_handles[&fh].cache_path.clone();
        write_slice(&cache_path, 0, name.as_bytes()).unwrap();
        if index == 0 {
            stale_record = fuse
                .db
                .get_by_remote_id(&fuse.write_handles[&fh].record_remote_id)
                .unwrap();
        }
        fuse.queue_upload_handle(fh).unwrap();
        fuse.write_handles.remove(&fh);
    }

    for _ in 0..100 {
        let completed = (0..4)
            .filter(|index| {
                fuse.db
                    .get_by_path(&format!("/small-{index}.txt"))
                    .ok()
                    .flatten()
                    .is_some_and(|record| record.state == FileState::Cached)
            })
            .count();
        if completed == 4 {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }

    assert_eq!(
        (0..4)
            .filter(|index| fuse
                .db
                .get_by_path(&format!("/small-{index}.txt"))
                .unwrap()
                .is_some_and(|record| record.state == FileState::Cached))
            .count(),
        4
    );
    assert!(
        fuse.backend.max_active_uploads.load(Ordering::SeqCst) >= 2,
        "expected at least two simultaneous uploads"
    );
    let stale_record = stale_record.unwrap();
    let refreshed = fuse.refresh_record(&stale_record);
    assert_eq!(refreshed.metadata.path, "/small-0.txt");
    assert_eq!(
        refreshed.metadata.remote_id, stale_record.metadata.remote_id,
        "the local identity must remain stable after upload"
    );
    assert!(refreshed.cloud_remote_id.is_some());
    drop(fuse);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn uploading_state_is_replayed_after_a_simulated_restart() {
    let test = TestFs::new("uploading-recovery");
    let cache_path = test.root.join("uploading.cache");
    fs::write(&cache_path, b"uploading before restart").unwrap();
    let metadata = MetadataEntry::new_file("uploading-id", "/uploading.txt", 0, 1, "etag");
    test.fs.db.upsert_metadata(&metadata).unwrap();
    test.fs.db.mark_cached("uploading-id", &cache_path).unwrap();
    test.fs
        .db
        .mark_state("uploading-id", FileState::Uploading)
        .unwrap();

    assert_eq!(
        recover_dirty_uploads(&test.fs.db, test.fs.backend.as_ref()).unwrap(),
        1
    );
    let recovered = test.fs.db.get_by_path("/uploading.txt").unwrap().unwrap();
    assert_eq!(recovered.state, FileState::Cached);
    assert_eq!(
        test.fs
            .backend
            .download(recovered.cloud_remote_id.as_deref().unwrap())
            .unwrap(),
        b"uploading before restart"
    );
}

#[test]
fn failed_upload_on_release_leaves_a_dirty_retry_record() {
    let root = test_root("failed-upload-release");
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("test.sqlite3"));
    db.init().unwrap();
    let backend = FailingBackend::new(true, false);
    sync_metadata(&db, &backend).unwrap();
    let mut fuse =
        TwoDriveFs::new(db, root.join("cache"), backend).expect("create failing test fs");

    let (_ino, fh, _) = fuse
        .create_upload(ROOT_INO, OsStr::new("offline.txt"), libc::O_CREAT)
        .unwrap();
    let cache_path = fuse.write_handles[&fh].cache_path.clone();
    write_slice(&cache_path, 0, b"kept dirty after failed upload").unwrap();
    fuse.sync_handle(fh, false).unwrap();

    assert!(fuse.upload_handle(fh).is_err());
    let record = fuse.db.get_by_path("/offline.txt").unwrap().unwrap();
    assert_eq!(record.state, FileState::Dirty);
    assert_eq!(record.cache_path.as_deref(), Some(cache_path.as_path()));
    assert_eq!(
        fs::read(cache_path).unwrap(),
        b"kept dirty after failed upload"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn stale_etag_upload_preserves_local_content_as_a_conflict_copy() {
    let root = test_root("conflict-upload");
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("test.sqlite3"));
    db.init().unwrap();
    let backend = ConflictBackend::new();
    sync_metadata(&db, &backend).unwrap();
    let mut fuse =
        TwoDriveFs::new(db, root.join("cache"), backend).expect("create conflict test fs");

    let (_ino, fh, _) = fuse
        .create_overwrite_upload(
            fuse.db.get_by_path("/README-cloud.txt").unwrap().unwrap(),
            true,
        )
        .unwrap();
    let cache_path = fuse.write_handles[&fh].cache_path.clone();
    write_slice(&cache_path, 0, b"local edit that lost the race").unwrap();
    fuse.sync_handle(fh, false).unwrap();

    fuse.upload_handle(fh).unwrap();
    let original = fuse.db.get_by_path("/README-cloud.txt").unwrap().unwrap();
    assert_eq!(original.state, FileState::OnlineOnly);
    assert!(original.cache_path.is_none());
    assert_eq!(original.metadata.size, 42);
    assert_eq!(original.metadata.etag, "etag-cloud-winner");

    let conflict = fuse
        .db
        .all_records()
        .unwrap()
        .into_iter()
        .find(|record| record.metadata.name.contains("TwoDrive conflict"))
        .expect("conflict copy metadata");
    assert_eq!(conflict.state, FileState::Cached);
    assert_eq!(
        fs::read(conflict.cache_path.unwrap()).unwrap(),
        b"local edit that lost the race"
    );
    assert_eq!(
        fuse.backend.download(&conflict.metadata.remote_id).unwrap(),
        b"local edit that lost the race"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn conflict_state_is_preserved_as_a_copy_after_restart() {
    let root = test_root("conflict-recovery");
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("test.sqlite3"));
    db.init().unwrap();
    let backend = ConflictBackend::new();
    sync_metadata(&db, &backend).unwrap();
    let mut fuse =
        TwoDriveFs::new(db, root.join("cache"), backend).expect("create conflict test fs");

    let (_ino, fh, _) = fuse
        .create_overwrite_upload(
            fuse.db.get_by_path("/README-cloud.txt").unwrap().unwrap(),
            true,
        )
        .unwrap();
    let cache_path = fuse.write_handles[&fh].cache_path.clone();
    write_slice(&cache_path, 0, b"conflict durable across restart").unwrap();
    fuse.sync_handle(fh, false).unwrap();
    fuse.db
        .mark_state("file-readme", FileState::Conflict)
        .unwrap();

    assert_eq!(
        recover_dirty_uploads(&fuse.db, fuse.backend.as_ref()).unwrap(),
        1
    );
    assert_eq!(
        recover_dirty_uploads(&fuse.db, fuse.backend.as_ref()).unwrap(),
        0
    );
    let original = fuse.db.get_by_path("/README-cloud.txt").unwrap().unwrap();
    assert_eq!(original.state, FileState::OnlineOnly);
    let conflict = fuse
        .db
        .all_records()
        .unwrap()
        .into_iter()
        .find(|record| record.metadata.name.contains("TwoDrive conflict"))
        .unwrap();
    assert_eq!(
        fuse.backend.download(&conflict.metadata.remote_id).unwrap(),
        b"conflict durable across restart"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn failed_backend_delete_is_hidden_locally_and_retried_later() {
    let root = test_root("pending-delete");
    fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("test.sqlite3"));
    db.init().unwrap();
    let backend = FailingBackend::new(false, true);
    sync_metadata(&db, &backend).unwrap();
    let mut fuse =
        TwoDriveFs::new(db, root.join("cache"), backend).expect("create failing delete fs");
    let ino = fuse.inodes.ino_for_path("/README-cloud.txt").unwrap();
    let record = fuse.record_for_ino(ino).unwrap();

    fuse.delete_record(ino, &record).unwrap();
    assert!(fuse.db.get_by_path("/README-cloud.txt").unwrap().is_none());
    let pending = fuse.db.pending_deletes().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].remote_id, "file-readme");
    assert!(fuse.inodes.ino_for_path("/README-cloud.txt").is_none());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn pending_delete_is_replayed_after_a_simulated_restart() {
    let test = TestFs::new("pending-delete-recovery");
    let record = test
        .fs
        .db
        .get_by_path("/README-cloud.txt")
        .unwrap()
        .unwrap();
    test.fs.db.queue_pending_delete(&record).unwrap();
    assert_eq!(test.fs.db.pending_deletes().unwrap().len(), 1);

    assert_eq!(
        recover_pending_deletes(&test.fs.db, test.fs.backend.as_ref()).unwrap(),
        1
    );
    assert!(test.fs.db.pending_deletes().unwrap().is_empty());
    assert!(test.fs.backend.download("file-readme").is_err());
}

#[test]
fn stale_local_upload_placeholder_can_be_deleted_and_recreated() {
    let mut test = TestFs::new("stale-local-placeholder");
    let metadata = MetadataEntry::new_file(
        "local-upload-stale-placeholder",
        "/stale-placeholder.bin",
        0,
        1,
        "etag-local-upload-stale-placeholder",
    );
    test.fs.db.upsert_metadata(&metadata).unwrap();
    let record = test
        .fs
        .db
        .get_by_remote_id(&metadata.remote_id)
        .unwrap()
        .unwrap();
    let ino = test.fs.inodes.insert_or_update(record.clone());

    test.fs.delete_record(ino, &record).unwrap();

    assert!(test.fs.db.pending_deletes().unwrap().is_empty());
    assert!(
        test.fs
            .db
            .get_by_path("/stale-placeholder.bin")
            .unwrap()
            .is_none()
    );
    let (_, fh, _) = test
        .fs
        .create_upload(ROOT_INO, OsStr::new("stale-placeholder.bin"), libc::O_CREAT)
        .unwrap();
    assert!(test.fs.write_handles.contains_key(&fh));
}

#[test]
fn legacy_pending_delete_for_local_placeholder_is_cleaned_without_cloud_call() {
    let test = TestFs::new("legacy-local-pending-delete");
    let metadata = MetadataEntry::new_file(
        "local-upload-legacy-pending",
        "/legacy-pending.bin",
        0,
        1,
        "etag-local-upload-legacy-pending",
    );
    test.fs.db.upsert_metadata(&metadata).unwrap();
    let record = test
        .fs
        .db
        .get_by_remote_id(&metadata.remote_id)
        .unwrap()
        .unwrap();
    test.fs.db.queue_pending_delete(&record).unwrap();

    assert_eq!(
        recover_pending_deletes(&test.fs.db, test.fs.backend.as_ref()).unwrap(),
        1
    );
    assert!(test.fs.db.pending_deletes().unwrap().is_empty());
}

#[test]
fn local_move_into_a_pinned_directory_hydrates_the_file() {
    let mut test = TestFs::new("move-into-pin");
    let courses = test.fs.db.get_by_path("/Courses").unwrap().unwrap();
    test.fs
        .db
        .set_explicit_pin(&courses.metadata.remote_id, true)
        .unwrap();
    let root_ino = ROOT_INO;
    let documents_ino = test.fs.inodes.ino_for_path("/Documents").unwrap();
    let courses_ino = test.fs.inodes.ino_for_path("/Courses").unwrap();

    test.fs
        .rename_record(
            documents_ino,
            OsStr::new("twodrive-notes.txt"),
            courses_ino,
            OsStr::new("twodrive-notes.txt"),
        )
        .unwrap();

    for _ in 0..40 {
        let moved = test
            .fs
            .db
            .get_by_path("/Courses/twodrive-notes.txt")
            .unwrap()
            .unwrap();
        if moved.state == FileState::Pinned && moved.cache_path.as_deref().is_some_and(Path::exists)
        {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }

    let moved = test
        .fs
        .db
        .get_by_path("/Courses/twodrive-notes.txt")
        .unwrap()
        .unwrap();
    assert!(moved.effective_pinned());
    assert_eq!(moved.state, FileState::Pinned);
    assert!(moved.cache_path.as_deref().is_some_and(Path::exists));
    assert_eq!(
        test.fs.inodes.parent_ino(&moved.metadata.path),
        Some(courses_ino)
    );
    assert_eq!(test.fs.inodes.parent_ino("/Courses"), Some(root_ino));
}

#[test]
fn cloud_file_discovered_later_stays_online_only_under_a_pinned_directory() {
    let test = TestFs::new("cloud-file-online-only");
    let courses = test.fs.db.get_by_path("/Courses").unwrap().unwrap();
    test.fs
        .db
        .set_explicit_pin(&courses.metadata.remote_id, true)
        .unwrap();
    test.fs
        .backend
        .upload("/Courses/new-from-cloud.txt", b"cloud content".to_vec())
        .unwrap();

    sync_metadata(&test.fs.db, test.fs.backend.as_ref()).unwrap();
    hydrate_pending_pins(&test.fs.db, &test.fs.cache_dir, test.fs.backend.as_ref()).unwrap();

    let discovered = test
        .fs
        .db
        .get_by_path("/Courses/new-from-cloud.txt")
        .unwrap()
        .unwrap();
    assert_eq!(discovered.state, FileState::OnlineOnly);
    assert!(discovered.pin_inheritance_blocked);
    assert!(!discovered.effective_pinned());
    assert!(discovered.cache_path.is_none());
}
