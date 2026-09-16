use super::activity::ActivityEntry;
use super::scan::parent_cloud_path;
use super::state::{
    FileSnapshot, KnownFolderState, KnownFolderUploadJob, PendingKnownFolderUpload,
};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use twodrive_backend::CloudBackend;
use twodrive_core::{AppPaths, Config, Database, join_cloud_path, normalize_cloud_path};

pub(super) fn retry_known_folder_uploads<B: CloudBackend>(
    paths: &AppPaths,
    backend: &B,
    db: &Database,
    config: &Config,
    state: &mut KnownFolderState,
) -> anyhow::Result<()> {
    state.prune_missing_pending();
    let jobs = state
        .pending
        .iter()
        .filter_map(|(local, pending)| {
            let metadata = fs::metadata(local).ok()?;
            Some(KnownFolderUploadJob {
                local_path: PathBuf::from(local),
                remote_path: pending.remote_path.clone(),
                snapshot: FileSnapshot::from_metadata(&metadata),
            })
        })
        .collect();
    state.save(&paths.data_dir)?;
    process_known_folder_uploads(paths, backend, db, config, jobs, state)
}

pub(super) fn process_known_folder_uploads<B: CloudBackend>(
    paths: &AppPaths,
    backend: &B,
    db: &Database,
    config: &Config,
    jobs: Vec<KnownFolderUploadJob>,
    state: &mut KnownFolderState,
) -> anyhow::Result<()> {
    if jobs.is_empty() {
        return Ok(());
    }
    for job in &jobs {
        state.pending.insert(
            job.local_path.to_string_lossy().into_owned(),
            PendingKnownFolderUpload {
                remote_path: job.remote_path.clone(),
                snapshot: job.snapshot.clone(),
            },
        );
    }
    state.save(&paths.data_dir)?;

    let jobs = Arc::new(Mutex::new(jobs.into_iter()));
    let (result_tx, result_rx) = mpsc::channel();
    let concurrency = config.power.ac_upload_concurrency.max(1) as usize;
    thread::scope(|scope| -> anyhow::Result<()> {
        for _ in 0..concurrency {
            let jobs = Arc::clone(&jobs);
            let result_tx = result_tx.clone();
            scope.spawn(move || {
                loop {
                    let job = match jobs.lock() {
                        Ok(mut jobs) => jobs.next(),
                        Err(_) => return,
                    };
                    let Some(job) = job else {
                        return;
                    };
                    let result = upload_known_folder_file(paths, backend, db, &job);
                    let _ = result_tx.send((job, result));
                }
            });
        }
        drop(result_tx);

        for (job, result) in result_rx {
            let local_key = job.local_path.to_string_lossy().into_owned();
            match result {
                Ok(uploaded) => {
                    db.upsert_metadata(&uploaded)?;
                    state.files.insert(local_key.clone(), job.snapshot);
                    state.pending.remove(&local_key);
                }
                Err(err) => {
                    eprintln!(
                        "twodrive: known folder upload remains queued for {}: {err:#}",
                        job.local_path.display()
                    );
                }
            }
            state.save(&paths.data_dir)?;
        }
        Ok(())
    })?;
    Ok(())
}

pub(super) fn upload_known_folder_file<B: CloudBackend>(
    paths: &AppPaths,
    backend: &B,
    db: &Database,
    job: &KnownFolderUploadJob,
) -> anyhow::Result<twodrive_core::MetadataEntry> {
    let local_path = &job.local_path;
    let remote_path = &job.remote_path;
    let snapshot = &job.snapshot;

    let name = local_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let mut activity = ActivityEntry::start(
        &paths.data_dir,
        "upload",
        remote_path,
        name,
        Some(snapshot.size),
    );
    let before = fs::metadata(local_path)?;
    if FileSnapshot::from_metadata(&before) != *snapshot {
        anyhow::bail!("local file changed before upload started");
    }
    if let Some(parent) = parent_cloud_path(remote_path) {
        ensure_remote_dir(backend, db, &parent)?;
    }
    let existing = db.get_by_path(remote_path)?;
    let uploaded = backend.upload_file_with_version(
        remote_path,
        local_path,
        existing
            .as_ref()
            .and_then(|record| record.cloud_remote_id.as_deref()),
        existing
            .as_ref()
            .map(|record| record.metadata.etag.as_str()),
        &mut |done, total| {
            activity.set_progress(done, Some(total));
            Ok(())
        },
    )?;
    let after = fs::metadata(local_path)?;
    let after_snapshot = FileSnapshot::from_metadata(&after);
    if after_snapshot != *snapshot {
        anyhow::bail!("local file changed while uploading");
    }
    activity.finish();
    Ok(uploaded)
}

pub(super) fn ensure_remote_dir<B: CloudBackend>(
    backend: &B,
    db: &Database,
    remote_dir: &str,
) -> anyhow::Result<()> {
    let remote_dir = normalize_cloud_path(remote_dir);
    if remote_dir == "/" {
        return Ok(());
    }

    let mut current = String::from("/");
    for part in remote_dir.trim_matches('/').split('/') {
        current = join_cloud_path(&current, part);
        if db.get_by_path(&current)?.is_some() {
            continue;
        }
        match backend.create_folder(&current) {
            Ok(entry) => db.upsert_metadata(&entry)?,
            Err(err) => {
                eprintln!("twodrive: create folder skipped for {current}: {err:#}");
            }
        }
    }
    Ok(())
}
