//! Durable byte ranges for edits to an Office package while its upload is reading.
//! A range points into an immutable ZIP or into one bounded literal in SQLite.
//! Gaps read as zero. Both the current map and at most one frozen map share data.
mod checkpoint;
mod merge;
pub(crate) use merge::maintain;
mod package;
use anyhow::{Context, Result, ensure};
use package::{Package, Scan};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{FileExt, PermissionsExt};
use std::path::{Path, PathBuf};

const ROOT: &str = ".twodrive-office";
const LITERAL: usize = 128 * 1024;
const LIVE: i64 = 0;
const CHECKPOINT_BYTES: u32 = 256 * 1024;

// Repeated changed media writes fill the WAL much faster than references.
const LITERAL_CHECKPOINT_BYTES: u32 = 4 * 1024 * 1024;

const SCHEMA:&str="
PRAGMA page_size=1024; PRAGMA application_id=1413762895; PRAGMA auto_vacuum=FULL; PRAGMA journal_mode=WAL;
CREATE TABLE carrier(dev INTEGER NOT NULL,ino INTEGER NOT NULL);
CREATE TABLE merge_state(pos INTEGER NOT NULL); INSERT INTO merge_state VALUES(0);
CREATE TABLE views(id INTEGER PRIMARY KEY,size INTEGER NOT NULL);
CREATE TABLE literals(id INTEGER PRIMARY KEY, data BLOB NOT NULL);
CREATE TABLE ranges(view INTEGER NOT NULL, start INTEGER NOT NULL, end INTEGER NOT NULL,
    literal INTEGER, src INTEGER NOT NULL, PRIMARY KEY(view,start)) WITHOUT ROWID;
CREATE INDEX by_literal ON ranges(literal) WHERE literal IS NOT NULL;
CREATE INDEX by_source ON ranges(view,src) WHERE literal IS NULL;
CREATE TRIGGER release_literal AFTER DELETE ON ranges WHEN OLD.literal IS NOT NULL BEGIN
 DELETE FROM literals WHERE id=OLD.literal AND NOT EXISTS(SELECT 1 FROM ranges WHERE literal=OLD.literal);
