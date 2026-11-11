//! Recover only a disconnected native mount with a verified local owner.
use super::*;
use std::path::Component;

#[derive(Clone, Debug, PartialEq, Eq)]
struct MountInfo {
    id: String,
    kind: String,
    source: String,
    uid: Option<u32>,
}

fn mount_info(text: &str, path: &Path) -> Option<MountInfo> {
    let mut found = None;
    for line in text.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let fields: Vec<_> = left.split_whitespace().collect();
        let Some(target) = fields.get(4) else {
            continue;
        };
        let target = target
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\");
        if Path::new(&target) != path {
            continue;
        }
        // Stacked mounts have ambiguous ownership. Leave them untouched.
        if found.is_some() {
            return None;
        }
        let mut parts = right.split_whitespace();
        found = Some(MountInfo {
            id: fields[0].into(),
            kind: parts.next()?.into(),
            source: parts.next()?.into(),
            uid: parts
                .next()?
                .split(',')
                .find_map(|v| v.strip_prefix("user_id=")?.parse().ok()),
        });
    }
    found
}

fn recover_with(
    cache: &State,
    binding: &str,
    uid: u32,
    mut inspect: impl FnMut() -> anyhow::Result<Option<MountInfo>>,
    mut disconnected: impl FnMut() -> bool,
    mut detach: impl FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<bool> {
    let Some(original) = inspect()? else {
        return Ok(false);
    };
    if original.kind != "fuse"
        || original.source != "twodrive"
        || original.uid != Some(uid)
        || binding.is_empty()
    {
        return Ok(false);
    }
    // The worker holds this same lock for its complete mount lifetime.
    let Ok(_lock) = cache.run_lock() else {
        return Ok(false);
    };
    let Ok(saved) = cache.read::<Value>("binding.json") else {
        return Ok(false);
    };
    if saved["provider"].as_str() != Some(binding) {
        return Ok(false);
    }
    if !disconnected() {
        return Ok(false);
    }
    // Recheck kernel identity and ENOTCONN after acquiring the worker lock.
    if inspect()?.as_ref() != Some(&original) || !disconnected() {
        return Ok(false);
    }
    detach()?;
    ensure!(
        inspect()?.is_none(),
        "旧挂载仍在，已保留全部缓存，请稍后重试"
    );
    Ok(true)
}

fn disconnected(path: &Path) -> bool {
    // Native attributes are cached for 30s. A metadata-only directory request
    // detects dead FUSE immediately, even while stat still returns cached attrs.
    fs::read_dir(path)
        .and_then(|mut entries| entries.next().transpose().map(|_| ()))
        .err()
        .is_some_and(|e| e.raw_os_error() == Some(libc::ENOTCONN))
}

pub(super) fn recover(path: &Path, cache: &Path, binding: &str) -> anyhow::Result<bool> {
    if !path.is_absolute()
        || path.file_name().is_none()
        || !path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
        || !cache.is_dir()
    {
        return Ok(false);
    }
    let cache = State::new(cache.to_owned())?;
    recover_with(
        &cache,
        binding,
        unsafe { libc::geteuid() },
        || {
            Ok(mount_info(
                &fs::read_to_string("/proc/self/mountinfo")?,
                path,
            ))
        },
        || disconnected(path),
        || {
            // No lazy/forced detach and no cache removal. Recovery of pending
            // edits belongs to the new worker's existing durable recovery path.
            let result = Command::new("fusermount3")
                .args(["-u", "--"])
                .arg(path)
                .output()?;
            ensure!(
                result.status.success(),
                "失效挂载仍被占用，请关闭该目录后重试；缓存已保留"
            );
            Ok(())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, collections::VecDeque};

    fn entry() -> MountInfo {
        MountInfo {
            id: "12".into(),
            kind: "fuse".into(),
            source: "twodrive".into(),
            uid: Some(1000),
        }
    }
    fn fixture() -> (tempfile::TempDir, State) {
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path().join("private")).unwrap();
        state
            .save(
                "binding.json",
                &json!({"provider":"account-binding","writable":true}),
            )
            .unwrap();
        (root, state)
    }
    #[test]
    fn owned_dead_native_mount_recovers_without_touching_pending_bytes() {
        let (_root, cache) = fixture();
        fs::write(cache.dir.join("pending-edit"), b"synthetic unsent bytes").unwrap();
        let mut entries = VecDeque::from([Some(entry()), Some(entry()), None]);
        let detached = Cell::new(false);
        assert!(
            recover_with(
                &cache,
                "account-binding",
                1000,
                || Ok(entries.pop_front().unwrap()),
                || true,
                || {
                    detached.set(true);
                    Ok(())
                }
            )
            .unwrap()
        );
        assert!(detached.get());
        assert_eq!(
            fs::read(cache.dir.join("pending-edit")).unwrap(),
            b"synthetic unsent bytes"
        );
    }
    #[test]
    fn live_foreign_or_unbound_mounts_are_never_detached() {
        let (_root, cache) = fixture();
        for modified in [
            MountInfo {
                kind: "fuse.rclone".into(),
                ..entry()
            },
            MountInfo {
                source: "another-filesystem".into(),
                ..entry()
            },
            MountInfo {
                uid: Some(1001),
                ..entry()
            },
            MountInfo {
                uid: None,
                ..entry()
            },
        ] {
            assert!(
                !recover_with(
                    &cache,
                    "account-binding",
                    1000,
                    || Ok(Some(modified.clone())),
                    || true,
                    || panic!("foreign mount detached")
                )
                .unwrap()
            );
        }
        for (binding, dead) in [
            ("another-account", true),
            ("", true),
            ("account-binding", false),
        ] {
            assert!(
                !recover_with(
                    &cache,
                    binding,
                    1000,
                    || Ok(Some(entry())),
                    || dead,
                    || panic!("live/unbound mount detached")
                )
                .unwrap()
            );
        }
    }
    #[test]
    fn worker_lock_and_mount_identity_change_block_recovery() {
        let (_root, cache) = fixture();
        let lock = cache.run_lock().unwrap();
        assert!(
            !recover_with(
                &cache,
                "account-binding",
                1000,
                || Ok(Some(entry())),
                || true,
                || panic!("live worker detached")
            )
            .unwrap()
        );
        drop(lock);
        let mut entries = VecDeque::from([
            Some(entry()),
            Some(MountInfo {
                id: "new".into(),
                ..entry()
            }),
        ]);
        assert!(
            !recover_with(
                &cache,
                "account-binding",
                1000,
                || Ok(entries.pop_front().unwrap()),
                || true,
                || panic!("replacement mount detached")
            )
            .unwrap()
        );
        let calls = Cell::new(0);
        assert!(
            !recover_with(
                &cache,
                "account-binding",
                1000,
                || Ok(Some(entry())),
                || {
                    calls.set(calls.get() + 1);
                    calls.get() == 1
                },
                || panic!("reconnected mount detached")
            )
            .unwrap()
        );
    }
    #[test]
    fn escaped_mount_path_uid_and_stacked_mounts_are_checked() {
        let line = "12 4 0:40 / /tmp/TwoDrive\\040Development/google rw - fuse twodrive rw,user_id=1000,group_id=1000";
        let path = Path::new("/tmp/TwoDrive Development/google");
        assert_eq!(mount_info(line, path), Some(entry()));
        assert_eq!(mount_info(&format!("{line}\n{line}"), path), None);
        assert_eq!(mount_info(line, Path::new("/tmp/other")), None);
    }

    // A child test process lets SIGTERM/SIGKILL reproduce desktop session loss
    // without touching a real desktop, network service or user account.
    #[test]
    fn native_fuse_fixture_worker() {
        let Some(root) = std::env::var_os("TWODRIVE_STALE_NATIVE_FIXTURE") else {
            return;
        };
        let root = PathBuf::from(root);
        super::super::mount::mount(
            State::new(root.join("private")).unwrap(),
            root.join("mount"),
            Backend::new(super::super::test_provider::OpaqueProvider::default()),
            "synthetic-owned-binding".into(),
            true,
        )
        .unwrap();
    }
    #[test]
    #[ignore = "requires /dev/fuse; kills only private synthetic workers"]
    fn native_fuse_session_loss_recovers_and_remounts_without_cache_loss() {
        struct Worker {
            child: Child,
            path: PathBuf,
        }
        impl Drop for Worker {
            fn drop(&mut self) {
                let _ = self.child.kill();
                let _ = self.child.wait();
                let _ = Command::new("fusermount3")
                    .args(["-u", "--"])
                    .arg(&self.path)
                    .output();
            }
        }
        let spawn = |root: &Path| {
            let child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "desktop::stale_mount::tests::native_fuse_fixture_worker",
                    "--nocapture",
                ])
                .env("TWODRIVE_STALE_NATIVE_FIXTURE", root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let mut worker = Worker {
                child,
                path: root.join("mount"),
            };
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                assert!(
                    worker.child.try_wait().unwrap().is_none(),
                    "fixture worker exited before mount"
                );
                if is_mount(&worker.path).unwrap() && fs::metadata(&worker.path).is_ok() {
                    break;
                }
                assert!(Instant::now() < deadline, "fixture mount timeout");
                thread::sleep(Duration::from_millis(25));
            }
            worker
        };
        for signal in [libc::SIGTERM, libc::SIGKILL] {
            let root = tempfile::tempdir().unwrap();
            fs::create_dir(root.path().join("mount")).unwrap();
            let mut worker = spawn(root.path());
            let cache = State::new(root.path().join("private")).unwrap();
            fs::write(
                cache.dir.join("pending-sentinel"),
                b"synthetic bytes to preserve",
            )
            .unwrap();
            unsafe {
                libc::kill(worker.child.id() as i32, signal);
            }
            worker.child.wait().unwrap();
            assert!(
                disconnected(&worker.path),
                "dead FUSE directory request did not report ENOTCONN"
            );
            // The prior parent preflight rejected this still-mounted target.
            assert!(is_mount(&worker.path).unwrap());
            assert!(recover(&worker.path, &cache.dir, "synthetic-owned-binding").unwrap());
            assert!(!is_mount(&worker.path).unwrap());
            assert_eq!(
                fs::read(cache.dir.join("pending-sentinel")).unwrap(),
                b"synthetic bytes to preserve"
            );
            drop(worker);
            let mut remounted = spawn(root.path());
            assert!(
                !recover(&remounted.path, &cache.dir, "synthetic-owned-binding").unwrap(),
                "live remount was detached"
            );
            assert!(
                Command::new("fusermount3")
                    .args(["-u", "--"])
                    .arg(&remounted.path)
                    .status()
                    .unwrap()
                    .success()
            );
            assert!(remounted.child.wait().unwrap().success());
        }
    }
}
