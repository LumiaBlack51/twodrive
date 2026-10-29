//! Real FUSE over the editable private bridge; synthetic metadata/bytes only.
#![cfg(target_os = "linux")]
#[path = "support/opaque_provider.rs"]
mod opaque;
use opaque::OpaqueProvider;
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use twodrive_dev::{model::CloudBackend, provider::Backend, readonly_mount::Bridge};
struct Mount {
    target: PathBuf,
    process: Child,
}
impl Drop for Mount {
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
fn mounted(path: &Path) -> bool {
    std::fs::read_to_string("/proc/self/mountinfo")
        .unwrap()
        .lines()
        .any(|line| line.split_whitespace().nth(4) == path.to_str())
}
fn wait(mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "synthetic FUSE operation timed out"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
#[test]
#[ignore = "requires Linux /dev/fuse and TWODRIVE_TEST_RCLONE; synthetic provider only"]
fn editable_bridge_fuse_lists_reads_writes_renames_deletes_and_keeps_other_account() {
    let rclone =
        std::env::var_os("TWODRIVE_TEST_RCLONE").expect("explicit pinned component required");
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("mount");
    std::fs::create_dir(&target).unwrap();
    let provider = OpaqueProvider::default().with_caps(twodrive_dev::provider::Capabilities {
        upload: true,
        conditional_upload: true,
        mkdir: true,
        rename: true,
        delete: true,
    });
    provider.upload("/file.txt", b"original".to_vec()).unwrap();
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
    let process = Command::new(rclone)
        .args(["mount", "TWODRIVE:"])
        .arg(&target)
        .args([
            "--config=/dev/null",
            "--ask-password=false",
            "--vfs-cache-mode=full",
            "--vfs-write-back=0s",
            "--dir-cache-time=100ms",
            "--log-level=ERROR",
            "--retries=1",
            "--low-level-retries=1",
        ])
        .arg("--cache-dir")
        .arg(dir.path().join("cache"))
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
    let mut mount = Mount {
        target: target.clone(),
        process,
    };
    wait(|| {
        assert!(
            mount.process.try_wait().unwrap().is_none(),
            "synthetic component exited"
        );
        mounted(&target)
    });
    assert_eq!(std::fs::read_dir(&target).unwrap().count(), 1);
    assert_eq!(std::fs::read(target.join("file.txt")).unwrap(), b"original");
    std::fs::write(target.join("file.txt"), b"edited through native FUSE").unwrap();
    wait(|| {
        provider
            .get_metadata_by_path("/file.txt")
            .unwrap()
            .is_some_and(|m| {
                provider.download(&m.remote_id).unwrap() == b"edited through native FUSE"
            })
    });
    std::fs::write(target.join("new.txt"), b"new fixture bytes").unwrap();
    wait(|| provider.get_metadata_by_path("/new.txt").unwrap().is_some());
    std::fs::rename(target.join("new.txt"), target.join("renamed.txt")).unwrap();
    wait(|| {
        provider
            .get_metadata_by_path("/renamed.txt")
            .unwrap()
            .is_some()
    });
    std::fs::remove_file(target.join("renamed.txt")).unwrap();
    wait(|| {
        provider
            .get_metadata_by_path("/renamed.txt")
            .unwrap()
            .is_none()
    });
    std::fs::create_dir(target.join("folder")).unwrap();
    wait(|| provider.get_metadata_by_path("/folder").unwrap().is_some());
    std::fs::remove_dir(target.join("folder")).unwrap();
    wait(|| provider.get_metadata_by_path("/folder").unwrap().is_none());
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
    let status = Command::new("fusermount3")
        .arg("-u")
        .arg(&target)
        .status()
        .unwrap();
    assert!(status.success());
    wait(|| !mounted(&target));
    mount.process.wait().unwrap();
    assert_eq!(
        twodrive_dev::mount_write::recovery_status(&dir.path().join("recovery")).unwrap()["files"],
        0
    );
}
