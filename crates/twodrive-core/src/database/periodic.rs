use super::Database;
use crate::{FileRecord, now_unix};
use rusqlite::{OptionalExtension, params};
use std::path::{Path, PathBuf};

// PowerPoint briefly closes an empty pptXXXX.tmp, writes its package, and
// rotates the old presentation through XXXXXXXX.tmp. A surviving temp is a
// normal user file and must eventually upload under its own name.
pub(super) fn office_upload_after(path: &str, bytes: u64, now: i64) -> Option<i64> {
    if !cfg!(unix) {
        return None;
    }
    let name = Path::new(path).file_name()?.to_str()?;
    let (_, ext) = name.rsplit_once('.')?;
    let temp = powerpoint_backup(name) || powerpoint_temp(name);
    let large_package = bytes >= 10 * 1024 * 1024
        && ["pptx", "pptm", "xlsx", "xlsm", "docx", "docm"]
            .iter()
            .any(|kind| ext.eq_ignore_ascii_case(kind));
    (temp || large_package).then_some(now + 10)
}

pub(super) fn powerpoint_backup(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(stem, ext)| {
        ext.eq_ignore_ascii_case("tmp")
            && stem.len() == 8
            && stem.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

pub(super) fn powerpoint_temp(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(stem, ext)| {
        ext.eq_ignore_ascii_case("tmp")
            && (4..=7).contains(&stem.len())
            && stem.get(..3).is_some_and(|s| s.eq_ignore_ascii_case("ppt"))
            && stem.as_bytes()[3..].iter().all(u8::is_ascii_hexdigit)
    })
}

impl Database {
    pub fn upload_settling(&self, id: &str) -> anyhow::Result<bool> {
        Ok(self.connect()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM files WHERE remote_id=?1 AND upload_after>?2)",
            params![id, now_unix()],
            |r| r.get(0),
        )?)
    }

    pub fn metadata_operation_ready(&self, id: &str, path: &str) -> anyhow::Result<bool> {
        Ok(self.connect()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM pending_metadata_operations m JOIN files f ON f.remote_id=m.local_id
             WHERE m.local_id=?1 AND m.path=?2 AND f.path=m.path
             AND (f.office_backup_from IS NULL OR f.upload_after IS NULL OR f.upload_after<=?3))",
            params![id, path, now_unix()], |r| r.get(0),
        )?)
    }

    /// Removing the deadline only consumes the timer notification. The dirty
    /// generation remains durable until a matching receiver acknowledgement.
    pub fn take_due_closed_uploads(&self, now: i64) -> anyhow::Result<Vec<String>> {
        let conn = self.connect()?;
        if !conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM files WHERE upload_after <= ?1)",
            [now],
            |r| r.get::<_, bool>(0),
        )? {
            return Ok(Vec::new());
        }
        let mut stmt = conn.prepare(
            "UPDATE files SET upload_after=NULL, office_backup_from=NULL, office_replacement_id=NULL WHERE upload_after <= ?1 RETURNING remote_id",
        )?;
        Ok(stmt
            .query_map([now], |r| r.get(0))?
            .collect::<Result<_, _>>()?)
    }

    pub fn open_write_clock(&self) -> anyhow::Result<OpenWriteClock> {
        Ok(OpenWriteClock {
            db: self.connect()?,
            last: Default::default(),
        })
    }
    pub fn note_open_write(
        &self,
        id: &str,
        cache: &Path,
        first_write_time: i64,
    ) -> anyhow::Result<()> {
        self.open_write_clock()?.note(id, cache, first_write_time)
    }

    pub fn pending_open_writes(&self) -> anyhow::Result<Vec<(String, PathBuf, i64)>> {
        Ok(self
            .pending_open_write_timers()?
            .into_iter()
            .map(|(id, cache, since, _)| (id, cache, since))
            .collect())
    }

    pub fn pending_open_write_timers(&self) -> anyhow::Result<Vec<(String, PathBuf, i64, u64)>> {
        let conn = self.connect()?;
        let mut stmt=conn.prepare("SELECT remote_id,cache_path,first_unpublished_write,last_upload_secs FROM files
            WHERE state='writing' AND first_unpublished_write IS NOT NULL AND cache_path IS NOT NULL")?;
        Ok(stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    PathBuf::from(r.get::<_, String>(1)?),
                    r.get(2)?,
                    r.get(3)?,
                ))
            })?
            .collect::<Result<_, _>>()?)
    }

    /// Only consider an unambiguous active upload in the saving directory.
    pub fn office_save_basis(&self, path: &str) -> anyhow::Result<Option<PathBuf>> {
        let c = self.connect()?;
        let mut q=c.prepare("SELECT cache_path FROM files WHERE parent_path=?1 AND upload_cache_path=cache_path
            AND lower(substr(upload_path,-5)) IN ('.pptx','.xlsx','.docx','.pptm','.xlsm','.docm') LIMIT 2")?;
        let paths = q
            .query_map([crate::paths::parent_cloud_path(path)], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok((paths.len() == 1).then(|| PathBuf::from(&paths[0])))
    }

    pub fn first_open_write(&self, id: &str) -> anyhow::Result<Option<i64>> {
        Ok(self.open_write_timing(id)?.map(|(since, _)| since))
    }

    pub fn open_write_timing(&self, id: &str) -> anyhow::Result<Option<(i64, u64)>> {
        Ok(self
            .connect()?
            .query_row(
                "SELECT first_unpublished_write,last_upload_secs FROM files
             WHERE remote_id=?1 AND state='writing' AND first_unpublished_write IS NOT NULL",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// Called under the byte-write gate. Make this exact open revision eligible
    /// for the normal conditional upload, leaving all namespace checks in place.
    pub fn publish_open_write(
        &self,
        r: &FileRecord,
        since: i64,
        size: u64,
    ) -> anyhow::Result<bool> {
        Ok(self.connect()?.execute(
            "UPDATE files SET periodic_open=1,first_unpublished_write=NULL,state='dirty',size=?5,modified_unix=?6
             WHERE remote_id=?1 AND cache_path IS ?2 AND state='writing' AND first_unpublished_write=?3
                AND path=?4 AND etag=?7 AND cloud_remote_id IS ?8",
            params![r.metadata.remote_id, r.cache_path.as_ref().map(|p| p.to_string_lossy()),since,r.metadata.path,
                i64::try_from(size)?,now_unix(),r.metadata.etag,r.cloud_remote_id])?==1)
    }

    pub fn finish_checkpointed_handle(&self, id: &str, cache: &Path) -> anyhow::Result<bool> {
        Ok(self.connect()?.execute(
            "UPDATE files SET periodic_open=0,state=CASE
              WHEN state<>'writing' THEN state
              WHEN pin_explicit=1 OR pin_origin_remote_id IS NOT NULL THEN 'pinned' ELSE 'cached' END
             WHERE remote_id=?1 AND cache_path=?2 AND periodic_open=1 AND first_unpublished_write IS NULL
               AND state IN ('writing','dirty','uploading')",params![id,cache.to_string_lossy()])?==1)
    }

    /// Startup runs before the mount accepts handles. A completed open checkpoint
    /// needs no replay, while dirty/uncertain uploads follow the ordinary recovery path.
    pub fn recover_checkpoint_handles(&self) -> anyhow::Result<()> {
        self.connect()?.execute("UPDATE files SET periodic_open=0, state=CASE
            WHEN state='writing' AND first_unpublished_write IS NULL AND upload_path IS NULL
              AND cloud_remote_id IS NOT NULL THEN CASE
                  WHEN pin_explicit=1 OR pin_origin_remote_id IS NOT NULL THEN 'pinned' ELSE 'cached' END
            ELSE state END WHERE periodic_open=1", [])?;
        Ok(())
    }

    /// Keep the writable cache bound to the open document on conflict. A close
    /// publishes the final revision and ordinary conflict preservation can run.
    pub fn defer_open_conflict(&self, r: &FileRecord) -> anyhow::Result<bool> {
        Ok(self.connect()?.execute(
            "UPDATE files SET state='writing',first_unpublished_write=COALESCE(first_unpublished_write,?3)
             WHERE remote_id=?1 AND cache_path IS ?2 AND periodic_open=1 AND state IN ('uploading','writing')",
            params![r.metadata.remote_id,r.cache_path.as_ref().map(|p| p.to_string_lossy()),now_unix()])?==1)
    }
}
/// Reuse the conditional statement across successive FUSE writes.
pub struct OpenWriteClock {
    db: rusqlite::Connection,
    last: std::cell::RefCell<Option<(String, PathBuf, i64)>>,
}
impl OpenWriteClock {
    pub fn note(&self, id: &str, cache: &Path, first_write_time: i64) -> anyhow::Result<()> {
        // The snapshot publisher, conflict handler and rename paths all use
        // other SQLite connections. Any commit there invalidates this hint.
        // Sample before the update so an intervening commit cannot be missed.
        let version: i64 = self
            .db
            .prepare_cached("PRAGMA data_version")?
            .query_row([], |r| r.get(0))?;
        if self
            .last
            .borrow()
            .as_ref()
            .is_some_and(|(item, path, epoch)| item == id && path == cache && *epoch == version)
        {
            return Ok(());
        }
        self.db
            .prepare_cached(
                "UPDATE files SET first_unpublished_write=COALESCE(first_unpublished_write,?3),
                state='writing' WHERE remote_id=?1 AND cache_path=?2
                AND state IN ('writing','dirty','uploading','cached','pinned')
                AND (first_unpublished_write IS NULL OR state<>'writing')",
            )?
            .execute(params![id, cache.to_string_lossy(), first_write_time])?;
        self.last
            .replace(Some((id.to_owned(), cache.to_owned(), version)));
        Ok(())
    }
}
