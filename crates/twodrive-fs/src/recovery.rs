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

use crate::activity::ActivityGuard;
use crate::cache_io::has_existing_cache;
use crate::conflicts::{preserve_conflict_copy, record_upload_etag, record_upload_remote_id};
use crate::locks::item_sync_lock;
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
            !db.excludes_upload(record)
                && matches!(
                    record.state,
                    FileState::Dirty | FileState::Uploading | FileState::Conflict
                )
                && record.cache_path.as_deref().is_some_and(Path::exists)
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

pub fn recover_dirty_record<B: CloudBackend>(
    db: &Database,
    backend: &B,
    mut record: FileRecord,
    on_progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
) -> anyhow::Result<bool> {
    let lock = item_sync_lock(&record.metadata.remote_id);
    let _guard = lock
        .lock()
        .map_err(|_| anyhow::anyhow!("item sync lock is poisoned"))?;
    let Some(current) = db.get_by_remote_id(&record.metadata.remote_id)? else {
        return Ok(false);
    };
    if db.excludes_upload(&current) || db.excludes_upload(&record) {
        return Ok(false);
    }
    if db.is_pending_delete(&record.metadata.remote_id, &record.metadata.path)? {
        return Ok(false);
    }
    if db.has_pending_metadata_at_or_above(&record.metadata.path)? {
        return Ok(false);
    }
    let Some(cache_path) = record.cache_path.clone().filter(|path| path.exists()) else {
        return Ok(false);
    };
    let cache_guard = match fs::File::open(&cache_path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err.into()),
    };
    cache_guard.lock_shared()?;
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
            .any(|delete| delete.cloud_remote_id.as_deref() == Some(remote.remote_id.as_str()));
        if awaiting_delete {
            return Ok(false);
        }
        if owned_by_another_record {
            let Some(adopted) = db.adopt_excluded_backup_identity(&record, &remote)? else {
                return Ok(false);
            };
            record = adopted;
        }
    }

    if !db.begin_upload(&record)? {
        return Ok(false);
    }
    let mut activity = ActivityGuard::start(
        cache_path.parent().unwrap_or(Path::new(".")),
        "upload",
        &record.metadata.path,
        &record.metadata.name,
        Some(record.metadata.size),
    );
    let uploaded_result = backend.upload_file_with_version(
        &record.metadata.path,
        &cache_path,
        record_upload_remote_id(&record),
        record_upload_etag(&record).as_deref(),
        &mut |done, total| {
            activity.set_progress(done, Some(total));
            on_progress(done, total)
        },
    );
    drop(activity);
    match uploaded_result {
        Ok(uploaded) => {
            let _commit_guard = upload_commit_lock()
                .lock()
                .map_err(|_| anyhow::anyhow!("upload commit lock is poisoned"))?;
            if db.complete_deleted_upload(&record.metadata.remote_id, &uploaded)? {
                return Ok(true);
            }
            let committed = match db.commit_uploaded(
                &record.metadata.remote_id,
                &record.metadata.path,
                &uploaded,
                &cache_path,
            ) {
                Ok(committed) => committed,
                Err(err) => {
                    if !db.complete_deleted_upload(&record.metadata.remote_id, &uploaded)? {
                        db.queue_remote_delete(&uploaded.remote_id, &record.metadata.path)?;
                    }
                    return Err(err);
                }
            };
            if committed.cloud_remote_id.as_deref() != Some(uploaded.remote_id.as_str()) {
                db.queue_remote_delete(&uploaded.remote_id, &record.metadata.path)?;
            }
            drop(cache_guard);
            if let Err(err) = db.finish_pending_releases() {
                eprintln!("twodrive: deferred release remains queued: {err:#}");
            }
            Ok(true)
        }
        Err(err) => {
            if db
                .pending_delete_for_local_id(&record.metadata.remote_id)?
                .is_some()
            {
                return Ok(false);
            }
            // A deleted/replaced cloud identity cannot be fixed by retrying its
            // ID forever, or by overwriting a new same-name item. Confirm that
            // exact ID is absent, then preserve these edits as a separate copy.
            let missing_target = backend.is_not_found_error(&err)
                && if let Some(id) = record.cloud_remote_id.as_deref() {
                    backend.get_metadata(id)?.is_none()
                } else {
                    false
                };
            if backend.is_conflict_error(&err) || missing_target {
                if !db.mark_upload_conflict(&record)? {
                    return Ok(false);
                }
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
                db.finish_failed_upload(&record, state)?;
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
        match recover_pending_delete_id(db, backend, &delete.remote_id) {
            Ok(true) => recovered += 1,
            Ok(false) => {}
            Err(err) => {
                eprintln!(
                    "twodrive: pending delete remains queued: {}",
                    twodrive_backend::diagnostics::safe_error(&err)
                );
            }
        }
    }
    Ok(recovered)
}

pub(crate) fn recover_pending_delete_id<B: CloudBackend>(
    db: &Database,
    backend: &B,
    delete_id: &str,
) -> anyhow::Result<bool> {
    let Some(delete) = db
        .pending_deletes()?
        .into_iter()
        .find(|delete| delete.remote_id == delete_id)
    else {
        return Ok(false);
    };
    let lock = item_sync_lock(delete.local_id.as_deref().unwrap_or(delete_id));
    let _guard = lock
        .lock()
        .map_err(|_| anyhow::anyhow!("item sync lock is poisoned"))?;
    // An upload may have acknowledged or another worker may have drained this
    // entry while we waited. Its result is authoritative, not the first snapshot.
    let Some(mut delete) = db
        .pending_deletes()?
        .into_iter()
        .find(|delete| delete.remote_id == delete_id)
    else {
        return Ok(false);
    };
    if !delete.retry_due(twodrive_core::now_unix()) {
        return Ok(false);
    }
    // A delayed upload acknowledgement can outlive its local identity. Its
    // cleanup must not delete an ID now owned by another live local record.
    // Recheck persisted old cleanup tasks as well as guarding new insertions.
    if let Some(id) = delete.cloud_remote_id.as_deref()
        && db.get_by_cloud_remote_id(id)?.is_some()
    {
        if delete.local_id.is_none() {
            db.remove_pending_delete(&delete.remote_id)?;
            return Ok(true);
        }
        return Ok(false);
    }
    let result = (|| -> anyhow::Result<bool> {
        // File locks also protect a transfer/handle owned by another process. Claim
        // all retained cache files before remote deletion or unknown-upload replay.
        let mut cache_guards = Vec::new();
        let mut paths = HashSet::new();
        for cache_path in delete
            .cache_path
            .iter()
            .chain(delete.upload_cache_path.iter())
        {
            if !paths.insert(cache_path.clone()) {
                continue;
            }
            let file = match fs::File::open(cache_path) {
                Ok(file) => file,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err.into()),
            };
            match file.try_lock() {
                Ok(()) => cache_guards.push((cache_path.clone(), file)),
                Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
                Err(err) => return Err(err.into()),
            }
        }
        if let Some(upload_path) = &delete.upload_path
            && delete.upload_target_id.is_none()
        {
            // After a lost acknowledgement/restart an unbound upload may already
            // exist remotely. Finish/resume that claimed generation to obtain its ID.
            // Same-path uploads stay blocked until this tombstone is retired.
            let source = delete.upload_cache_path.as_ref().ok_or_else(|| {
                anyhow::anyhow!("interrupted deleted upload has no retained cache")
            })?;
            let uploaded = backend.upload_file_with_version(
                upload_path,
                source,
                None,
                None,
                &mut |_, _| Ok(()),
            )?;
            let local_id = delete.local_id.as_deref().ok_or_else(|| {
                anyhow::anyhow!("interrupted deleted upload has no local identity")
            })?;
            db.complete_deleted_upload(local_id, &uploaded)?;
            delete = db
                .pending_delete_for_local_id(local_id)?
                .ok_or_else(|| anyhow::anyhow!("deleted upload tombstone disappeared"))?;
        }
        if let Some(cloud_remote_id) = &delete.cloud_remote_id {
            backend.delete(cloud_remote_id)?;
        }
        for (cache_path, _guard) in &cache_guards {
            match fs::remove_file(cache_path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        }
        db.remove_pending_delete(&delete.remote_id)?;
        Ok(true)
    })();
    if let Err(error) = &result {
        use twodrive_backend::diagnostics::{RequestFailure, RequestOperation};
        use twodrive_core::DeleteFailureKind;
        let (status, kind, retry_after) = match error.downcast_ref::<RequestFailure>() {
            Some(failure) => (
                Some(failure.status),
                match failure.status {
                    401 => DeleteFailureKind::Authentication,
                    403 if failure.code == "insufficient_claims"
                        || failure.inner_code == "insufficient_claims"
                        || matches!(
                            failure.operation,
                            RequestOperation::TokenRefresh | RequestOperation::TokenExchange
                        ) =>
                    {
                        DeleteFailureKind::Authentication
                    }
                    403 => DeleteFailureKind::Permission,
                    429 => DeleteFailureKind::Throttled,
                    500..=599 => DeleteFailureKind::Transient,
                    _ => DeleteFailureKind::Rejected,
                },
                failure.retry_after.as_secs(),
            ),
            None => (None, DeleteFailureKind::Transient, 0),
        };
        db.defer_pending_delete(delete_id, status, kind, retry_after)?;
    }
    result
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
    if db.excludes_upload(&record) {
        return Ok(false);
    }
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
    // Only at startup: there are no live writers. Keep every cached byte.
    for record in db.all_records()? {
        if record.metadata.is_dir {
            continue;
        }
        let excluded = db.excludes_file_name(&record.metadata.name);
        if excluded {
            // Also migrate legacy owner files that used to retry as normal uploads.
            if let Some(path) = &record.cache_path
                && let Ok(metadata) = fs::metadata(path)
            {
                db.mark_state(&record.metadata.remote_id, FileState::LocalOnly)?;
                db.mark_dirty_with_size(&record.metadata.remote_id, metadata.len())?;
            }
        } else if (record.state == FileState::Writing || record.state == FileState::LocalOnly)
            && let Some(path) = &record.cache_path
            && let Ok(metadata) = fs::metadata(path)
        {
            db.mark_state(&record.metadata.remote_id, FileState::Dirty)?;
            db.mark_dirty_with_size(&record.metadata.remote_id, metadata.len())?;
            if record.cloud_remote_id.is_some() {
                db.move_subtree_and_queue(&record.metadata.remote_id, &record.metadata.path)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn enqueue_recovery(db: &Database, sender: &UploadQueue) -> anyhow::Result<()> {
    for delete in db
        .pending_deletes()?
        .into_iter()
        .filter(|delete| delete.retry_due(twodrive_core::now_unix()))
    {
        sender.send(UploadCommand::Delete(delete.remote_id))?;
    }
    let mut queued = HashSet::new();
    for operation in db.pending_metadata_operations()? {
        if db
            .get_by_remote_id(&operation.local_id)?
            .as_ref()
            .is_some_and(|record| db.excludes_upload(record))
        {
            continue;
        }
        queued.insert(operation.local_id.clone());
        sender.send(UploadCommand::Upload(operation.local_id))?;
    }
    for record in db.all_records()? {
        if !db.excludes_upload(&record)
            && (matches!(
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
