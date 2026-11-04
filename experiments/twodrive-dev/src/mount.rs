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
    sync_delta_metadata(&db, &backend)?;
    if writable {
        return mount_backend(target, db, cache, backend, 4);
    }
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
