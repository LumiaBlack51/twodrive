use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;
use twodrive_backend::CloudBackend;
use twodrive_core::{Database, FileRecord, FileState, normalize_cloud_path};

use crate::activity::ActivityGuard;
use crate::cache_io::sanitize_cache_name;
use crate::locks::item_sync_lock;

pub fn hydrate_pending_pins<B: CloudBackend>(
    db: &Database,
    cache_dir: &Path,
    backend: &B,
) -> anyhow::Result<usize> {
    let records = db
        .all_records()?
        .into_iter()
        .filter(|record| {
            !record.metadata.is_dir
                && record.effective_pinned()
                && !record.cache_path.as_deref().is_some_and(Path::exists)
        })
        .collect::<Vec<_>>();
    let mut hydrated = 0;
    for record in records {
        match hydrate_record(db, cache_dir, backend, &record) {
            Ok(_) => hydrated += 1,
            Err(err) => {
                eprintln!(
                    "twodrive: pinned hydration remains queued for {}: {err:#}",
                    record.metadata.path
                );
            }
        }
    }
    Ok(hydrated)
}

pub fn hydrate_record<B: CloudBackend>(
    db: &Database,
    cache_dir: &Path,
    backend: &B,
    record: &FileRecord,
) -> anyhow::Result<PathBuf> {
    let generation = db.download_generation(&record.metadata.remote_id)?;
    hydrate_generation(db, cache_dir, backend, record, generation)
}

pub(crate) fn hydrate_generation<B: CloudBackend>(
    db: &Database,
    cache_dir: &Path,
    backend: &B,
    record: &FileRecord,
    generation: i64,
) -> anyhow::Result<PathBuf> {
    if record.metadata.is_dir {
        anyhow::bail!("cannot hydrate a directory");
    }
    let lock = item_sync_lock(&format!("hydrate:{}", record.metadata.remote_id));
    let _guard = lock
        .lock()
        .map_err(|_| anyhow::anyhow!("hydration lock poisoned"))?;
    check_download_generation(db, &record.metadata.remote_id, generation)?;
    let refreshed = db.get_by_remote_id(&record.metadata.remote_id)?;
    let record = refreshed.as_ref().unwrap_or(record);

    if matches!(
        record.state,
        FileState::Cached
            | FileState::Pinned
            | FileState::Writing
            | FileState::Dirty
            | FileState::Uploading
    ) && let Some(cache_path) = &record.cache_path
        && cache_path.exists()
    {
        db.mark_cache_accessed(&record.metadata.remote_id)?;
        return Ok(cache_path.clone());
    }

    if !db.begin_hydration(&record.metadata.remote_id, generation)? {
        return Err(io::Error::from_raw_os_error(libc::ECANCELED).into());
    }
    fs::create_dir_all(cache_dir)?;

    let mut activity = ActivityGuard::start(
        cache_dir,
        "download",
        &record.metadata.path,
        &record.metadata.name,
        Some(record.metadata.size),
    );
    let cache_path = cache_dir.join(sanitize_cache_name(&record.metadata.remote_id));
    let tmp_path = cache_path.with_extension("tmp");
    let mut tmp_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tmp_path)?;
    let cloud_remote_id = record
        .cloud_remote_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("local-only file has no cloud content to hydrate"))?;
    let mut last_check = std::time::Instant::now();
    let result = backend.download_sized_to(
        cloud_remote_id,
        record.metadata.size,
        &mut tmp_file,
        &mut |bytes_done| {
            if last_check.elapsed() >= Duration::from_millis(100) {
                check_download_generation(db, &record.metadata.remote_id, generation)?;
                last_check = std::time::Instant::now();
            }
            activity.set_progress(bytes_done, Some(record.metadata.size));
            Ok(())
        },
    );
    drop(tmp_file);
    let result = check_download_generation(db, &record.metadata.remote_id, generation).and(result);
    let result = result.and_then(|_| {
        if db.finish_hydration(
            &record.metadata.remote_id,
            generation,
            &tmp_path,
            &cache_path,
        )? {
            Ok(cache_path)
        } else {
            Err(io::Error::from_raw_os_error(libc::ECANCELED).into())
        }
    });
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
        let _ = db.fail_hydration(&record.metadata.remote_id, generation);
    }
    result
}

pub(crate) fn check_download_generation(
    db: &Database,
    id: &str,
    generation: i64,
) -> anyhow::Result<()> {
    if db.download_generation(id)? != generation {
        return Err(io::Error::from_raw_os_error(libc::ECANCELED).into());
    }
    Ok(())
}

pub fn pin_path<B: CloudBackend>(
    db: &Database,
    cache_dir: &Path,
    backend: &B,
    path: &str,
) -> anyhow::Result<usize> {
    let Some(record) = db.get_by_path(path)? else {
        anyhow::bail!(
            "path is not in twodrive metadata: {}",
            normalize_cloud_path(path)
        );
    };

    db.set_explicit_pin(&record.metadata.remote_id, true)?;
    if !record.metadata.is_dir {
        let updated = db
            .get_by_remote_id(&record.metadata.remote_id)?
            .unwrap_or(record);
        hydrate_record(db, cache_dir, backend, &updated)?;
        return Ok(1);
    }

    let mut count = 0;
    for child in db.list_descendants(&record.metadata.path)? {
        if !child.metadata.is_dir && child.effective_pinned() {
            let updated = db
                .get_by_remote_id(&child.metadata.remote_id)?
                .unwrap_or(child);
            hydrate_record(db, cache_dir, backend, &updated)?;
            count += 1;
        }
    }
    Ok(count)
}

pub fn unpin_path(db: &Database, path: &str) -> anyhow::Result<usize> {
    let Some(record) = db.get_by_path(path)? else {
        anyhow::bail!(
            "path is not in twodrive metadata: {}",
            normalize_cloud_path(path)
        );
    };

    db.set_explicit_pin(&record.metadata.remote_id, false)?;
    if !record.metadata.is_dir {
        return Ok(1);
    }

    Ok(db.list_descendants(&record.metadata.path)?.len())
}
