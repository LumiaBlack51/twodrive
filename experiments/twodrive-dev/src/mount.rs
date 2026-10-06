use anyhow::ensure;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::mpsc, thread, time::Duration};
use twodrive_core::Database;
use twodrive_dev::{state::State, webdav::WebDavBackend};
use twodrive_fs::{TwoDriveFs, mount_backend, sync_delta_metadata};

#[derive(Serialize, Deserialize, PartialEq)]
struct Binding {
    provider: String,
    writable: bool,
}

pub fn mount(
    state: State,
    target: PathBuf,
    backend: WebDavBackend,
    provider: String,
    writable: bool,
) -> anyhow::Result<()> {
    let _lock = state.run_lock()?;
    let binding = Binding { provider, writable };
    if state.dir.join("binding.json").exists() {
        ensure!(
            state.read::<Binding>("binding.json")? == binding,
            "mount state belongs to another provider or permission mode; choose a new state"
        );
    } else {
        state.save("binding.json", &binding)?;
    }
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
