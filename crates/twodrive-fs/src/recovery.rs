use std::collections::HashSet;
use std::fs::{self};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::{self};
use twodrive_backend::CloudBackend;
use twodrive_core::{
    Database, FileRecord, FileState, PendingMetadataKind, PendingMetadataOperation,
};

use crate::cache_io::has_existing_cache;
use crate::conflicts::{preserve_conflict_copy, record_upload_etag, record_upload_remote_id};
use crate::upload_queue::{UploadCommand, UploadQueue};

pub fn recover_dirty_uploads<B: CloudBackend>(db: &Database, backend: &B) -> anyhow::Result<usize> {
    recover_dirty_uploads_concurrent(db, backend, 1)
}

pub fn recover_dirty_uploads_concurrent<B: CloudBackend>(
    db: &Database,
    backend: &B,
    concurrency: usize,
) -> anyhow::Result<usize> {
    let records = db
        .all_records()?
        .into_iter()
        .filter(|record| {
            matches!(
                record.state,
                FileState::Dirty | FileState::Uploading | FileState::Conflict
            ) && record.cache_path.as_deref().is_some_and(Path::exists)
        })
        .collect::<Vec<_>>();
    if records.is_empty() {
        return Ok(0);
    }
    let jobs = Mutex::new(records.into_iter());
    let recovered = AtomicUsize::new(0);
    let first_error = Mutex::new(None);
    thread::scope(|scope| {
        for _ in 0..concurrency.max(1) {
            scope.spawn(|| {
                loop {
                    let record = match jobs.lock() {
                        Ok(mut jobs) => jobs.next(),
                        Err(_) => {
                            if let Ok(mut error) = first_error.lock() {
                                error.get_or_insert_with(|| {
                                    anyhow::anyhow!("upload job lock is poisoned")
                                });
                            }
                            return;
                        }
                    };
                    let Some(record) = record else {
                        return;
                    };
                    match recover_dirty_record(db, backend, record, &mut |_, _| Ok(())) {
                        Ok(true) => {
                            recovered.fetch_add(1, Ordering::Relaxed);
                        }
                        Ok(false) => {}
                        Err(err) => {
                            if let Ok(mut error) = first_error.lock() {
                                error.get_or_insert(err);
                            }
                        }
                    }
                }
            });
        }
    });
    if let Some(err) = first_error
        .into_inner()
        .map_err(|_| anyhow::anyhow!("upload error lock is poisoned"))?
    {
        return Err(err);
    }
    Ok(recovered.load(Ordering::Relaxed))
}

pub(crate) fn recover_dirty_record<B: CloudBackend>(
    db: &Database,
    backend: &B,
    record: FileRecord,
    on_progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
) -> anyhow::Result<bool> {
    if db.has_pending_metadata_at_or_above(&record.metadata.path)? {
        return Ok(false);
    }
    let Some(cache_path) = record.cache_path.clone().filter(|path| path.exists()) else {
        return Ok(false);
    };
    if record.state == FileState::Conflict {
        return match preserve_conflict_copy(db, backend, &record, &cache_path, on_progress) {
            Ok(_) => Ok(true),
            Err(err) => {
                eprintln!(
                    "twodrive: conflict copy remains queued for {}: {err:#}",
                    record.metadata.path
                );
                Ok(false)
            }
        };
    }

    // A backup-style save moves the old local record before its cloud move
    // finishes. Uploading by path now would reuse that old cloud identity;
    // deleting the backup would subsequently delete the newly saved file.
    if record.cloud_remote_id.is_none()
        && let Some(remote) = backend.get_metadata_by_path(&record.metadata.path)?
    {
        let owned_by_another_record = db
            .get_by_cloud_remote_id(&remote.remote_id)?
            .is_some_and(|owner| owner.metadata.remote_id != record.metadata.remote_id);
        let awaiting_delete = db
            .pending_deletes()?
            .iter()
            .any(|delete| delete.remote_id == remote.remote_id);
        if owned_by_another_record || awaiting_delete {
            return Ok(false);
        }
    }

    if !db.begin_upload(&record)? {
        return Ok(false);
    }
    match backend.upload_file_with_version(
        &record.metadata.path,
        &cache_path,
        record_upload_remote_id(&record),
        record_upload_etag(&record).as_deref(),
        on_progress,
    ) {
        Ok(uploaded) => {
            let _commit_guard = upload_commit_lock()
                .lock()
                .map_err(|_| anyhow::anyhow!("upload commit lock is poisoned"))?;
            let committed = match db.commit_uploaded(
                &record.metadata.remote_id,
                &record.metadata.path,
                &uploaded,
                &cache_path,
            ) {
                Ok(committed) => committed,
                Err(err) => {
                    db.queue_remote_delete(&uploaded.remote_id, &record.metadata.path)?;
                    return Err(err);
                }
            };
            if committed.cloud_remote_id.as_deref() != Some(uploaded.remote_id.as_str()) {
                db.queue_remote_delete(&uploaded.remote_id, &record.metadata.path)?;
            }
            if let Err(err) = db.finish_pending_releases() {
                eprintln!("twodrive: deferred release remains queued: {err:#}");
            }
            Ok(true)
        }
        Err(err) => {
            let _commit_guard = upload_commit_lock()
                .lock()
                .map_err(|_| anyhow::anyhow!("upload commit lock is poisoned"))?;
            if backend.is_conflict_error(&err) {
                db.mark_state(&record.metadata.remote_id, FileState::Conflict)?;
                let conflict_record = db
                    .get_by_remote_id(&record.metadata.remote_id)?
                    .unwrap_or(record.clone());
                match preserve_conflict_copy(
                    db,
                    backend,
                    &conflict_record,
                    &cache_path,
                    on_progress,
                ) {
                    Ok(_) => return Ok(true),
                    Err(conflict_err) => {
                        eprintln!(
                            "twodrive: conflict copy remains queued for {}: {conflict_err:#}",
                            record.metadata.path
                        );
                    }
                }
            } else {
                let state = if err.to_string().contains("unsupported OneDrive file name") {
                    FileState::Error
                } else {
                    FileState::Dirty
                };
                db.mark_state(&record.metadata.remote_id, state)?;
            }
            eprintln!(
                "twodrive: dirty upload remains queued for {}: {err:#}",
                record.metadata.path
            );
            Ok(false)
        }
    }
}

