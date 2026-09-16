use twodrive_backend::CloudBackend;
use twodrive_core::{Database, FileState};

pub fn sync_metadata<B: CloudBackend>(db: &Database, backend: &B) -> anyhow::Result<usize> {
    let entries = backend.list_all()?;
    let count = entries.len();
    db.upsert_metadata_batch(&entries)?;
    Ok(count)
}

pub fn sync_delta_metadata<B: CloudBackend>(db: &Database, backend: &B) -> anyhow::Result<usize> {
    let delta = backend.list_delta(db.delta_link()?.as_deref())?;
    for remote_id in &delta.deleted_remote_ids {
        db.remove_pending_delete(remote_id)?;
        let record = db.get_by_cloud_remote_id(remote_id)?;
        if record.as_ref().is_some_and(|record| {
            matches!(
                record.state,
                FileState::Writing | FileState::Dirty | FileState::Uploading
            ) || db
                .pending_metadata_operation(&record.metadata.remote_id)
                .ok()
                .flatten()
                .is_some()
        }) {
            continue;
        }
        if let Some(record) = record {
            db.remove_by_remote_id(&record.metadata.remote_id)?;
        }
    }

    let count = delta.entries.len();
    db.upsert_metadata_batch(&delta.entries)?;
    if let Some(delta_link) = delta.delta_link {
        db.set_delta_link(&delta_link)?;
    }
    Ok(count)
}
