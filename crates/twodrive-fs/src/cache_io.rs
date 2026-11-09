use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use twodrive_core::{Database, FileRecord, FileState};

pub(crate) fn read_slice(path: &Path, offset: u64, size: u32) -> anyhow::Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut data = vec![0; size as usize];
    let read = file.read(&mut data)?;
    data.truncate(read);
    Ok(data)
}

pub(crate) fn write_slice(path: &Path, offset: u64, data: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(data)?;
    Ok(())
}

pub(crate) fn has_existing_cache(record: &FileRecord) -> bool {
    matches!(
        record.state,
        FileState::Cached
            | FileState::Pinned
            | FileState::Writing
            | FileState::Dirty
            | FileState::Uploading
    ) && record.cache_path.as_deref().is_some_and(|path| {
        fs::metadata(path).is_ok_and(|metadata| {
            !matches!(record.state, FileState::Cached | FileState::Pinned)
                || metadata.len() == record.metadata.size
        })
    })
}

pub(crate) fn reconcile_cached_record(
    db: &Database,
    record: &FileRecord,
) -> anyhow::Result<FileRecord> {
    if matches!(record.state, FileState::Cached | FileState::Pinned)
        && let Some(path) = &record.cache_path
        && fs::metadata(path).is_ok_and(|metadata| metadata.len() != record.metadata.size)
    {
        db.invalidate_cached_snapshot(record)?;
        return db
            .get_by_remote_id(&record.metadata.remote_id)?
            .ok_or_else(|| anyhow::anyhow!("cache record disappeared"));
    }
    Ok(record.clone())
}

pub(crate) fn download_cache_path(
    cache_dir: &Path,
    remote_id: &str,
    generation: i64,
) -> std::path::PathBuf {
    cache_dir.join(format!(
        "{}-download-{generation}",
        sanitize_cache_name(remote_id)
    ))
}

pub(crate) fn sanitize_cache_name(remote_id: &str) -> String {
    remote_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(unix)]
pub(crate) fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

#[cfg(unix)]
pub(crate) fn current_gid() -> u32 {
    unsafe { libc::getgid() }
}

pub(crate) fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{nanos}-{}", std::process::id())
}
