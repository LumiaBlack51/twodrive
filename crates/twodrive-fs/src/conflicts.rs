use std::path::Path;
use std::time::UNIX_EPOCH;
use twodrive_backend::CloudBackend;
use twodrive_core::{Database, FileRecord, MetadataEntry, join_cloud_path};

pub(crate) fn record_upload_etag(record: &FileRecord) -> Option<String> {
    if record.cloud_remote_id.is_none() || record.metadata.etag.is_empty() {
        None
    } else {
        Some(record.metadata.etag.clone())
    }
}

pub(crate) fn record_upload_remote_id(record: &FileRecord) -> Option<&str> {
    record.cloud_remote_id.as_deref()
}

pub(crate) fn preserve_conflict_copy<B: CloudBackend>(
    db: &Database,
    backend: &B,
    original: &FileRecord,
    cache_path: &Path,
    on_progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
) -> anyhow::Result<MetadataEntry> {
    let conflict_path = conflict_copy_path(original, cache_path);
    let conflict =
        backend.upload_file_with_version(&conflict_path, cache_path, None, None, on_progress)?;
    let cloud_remote_id = original
        .cloud_remote_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("local-only item cannot have an etag conflict"))?;
    let latest_original = backend.get_metadata(cloud_remote_id)?.ok_or_else(|| {
        anyhow::anyhow!(
            "original remote item disappeared while preserving conflict {}",
            original.metadata.path
        )
    })?;
    db.upsert_metadata(&conflict)?;
    db.mark_cached(&conflict.remote_id, cache_path)?;
    db.upsert_metadata(&latest_original)?;
    db.mark_online_only(&original.metadata.remote_id)?;
    Ok(conflict)
}

pub(crate) fn conflict_copy_path(record: &FileRecord, cache_path: &Path) -> String {
    let modified = cache_path
        .metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(record.metadata.modified_unix.max(0) as u64);
    let fingerprint = stable_path_fingerprint(&record.metadata.remote_id);
    let suffix = format!(" (TwoDrive conflict {modified}-{fingerprint:08x})");
    let name = &record.metadata.name;
    let extension_start = name.rfind('.').filter(|index| *index > 0);
    let (stem, extension) = extension_start
        .map(|index| (&name[..index], &name[index..]))
        .unwrap_or((name.as_str(), ""));
    let extension = truncate_utf8(extension, 32);
    let max_stem_bytes = 240_usize
        .saturating_sub(suffix.len())
        .saturating_sub(extension.len());
    let stem = truncate_utf8(stem, max_stem_bytes);
    join_cloud_path(
        &record.metadata.parent_path,
        &format!("{stem}{suffix}{extension}"),
    )
}

pub(crate) fn stable_path_fingerprint(value: &str) -> u32 {
    value.as_bytes().iter().fold(0x811c9dc5_u32, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(0x01000193)
    })
}

pub(crate) fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}
