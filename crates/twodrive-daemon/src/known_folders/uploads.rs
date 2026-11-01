use super::activity::ActivityEntry;
use super::scan::{parent_cloud_path, should_skip_source};
use super::state::{FileSnapshot, KnownFolderState, KnownFolderUploadJob};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use twodrive_backend::CloudBackend;
use twodrive_core::{AppPaths, Config, Database, join_cloud_path, normalize_cloud_path};

pub(super) fn retry_known_folder_uploads<B: CloudBackend>(
    paths: &AppPaths,
    backend: &B,
    db: &Database,
    config: &Config,
    state: &mut KnownFolderState,
) -> anyhow::Result<()> {
    if state.pending.is_empty() {
        state.rate_deferred = false;
        return Ok(());
    }
    // Apply current exclusions to persisted work as well as new scans/events.
    // This only retires local bookkeeping; source and cloud bytes are untouched.
    let before = (state.files.len(), state.pending.len());
    state
        .pending
        .retain(|local, _| !should_skip_source(std::path::Path::new(local), config));
    state
        .files
        .retain(|local, _| !should_skip_source(std::path::Path::new(local), config));
    state.prune_missing_pending();
    if before != (state.files.len(), state.pending.len()) {
        state.save(&paths.data_dir)?;
    }
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
    state.rate_deferred = false;
    process_known_folder_uploads(paths, backend, db, config, jobs, state)
}

pub(super) fn process_known_folder_uploads<B: CloudBackend>(
    paths: &AppPaths,
    backend: &B,
    db: &Database,
    config: &Config,
    mut jobs: Vec<KnownFolderUploadJob>,
    state: &mut KnownFolderState,
) -> anyhow::Result<()> {
    jobs.retain(|job| !should_skip_source(&job.local_path, config));
    if jobs.is_empty() {
        return Ok(());
    }
    state.queue_uploads(&paths.data_dir, &jobs)?;
    let rate_window = Duration::from_secs(config.known_folder_upload_rate_seconds()?);
    let rate = Arc::clone(&state.rate);
    let deferred = std::sync::atomic::AtomicUsize::new(0);

    let jobs = jobs
        .into_iter()
        .map(|job| {
            let changed = state
                .files
                .get(job.local_path.to_string_lossy().as_ref())
                .is_some_and(|previous| previous != &job.snapshot);
            (job, changed)
        })
        .collect::<Vec<_>>();
    let jobs = Arc::new(Mutex::new(jobs.into_iter()));
    let (result_tx, result_rx) = mpsc::channel();
    let concurrency = config.power.ac_upload_concurrency.max(1) as usize;
    thread::scope(|scope| -> anyhow::Result<()> {
        for _ in 0..concurrency {
            let jobs = Arc::clone(&jobs);
            let result_tx = result_tx.clone();
            let rate = Arc::clone(&rate);
            let deferred = &deferred;
            scope.spawn(move || {
                loop {
                    let job = match jobs.lock() {
                        Ok(mut jobs) => jobs.next(),
                        Err(_) => return,
                    };
                    let Some((job, changed)) = job else {
                        return;
                    };
                    let allowed = rate
                        .lock()
                        .map(|mut rate| {
                            rate.acquire(&job, changed, config, rate_window, Instant::now())
                        })
                        .unwrap_or(false);
                    if !allowed {
                        deferred.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        continue;
                    }
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
                    state.complete_upload(local_key, job.snapshot)?;
                }
                Err(err) => {
                    eprintln!(
                        "twodrive: known folder upload remains queued for {}: {err:#}",
                        job.local_path.display()
                    );
                }
            }
        }
        Ok(())
    })?;
    let deferred = deferred.load(std::sync::atomic::Ordering::Relaxed);
    if deferred > 0 {
        state.rate_deferred = true;
        eprintln!(
            "twodrive: known folder rate limit deferred {deferred} uploads; latest source contents remain queued"
        );
    }
    if state.pending.is_empty() {
        state.rate_deferred = false;
    }
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
        if let Some(record) = db.get_by_path(&current)? {
            anyhow::ensure!(record.metadata.is_dir, "remote parent is a file");
            continue;
        }
        if let Some(entry) = backend.get_metadata_by_path(&current)? {
            anyhow::ensure!(entry.is_dir, "remote parent is a file");
            db.upsert_metadata(&entry)?;
            continue;
        }
        match backend.create_folder(&current) {
            Ok(entry) => db.upsert_metadata(&entry)?,
            Err(err) => {
                // A concurrent creator can win after our lookup. Resolve the
                // actual object and remember its identity; never swallow errors.
                let entry = backend.get_metadata_by_path(&current)?.ok_or(err)?;
                anyhow::ensure!(entry.is_dir, "remote parent is a file");
                db.upsert_metadata(&entry)?;
            }
        }
    }
    Ok(())
}
