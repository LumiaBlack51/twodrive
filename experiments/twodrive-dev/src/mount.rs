use anyhow::ensure;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::mpsc, thread, time::Duration};
use twodrive_core::Database;
use twodrive_dev::{model::CloudBackend, state::State};
use twodrive_fs::{TwoDriveFs, mount_backend, sync_delta_metadata};

#[derive(Serialize, Deserialize, PartialEq)]
struct Binding {
    provider: String,
    writable: bool,
}

fn bind_state(state: &State, provider: String, writable: bool) -> anyhow::Result<()> {
    let binding = Binding { provider, writable };
    if state.dir.join("binding.json").exists() {
        // Historical files also record the last permission mode. A restart
        // must be able to relock the same storage without abandoning its edits.
        ensure!(
            state.read::<Binding>("binding.json")?.provider == binding.provider,
            "mount state belongs to another provider; choose a new state"
        );
    }
    state.save("binding.json", &binding)?;
    Ok(())
}

pub fn mount<B: CloudBackend + Clone>(
    state: State,
    target: PathBuf,
    backend: B,
    provider: String,
    writable: bool,
) -> anyhow::Result<()> {
    let _lock = state.run_lock()?;
    bind_state(&state, provider, writable)?;
    std::fs::create_dir_all(&target)?;
    let target = target.canonicalize()?;
    ensure!(
        target != std::path::Path::new("/")
            && !state.dir.starts_with(&target)
            && !target.starts_with(&state.dir),
        "mount and private state must be separate"
    );
    ensure!(
        std::fs::read_dir(&target)?.next().is_none(),
        "mount target must be empty"
    );
    let cache = state.dir.join("cache");
    twodrive_dev::state::private_dir(&cache)?;
    let db = Database::new(state.dir.join("mount.sqlite3"));
    db.init()?;
    if writable {
        // The writable mount already refreshes metadata on its recovery thread.
        // Serving the local cache must not wait for a complete cloud scan.
        return mount_backend(target, db, cache, backend, 4);
    }
    sync_delta_metadata(&db, &backend)?;
    let fs = TwoDriveFs::new(db.clone(), cache, backend.clone())?;
    let (stop, stopped) = mpsc::channel();
    let refresh = thread::spawn(move || {
        while let Err(mpsc::RecvTimeoutError::Timeout) =
            stopped.recv_timeout(Duration::from_secs(60))
        {
            if sync_delta_metadata(&db, &backend).is_err() {
                eprintln!("twodrive-dev: metadata refresh deferred");
            }
        }
    });
    let result = fuser::mount2(
        fs,
        target,
        &[
            fuser::MountOption::FSName("twodrive-dev".into()),
            fuser::MountOption::DefaultPermissions,
            fuser::MountOption::RO,
        ],
    );
    let _ = stop.send(());
    let _ = refresh.join();
    result?;
    Ok(())
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    use std::sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use twodrive_backend::{CloudBackend, DeltaResult, MockBackend};
    use twodrive_core::MetadataEntry;

    #[derive(Clone)]
    struct DelayedMetadata {
        inner: Arc<MockBackend>,
        started: Arc<AtomicBool>,
        release: Arc<(Mutex<bool>, Condvar)>,
    }
    impl CloudBackend for DelayedMetadata {
        fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
            self.inner.list_all()
        }
        fn list_delta(&self, cursor: Option<&str>) -> anyhow::Result<DeltaResult> {
            self.started.store(true, Ordering::SeqCst);
            let (lock, wake) = &*self.release;
            let mut allowed = lock.lock().unwrap();
            while !*allowed {
                allowed = wake.wait(allowed).unwrap();
            }
            self.inner.list_delta(cursor)
        }
        fn download(&self, id: &str) -> anyhow::Result<Vec<u8>> {
            self.inner.download(id)
        }
        fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry> {
            self.inner.upload(path, content)
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
    }
    fn mounted(path: &std::path::Path) -> bool {
        std::process::Command::new("mountpoint")
            .arg("-q")
            .arg(path)
            .status()
            .is_ok_and(|s| s.success())
    }
    #[test]
    #[ignore = "requires /dev/fuse; uses only private synthetic metadata"]
    fn writable_mount_does_not_wait_for_cloud_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("mount");
        let state = State::new(temp.path().join("state")).unwrap();
        let started = Arc::new(AtomicBool::new(false));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let backend = DelayedMetadata {
            inner: Arc::new(MockBackend::new()),
            started: started.clone(),
            release: release.clone(),
        };
        let thread_target = target.clone();
        let runner = thread::spawn(move || {
            mount(
                state,
                thread_target,
                backend,
                "synthetic-delayed-metadata".into(),
                true,
            )
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline
            && !(started.load(Ordering::SeqCst) && mounted(&target))
        {
            thread::sleep(Duration::from_millis(25));
        }
        let ready_before_metadata = mounted(&target);
        let metadata_started = started.load(Ordering::SeqCst);
        let (lock, wake) = &*release;
        *lock.lock().unwrap() = true;
        wake.notify_all();
        let cleanup_deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !mounted(&target)
            && !runner.is_finished()
            && std::time::Instant::now() < cleanup_deadline
        {
            thread::sleep(Duration::from_millis(25));
        }
        if mounted(&target) {
            let unmounted = std::process::Command::new("fusermount3")
                .arg("-u")
                .arg(&target)
                .status()
                .unwrap();
            assert!(unmounted.success(), "synthetic mount cleanup failed");
        }
        runner.join().unwrap().unwrap();
        assert!(metadata_started, "synthetic cloud refresh was not reached");
        assert!(
            ready_before_metadata,
            "writable mount waited for the initial cloud scan"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_writable_binding_can_relock_and_unlock_without_losing_pending_content() {
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path().join("private")).unwrap();
        bind_state(&state, "same-account-and-root".into(), true).unwrap();
        let cache = state.dir.join("cache");
        std::fs::create_dir(&cache).unwrap();
        let pending = cache.join("pending-edit.content");
        std::fs::write(&pending, b"unuploaded local bytes").unwrap();
        // The exact historical binding remains valid when a new engine run
        // relocks the account. Permission is not storage identity.
        bind_state(&state, "same-account-and-root".into(), false).unwrap();
        assert!(!state.read::<Binding>("binding.json").unwrap().writable);
        assert_eq!(std::fs::read(&pending).unwrap(), b"unuploaded local bytes");
        bind_state(&state, "same-account-and-root".into(), true).unwrap();
        assert_eq!(std::fs::read(&pending).unwrap(), b"unuploaded local bytes");
        let before = std::fs::read(state.dir.join("binding.json")).unwrap();
        assert!(bind_state(&state, "different-account-or-root".into(), false).is_err());
        assert_eq!(
            std::fs::read(state.dir.join("binding.json")).unwrap(),
            before
        );
        assert_eq!(std::fs::read(pending).unwrap(), b"unuploaded local bytes");
    }
}