pub(crate) fn upload_commit_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub fn recover_pending_deletes<B: CloudBackend>(
    db: &Database,
    backend: &B,
) -> anyhow::Result<usize> {
    let pending = db.pending_deletes()?;
    let mut recovered = 0;
    for delete in pending {
        if delete.remote_id.starts_with("local-upload-") {
            if let Some(cache_path) = &delete.cache_path {
                match fs::remove_file(cache_path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err.into()),
                }
            }
            db.remove_pending_delete(&delete.remote_id)?;
            recovered += 1;
            continue;
        }
        match backend.delete(&delete.remote_id) {
            Ok(()) => {
                if let Some(cache_path) = &delete.cache_path {
                    match fs::remove_file(cache_path) {
                        Ok(()) => {}
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                        Err(err) => return Err(err.into()),
                    }
                }
                db.remove_pending_delete(&delete.remote_id)?;
                recovered += 1;
            }
            Err(err) => {
                eprintln!(
                    "twodrive: pending delete remains queued for {}: {err:#}",
                    delete.path
                );
            }
        }
    }
    Ok(recovered)
}

pub(crate) fn recover_pending_delete_id<B: CloudBackend>(
    db: &Database,
    backend: &B,
    cloud_remote_id: &str,
) -> anyhow::Result<bool> {
    let Some(delete) = db
        .pending_deletes()?
        .into_iter()
        .find(|delete| delete.remote_id == cloud_remote_id)
    else {
        return Ok(false);
    };
    if !delete.remote_id.starts_with("local-upload-") {
        backend.delete(&delete.remote_id)?;
    }
    if let Some(cache_path) = &delete.cache_path {
        match fs::remove_file(cache_path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }
    db.remove_pending_delete(&delete.remote_id)?;
    Ok(true)
}

pub fn recover_pending_metadata_operations<B: CloudBackend>(
    db: &Database,
    backend: &B,
) -> anyhow::Result<usize> {
    let operations = db.pending_metadata_operations()?;
    let mut recovered = 0;
    for operation in operations {
        match recover_pending_metadata_record(db, backend, &operation) {
            Ok(true) => recovered += 1,
            Ok(false) => {}
            Err(err) => eprintln!(
                "twodrive: pending metadata operation remains queued for {}: {err:#}",
                operation.path
            ),
        }
    }
    Ok(recovered)
}

pub(crate) fn recover_pending_metadata_record<B: CloudBackend>(
    db: &Database,
    backend: &B,
    operation: &PendingMetadataOperation,
) -> anyhow::Result<bool> {
    let Some(record) = db.get_by_remote_id(&operation.local_id)? else {
        return Ok(false);
    };
    // A queued snapshot may predate a local parent rename. Never replay its
    // old path, or let a child create a destination before the parent moves.
    if record.metadata.path != operation.path
        || db.has_pending_metadata_at_or_above(&record.metadata.parent_path)?
    {
        return Ok(false);
    }
    let entry = match operation.kind {
        PendingMetadataKind::CreateFolder => match backend.create_folder(&operation.path) {
            Ok(entry) => entry,
            Err(create_err) => backend
                .get_metadata_by_path(&operation.path)?
                .filter(|entry| entry.is_dir)
                .ok_or(create_err)?,
        },
        PendingMetadataKind::Move => {
            let cloud_remote_id = record
                .cloud_remote_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("move has no cloud identity"))?;
            backend.rename(cloud_remote_id, &operation.path)?
        }
    };
    db.complete_metadata_operation(&operation.local_id, &operation.path, &entry)
}

pub(crate) fn recover_interrupted_writes(db: &Database) -> anyhow::Result<()> {
    // Only at startup: there are no live writers yet. Keep every byte left in cache.
    for record in db.all_records()? {
        if record.state == FileState::Writing
            && let Some(path) = record.cache_path
            && let Ok(metadata) = fs::metadata(path)
        {
            db.mark_dirty_with_size(&record.metadata.remote_id, metadata.len())?;
        }
    }
    Ok(())
}

pub(crate) fn enqueue_recovery(db: &Database, sender: &UploadQueue) -> anyhow::Result<()> {
    for delete in db.pending_deletes()? {
        sender.send(UploadCommand::Delete(delete.remote_id))?;
    }
    let mut queued = HashSet::new();
    for operation in db.pending_metadata_operations()? {
        queued.insert(operation.local_id.clone());
        sender.send(UploadCommand::Upload(operation.local_id))?;
    }
    for record in db.all_records()? {
        if (matches!(
            record.state,
            FileState::Dirty | FileState::Uploading | FileState::Conflict
        ) || (!record.metadata.is_dir
            && record.effective_pinned()
            && !has_existing_cache(&record)))
            && queued.insert(record.metadata.remote_id.clone())
        {
            sender.send(UploadCommand::Upload(record.metadata.remote_id))?;
        }
    }
    Ok(())
}
