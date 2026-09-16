use fuser::MountOption;
use std::fs::{self};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::mpsc::{self};
use std::thread::{self};
use std::time::Duration;
use twodrive_backend::{CloudBackend, GraphBackend, MockBackend};
use twodrive_core::{AppPaths, Config, Database};

use crate::filesystem::TwoDriveFs;
use crate::metadata::{sync_delta_metadata, sync_metadata};
use crate::recovery::{enqueue_recovery, recover_interrupted_writes};

pub fn mount_mock(paths: AppPaths) -> anyhow::Result<()> {
    cleanup_stale_mountpoint(&paths.mount_dir);
    paths.ensure()?;
    let db = Database::new(paths.db_path.clone());
    db.init()?;
    let backend = MockBackend::new();
    let upload_concurrency = Config::load_or_create(&paths)?
        .power
        .ac_upload_concurrency
        .max(1) as usize;
    let count = sync_metadata(&db, &backend)?;
    println!("twodrive: loaded {count} mock metadata entries");
    println!("twodrive: database {}", db.path().display());
    println!("twodrive: cache {}", paths.cache_dir.display());
    println!("twodrive: mounting {}", paths.mount_dir.display());

    mount_backend(
        paths.mount_dir.clone(),
        db,
        paths.cache_dir.clone(),
        backend,
        upload_concurrency,
    )?;
    Ok(())
}

pub fn mount_graph(paths: AppPaths) -> anyhow::Result<()> {
    cleanup_stale_mountpoint(&paths.mount_dir);
    paths.ensure()?;
    let db = Database::new(paths.db_path.clone());
    db.init()?;
    let backend = GraphBackend::from_paths(&paths)?;
    let upload_concurrency = Config::load_or_create(&paths)?
        .power
        .ac_upload_concurrency
        .max(1) as usize;
    println!("twodrive: database {}", db.path().display());
    println!("twodrive: cache {}", paths.cache_dir.display());
    println!("twodrive: mounting {}", paths.mount_dir.display());

    mount_backend(
        paths.mount_dir.clone(),
        db,
        paths.cache_dir.clone(),
        backend,
        upload_concurrency,
    )
}

pub fn mount_backend<B: CloudBackend>(
    mount_dir: PathBuf,
    db: Database,
    cache_dir: PathBuf,
    backend: B,
    upload_concurrency: usize,
) -> anyhow::Result<()> {
    // No live handles exist yet; reconcile the durable local cache before serving requests.
    recover_interrupted_writes(&db)?;
    let fs = TwoDriveFs::new_with_upload_concurrency(db, cache_dir, backend, upload_concurrency)?;
    let (stop_tx, stop_rx) = mpsc::channel();
    let recovery_db = fs.db.clone();
    let recovery_backend = Arc::clone(&fs.backend);
    let sender = fs.upload_pool.sender.clone();
    let recovery = thread::spawn(move || {
        loop {
            // Replay via the normal workers so recovery and new saves share item locks.
            if let Err(err) = enqueue_recovery(&recovery_db, &sender) {
                eprintln!("twodrive: recovery scan failed: {err:#}");
            }
            if let Err(err) = sync_delta_metadata(&recovery_db, recovery_backend.as_ref()) {
                eprintln!("twodrive: metadata refresh deferred: {err:#}");
            }
            match stop_rx.recv_timeout(Duration::from_secs(60)) {
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                _ => break,
            }
        }
    });
    let options = [
        MountOption::FSName("twodrive".to_string()),
        MountOption::DefaultPermissions,
    ];
    let result = fuser::mount2(fs, &mount_dir, &options);
    let _ = stop_tx.send(());
    let _ = recovery.join();
    result?;
    Ok(())
}

pub(crate) fn cleanup_stale_mountpoint(mount_dir: &Path) {
    match fs::metadata(mount_dir) {
        Ok(_) => {}
        Err(err) if err.raw_os_error() == Some(libc::ENOTCONN) => {
            let _ = Command::new("fusermount3")
                .arg("-uz")
                .arg(mount_dir)
                .status();
        }
        Err(_) => {}
    }
}