END;";
pub(crate) fn extension(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            ["pptx", "docx", "xlsx", "pptm", "docm", "xlsm"]
                .iter()
                .any(|s| e.eq_ignore_ascii_case(s))
        })
}
fn dir(path: &Path) -> PathBuf {
    twodrive_core::office_cache_directory(path).expect("cache file has a parent and name")
}
fn conn(path: &Path) -> Result<Connection> {
    let c = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    // Impress commonly writes 4 KiB at a time. Compact pages let many small
    // range updates commit before a checkpoint. Existing maps keep their page
    // size, so compute the WAL threshold in pages for this connection.
    let page_size: u32 = c.pragma_query_value(None, "page_size", |row| row.get(0))?;
    c.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA cache_size=-1024;")?;
    c.pragma_update(None, "wal_autocheckpoint", CHECKPOINT_BYTES / page_size)?;
    c.pragma_update(None, "journal_size_limit", CHECKPOINT_BYTES)?;
    Ok(c)
}
pub(crate) struct Office {
    checkpoints: checkpoint::Checkpoints,
    db: Connection,
    base: File,
    index: Option<Package>,
    indexed: bool,
    buffer: Vec<u8>,
    scan: Scan,
    literal_bytes: usize,
}
impl Office {
    pub fn open(path: &Path) -> Result<Option<Self>> {
        let d = dir(path);
        if !d.try_exists()? {
            return Ok(None);
        }
        let office = Self::at(&d)?;
        use std::os::unix::fs::MetadataExt;
        let carrier = path.metadata()?;
        let base = office.base.metadata()?;
        if carrier.dev() != base.dev() || carrier.ino() != base.ino() {
            let expected: (u64, u64) =
                office
                    .db
                    .query_row("SELECT dev,ino FROM carrier", [], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })?;
            ensure!(
                expected == (carrier.dev(), carrier.ino()),
                "Office cache identity changed"
            );
        }
        Ok(Some(office))
    }
    fn at(d: &Path) -> Result<Self> {
        Ok(Self {
            checkpoints: Default::default(),
            db: conn(&d.join("map"))?,
            base: File::open(d.join("base"))?,
            index: None,
            indexed: false,
            buffer: Vec::new(),
            scan: Scan::default(),
            literal_bytes: 0,
        })
    }
    // Caller holds the source cache gate. New temp targets are not visible yet.
    pub fn enroll(path: &Path, source: &Path, original: bool) -> Result<Option<Self>> {
        if let Some(active) = Self::open(path)? {
            return Ok(Some(active));
        }
        let base_path = if original {
            source.to_owned()
        } else {
            dir(source).join("base")
        };
        let file = File::open(&base_path)?;
        if file.metadata()?.len() <= 10 * 1024 * 1024 {
            return Ok(None);
        }
        let Some(index) = Package::open(&file)? else {
            return Ok(None);
        };
        let target = dir(path);
        let parent = target.parent().unwrap();
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        File::open(parent.parent().unwrap())?.sync_all()?;
        let init = parent.join(format!(
            ".init-{}",
            path.file_name().unwrap().to_string_lossy()
        ));
        merge::discard_staging(&init)?;
        fs::create_dir(&init)?;
        let result = (|| -> Result<()> {
            fs::hard_link(&base_path, init.join("base"))?;
            file.sync_all()?;
            let c = Connection::open(init.join("map"))?;
            c.execute_batch(SCHEMA)?;
            use std::os::unix::fs::MetadataExt;
            let carrier = path.metadata()?;
            c.execute(
                "INSERT INTO carrier VALUES(?1,?2)",
                params![carrier.dev(), carrier.ino()],
            )?;
            let size = if original { file.metadata()?.len() } else { 0 };
            c.execute("INSERT INTO views VALUES(0,?1)", [size])?;
            if size > 0 {
                c.execute("INSERT INTO ranges VALUES(0,0,?1,NULL,0)", [size])?;
            }
            drop(c);
            File::open(&init)?.sync_all()?;
            fs::rename(&init, &target)?;
            File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if init.exists() {
            let _ = fs::remove_dir_all(&init);
        }
        if let Err(e) = result {
            if e.downcast_ref::<io::Error>()
                .and_then(io::Error::raw_os_error)
                .is_some_and(|n| {
                    [libc::EXDEV, libc::EOPNOTSUPP, libc::EPERM, libc::EMLINK].contains(&n)
                })
                && !target.exists()
            {
                return Ok(None);
            }
            return Err(e);
        }
        let mut storage = Self::at(&target)?;
        storage.index = Some(index);
        storage.indexed = true;
        Ok(Some(storage))
    }
    pub fn size(&self, view: i64) -> Result<u64> {
        Ok(self
            .db
            .query_row("SELECT size FROM views WHERE id=?1", [view], |r| r.get(0))?)
    }
    pub fn freeze(&mut self) -> Result<()> {
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM ranges WHERE view=1", [])?;
        tx.execute(
            "INSERT OR REPLACE INTO views SELECT 1,size FROM views WHERE id=0",
            [],
        )?;
        tx.execute(
            "INSERT INTO ranges SELECT 1,start,end,literal,src FROM ranges WHERE view=0",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn sync(&self) -> Result<()> {
        let path = self
            .db
            .path()
            .and_then(|p| Path::new(p).parent())
            .context("missing Office map directory")?;
        let checkpoint_guard = File::open(path)?;
        checkpoint_guard.lock()?;
        let (busy, log, done): (i64, i64, i64) =
            self.db.query_row("PRAGMA wal_checkpoint(FULL)", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
        ensure!(
            busy == 0 && log == done,
            "incomplete Office save checkpoint"
        );
        Ok(())
    }
    pub fn is_frozen(&self) -> Result<bool> {
        Ok(self
            .db
            .query_row("SELECT EXISTS(SELECT 1 FROM views WHERE id=1)", [], |r| {
                r.get(0)
            })?)
    }
    pub fn thaw(&mut self) -> Result<()> {
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM ranges WHERE view=1", [])?;
        tx.execute("DELETE FROM views WHERE id=1", [])?;
        tx.commit()?;
        Ok(())
    }
    pub fn read(&self, view: i64, off: u64, buf: &mut [u8]) -> Result<usize> {
        let n = (self.size(view)?.saturating_sub(off)).min(buf.len() as u64) as usize;
        if n == 0 {
            return Ok(0);
        }
        let end = off + n as u64;
        buf[..n].fill(0);
        let mut stmt=self.db.prepare_cached("SELECT start,end,literal,src FROM ranges WHERE view=?1
            AND start>=COALESCE((SELECT max(start) FROM ranges WHERE view=?1 AND start<=?2),0) AND start<?3 ORDER BY start")?;
        let mut rows = stmt.query(params![view, off, end])?;
        while let Some(row) = rows.next()? {
            let start: u64 = row.get(0)?;
            let stop: u64 = row.get(1)?;
            let a = start.max(off);
            let b = stop.min(end);
            if b <= a {
                continue;
            }
            let src = row.get::<_, u64>(3)? + (a - start);
            let slice = &mut buf[(a - off) as usize..(b - off) as usize];
            if let Some(id) = row.get::<_, Option<i64>>(2)? {
                let mut q = self
                    .db
                    .prepare_cached("SELECT substr(data,?2,?3) FROM literals WHERE id=?1")?;
                let mut data = q.query(params![id, src + 1, b - a])?;
                let row = data.next()?.context("missing saved Office bytes")?;
                let bytes = row.get_ref(0)?.as_blob()?;
                ensure!(bytes.len() == slice.len(), "short Office literal");
                slice.copy_from_slice(bytes);
            } else {
                self.base.read_exact_at(slice, src)?;
            }
        }
        Ok(n)
    }
    pub fn truncate(&mut self, size: u64) -> Result<()> {
        ensure!(size <= i64::MAX as u64, "invalid file size");
        let tx = self.db.transaction()?;
        tx.execute("UPDATE merge_state SET pos=?1 WHERE pos>?1", [size])?;
        discard(&tx, size, i64::MAX as u64)?;
        tx.execute("UPDATE views SET size=?1 WHERE id=0", [size])?;
        tx.commit()?;
        self.scan = Scan::default();
        Ok(())
    }
    fn note_literal_bytes(&mut self, count: usize) -> Result<()> {
        if self.literal_bytes >= LITERAL {
            return Ok(());
        }
        let total = self.literal_bytes.saturating_add(count);
        if total >= LITERAL {
            let page_size: u32 = self
                .db
                .pragma_query_value(None, "page_size", |r| r.get(0))?;
            self.db.pragma_update(
                None,
                "wal_autocheckpoint",
                LITERAL_CHECKPOINT_BYTES / page_size,
            )?;
        }
        self.literal_bytes = total;
        Ok(())
    }

    pub fn write(&mut self, off: u64, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let end = off
            .checked_add(bytes.len() as u64)
            .filter(|v| *v <= i64::MAX as u64)
            .context("offset overflow")?;
        if !self.indexed {
            self.index = Package::open(&self.base)?;
            self.indexed = true;
        }
        let mut hints = self
            .index
            .as_ref()
            .map(|idx| self.scan.candidates(idx, off, bytes))
            .unwrap_or_default();
        // Most bytes in a presentation are sequential pieces of a known
        // image. One comparison and two prepared updates extend its run.
        let mut whole_literal = hints.is_empty();
        if let [(0, stop, source)] = hints.as_slice()
            && *stop == bytes.len()
            && bytes.len() <= LITERAL
        {
            self.buffer.resize(bytes.len(), 0);
            self.base.read_exact_at(&mut self.buffer, *source)?;
            if self.buffer == bytes {
                let tx = self.db.transaction()?;
                let extended=tx.prepare_cached("UPDATE ranges SET end=?2 WHERE view=0 AND start=(SELECT max(start) FROM ranges WHERE view=0)
                    AND end=?1 AND literal IS NULL AND src+(end-start)=?3 AND (SELECT size FROM views WHERE id=0)=?1")?
                    .execute(params![off,end,source])?;
                if extended == 1 {
                    tx.prepare_cached("UPDATE views SET size=?1 WHERE id=0")?
                        .execute([end])?;
                    tx.commit()?;
                    self.checkpoints.note(&self.db);
                    return Ok(());
                }
            } else {
                whole_literal = true;
            }
        }
        if whole_literal {
            self.note_literal_bytes(bytes.len())?;
            hints.clear();
        }
        let tx = self.db.transaction()?;
        // Past EOF there are no ranges to split or merge progress to rewind.
        // Read and update the size in this transaction in case another handle wrote.
        if whole_literal
            && tx
                .prepare_cached("UPDATE views SET size=?2 WHERE id=0 AND size<=?1")?
                .execute(params![off, end])?
                == 1
        {
            literal(&tx, off, bytes)?;
            tx.commit()?;
            self.checkpoints.note(&self.db);
            return Ok(());
        }
        tx.execute("UPDATE merge_state SET pos=?1 WHERE pos>?1", [off])?;
        discard(&tx, off, end)?;
        let mut next = 0;
        self.buffer.resize(bytes.len().min(LITERAL), 0);
        let buffer = &mut self.buffer;
        for (a, b, src) in hints {
            literal(&tx, off + next as u64, &bytes[next..a])?;
            let mut p = a;
            while p < b {
                let len = (b - p).min(buffer.len());
                let at = src + (p - a) as u64;
                self.base.read_exact_at(&mut buffer[..len], at)?;
                if buffer[..len] == bytes[p..p + len] {
                    reference(&tx, off + p as u64, len as u64, at)?;
                } else {
                    literal(&tx, off + p as u64, &bytes[p..p + len])?;
                }
                p += len;
            }
            next = b;
        }
        literal(&tx, off + next as u64, &bytes[next..])?;
        tx.execute("UPDATE views SET size=max(size,?1) WHERE id=0", [end])?;
        tx.commit()?;
        self.checkpoints.note(&self.db);
        Ok(())
    }
}
fn reference(tx: &Transaction<'_>, start: u64, len: u64, src: u64) -> Result<()> {
    if tx.execute("UPDATE ranges SET end=?3 WHERE view=0 AND start=(SELECT max(start) FROM ranges WHERE view=0 AND start<?1) AND end=?1 AND literal IS NULL AND src+(end-start)=?2",params![start,src,start+len])?==0 {
        tx.execute("INSERT INTO ranges VALUES(0,?1,?2,NULL,?3)",params![start,start+len,src])?;
    }
    Ok(())
}
fn literal(tx: &Transaction<'_>, mut start: u64, bytes: &[u8]) -> Result<()> {
    for piece in bytes.chunks(LITERAL) {
        let end = start + piece.len() as u64;
        tx.prepare_cached("INSERT INTO literals(data) VALUES(?1)")?
            .execute([piece])?;
        tx.prepare_cached("INSERT INTO ranges VALUES(0,?1,?2,?3,0)")?
            .execute(params![start, end, tx.last_insert_rowid()])?;
        start = end;
    }
    Ok(())
}
// Split crossing rows before deleting their middles so the last-reference
// trigger cannot reclaim a literal still used by either surviving slice.
fn discard(tx: &Transaction<'_>, start: u64, end: u64) -> Result<()> {
    for at in [start, end] {
        let split:Option<(u64,u64,Option<i64>,u64)>=tx.query_row(
            "SELECT start,end,literal,src FROM ranges WHERE view=0 AND start=(SELECT max(start) FROM ranges WHERE view=0 AND start<?1) AND end>?1",
            [at],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        if let Some((old, stop, id, src)) = split {
            tx.execute(
                "UPDATE ranges SET end=?2 WHERE view=0 AND start=?1",
                params![old, at],
            )?;
            tx.execute(
                "INSERT INTO ranges VALUES(0,?1,?2,?3,?4)",
                params![at, stop, id, src + (at - old)],
            )?;
        }
    }
    tx.execute(
        "DELETE FROM ranges WHERE view=0 AND start>=?1 AND start<?2",
        params![start, end],
    )?;
    Ok(())
}
pub(crate) use twodrive_core::cache_content_size as logical_size;
pub(crate) fn sync(path: &Path) -> Result<()> {
    let _gate = crate::snapshot::CacheGate::acquire(path)?;
    if let Some(o) = Office::open(path)? {
        o.sync()?;
    }
    Ok(())
}
pub(crate) fn handle_size(path: &Path, file: &File, office: Option<&Office>) -> Result<u64> {
    if let Some(map) = office {
        return map.size(LIVE);
    }
    match Office::open(path)? {
        Some(map) => map.size(LIVE),
        None => Ok(file.metadata()?.len()),
    }
}
pub(crate) fn read_handle(
    path: &Path,
    file: &File,
    office: &mut Option<Office>,
    buf: &mut [u8],
    offset: u64,
) -> Result<usize> {
    let _gate = crate::snapshot::CacheGate::acquire(path)?;
    if office.is_none() {
        *office = Office::open(path)?;
    }
    match office {
        Some(o) => o.read(LIVE, offset, buf),
        None => Ok(file.read_at(buf, offset)?),
    }
}
pub(crate) fn read_at(path: &Path, file: &File, buf: &mut [u8], offset: u64) -> Result<usize> {
    read_handle(path, file, &mut None, buf, offset)
}

#[cfg(test)]
pub(crate) mod tests;
