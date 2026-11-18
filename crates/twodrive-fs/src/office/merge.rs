//! Fold the live map back into a flat cache without allocating another whole file.
//! Materialize one window at a time. Before overwriting source bytes, redirect
//! their remaining references into durable literals. Either side of a crash
//! therefore still describes exactly the same logical file.
use super::*;
use crate::snapshot::CacheGate;
use std::os::unix::fs::MetadataExt;
use std::time::{Duration, Instant};

fn try_exclusive(f: &File) -> Result<bool> {
    match f.try_lock() {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

pub(super) fn advance(path: &Path) -> Result<bool> {
    let _gate = CacheGate::acquire(path)?;
    let d = dir(path);
    if !d.exists() {
        return Ok(false);
    }
    let carrier = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let Some(mut o) = Office::open(path)? else {
        return Ok(false);
    };
    if o.is_frozen()? {
        return Ok(false);
    }
    let base = OpenOptions::new()
        .read(true)
        .write(true)
        .open(d.join("base"))?;
    let info = base.metadata()?;
    let same = info.ino() == carrier.metadata()?.ino() && info.dev() == carrier.metadata()?.dev();
    let next = d.join("adopting");
    if next.try_exists()? {
        fs::remove_file(&next)?;
    }
    if !same {
        if base.metadata()?.nlink() != 1 || !try_exclusive(&carrier)? || !try_exclusive(&base)? {
            return Ok(false);
        }
        // Only this map owns the abandoned original. Move its inode back under
        // the public cache name. The durable map is authoritative on both sides.
        fs::hard_link(d.join("base"), &next)?;
        fs::rename(next, path)?;
        File::open(path.parent().unwrap())?.sync_all()?;
        return Ok(true);
    }
    if info.nlink() != 2 {
        return Ok(false);
    }
    let size = o.size(LIVE)?;
    let progress: u64 =
        o.db.query_row("SELECT pos FROM merge_state", [], |r| r.get(0))?;
    if progress >= size {
        // Existing handles may keep a database connection. Leave the identity
        // map in place until those handles close; its bytes are already folded.
        if !try_exclusive(&carrier)? {
            return Ok(false);
        }
        carrier.set_len(size)?;
        carrier.sync_all()?;
        drop(o);
        let gc = d.with_file_name(format!(
            ".done-{}",
            path.file_name().unwrap().to_string_lossy()
        ));
        discard_staging(&gc)?;
        fs::rename(&d, &gc)?;
        File::open(gc.parent().unwrap())?.sync_all()?;
        fs::remove_dir_all(gc)?;
        return Ok(false);
    }
    let matching:Option<u64>=o.db.query_row(
        "SELECT end FROM ranges WHERE view=0 AND start=(SELECT max(start) FROM ranges WHERE view=0 AND start<=?1)
        AND end>?1 AND literal IS NULL AND src=start",[progress],|r|r.get(0)).optional()?;
    if let Some(end) = matching {
        o.db.execute("UPDATE merge_state SET pos=?1", [end])?;
        return Ok(true);
    }
    let n = (size - progress).min(LITERAL as u64) as usize;
    let end = progress + n as u64;
    let mut output = vec![0; n];
    o.read(LIVE, progress, &mut output)?;
    // Bound the SQL result and the amount of retained data per transaction.
    // Redirect one overlap per iteration if the same source bytes occur many times.
    let dependency: Option<(u64, u64, u64)> =
        o.db.query_row(
            "SELECT start,end,src FROM ranges WHERE view=0 AND literal IS NULL AND src<?2
        AND src+(end-start)>?1 LIMIT 1",
            params![progress, end],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((start, stop, src)) = dependency {
        let from = src.max(progress);
        let count = (src + (stop - start)).min(end) - from;
        let mut old = vec![0; count as usize];
        o.base.read_exact_at(&mut old, from)?;
        let at = start + (from - src);
        let tx = o.db.transaction()?;
        discard(&tx, at, at + count)?;
        literal(&tx, at, &old)?;
        tx.commit()?;
        return Ok(true);
    }
    // Persist every redirected source reference before changing its raw bytes.
    o.sync()?;
    carrier.write_all_at(&output, progress)?;
    carrier.sync_data()?;
    let tx = o.db.transaction()?;
    discard(&tx, progress, end)?;
    reference(&tx, progress, n as u64, progress)?;
    tx.execute("UPDATE merge_state SET pos=?1", [end])?;
    tx.commit()?;
    Ok(true)
}

pub(crate) fn maintain(cache: &Path, db: &twodrive_core::Database) -> Result<()> {
    let root = cache.join(ROOT);
    let items = match root.read_dir() {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for it in items {
        let it = it?;
        let name = it.file_name();
        if let Some(owner) = name.to_str().and_then(|name| {
            name.strip_prefix(".init-")
                .or_else(|| name.strip_prefix(".done-"))
        }) && !owner.is_empty()
            && !owner.starts_with('.')
        {
            let _gate = CacheGate::acquire(&cache.join(owner))?;
            discard_staging(&it.path())?;
            continue;
        }
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let path = cache.join(name);
        if !path.try_exists()? {
            let _gate = CacheGate::acquire(&path)?;
            if !path.try_exists()? && !db.has_cache_reference(&path)? && recognized(&it.path())? {
                fs::remove_dir_all(it.path())?;
            }
            continue;
        }
        let until = Instant::now() + Duration::from_secs(2);
        while advance(&path)? && Instant::now() < until {}
    }
    Ok(())
}

fn recognized(d: &Path) -> Result<bool> {
    if !d.is_dir() || !d.join("map").is_file() || !d.join("base").is_file() {
        return Ok(false);
    }
    for child in d.read_dir()? {
        let c = child?;
        if !c.file_type()?.is_file()
            || !matches!(
                c.file_name().to_str(),
                Some("base" | "map" | "map-wal" | "map-shm" | "adopting" | "map-journal")
            )
        {
            return Ok(false);
        }
    }
    let db =
        Connection::open_with_flags(d.join("map"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    Ok(db.query_row("PRAGMA application_id", [], |r| r.get::<_, u32>(0))? == 1413762895)
}

pub(super) fn discard_staging(dir: &Path) -> Result<()> {
    if !dir.try_exists()? {
        return Ok(());
    }
    // These names are unpublished or already folded. Preserve an unrecognized
    // entry or the sole surviving copy of a base for manual recovery.
    for entry in dir.read_dir()? {
        let entry = entry?;
        ensure!(
            entry.file_type()?.is_file()
                && matches!(
                    entry.file_name().to_str(),
                    Some("base" | "map" | "map-wal" | "map-shm" | "map-journal")
                ),
            "unrecognized Office recovery material"
        );
        if entry.file_name() == "base" {
            ensure!(
                entry.metadata()?.nlink() > 1,
                "retaining unique Office recovery base"
            );
        }
    }
    fs::remove_dir_all(dir)?;
    Ok(())
}
