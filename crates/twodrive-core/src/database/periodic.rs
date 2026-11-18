use super::Database;
use crate::{FileRecord, now_unix};
use rusqlite::{OptionalExtension, params};
use std::path::{Path, PathBuf};

impl Database {
    pub fn open_write_clock(&self) -> anyhow::Result<OpenWriteClock> {
        Ok(OpenWriteClock(self.connect()?))
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
        let conn = self.connect()?;
        let mut stmt=conn.prepare("SELECT remote_id,cache_path,first_unpublished_write FROM files
            WHERE state='writing' AND first_unpublished_write IS NOT NULL AND cache_path IS NOT NULL")?;
        Ok(stmt
            .query_map([], |r| {
                Ok((r.get(0)?, PathBuf::from(r.get::<_, String>(1)?), r.get(2)?))
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
        Ok(self
            .connect()?
            .query_row(
                "SELECT first_unpublished_write FROM files WHERE remote_id=?1 AND state='writing'",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
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
pub struct OpenWriteClock(rusqlite::Connection);
impl OpenWriteClock {
    pub fn note(&self, id: &str, cache: &Path, first_write_time: i64) -> anyhow::Result<()> {
        self.0
            .prepare_cached(
                "UPDATE files SET first_unpublished_write=COALESCE(first_unpublished_write,?3),
                state='writing' WHERE remote_id=?1 AND cache_path=?2
                AND state IN ('writing','dirty','uploading','cached','pinned')
                AND (first_unpublished_write IS NULL OR state<>'writing')",
            )?
            .execute(params![id, cache.to_string_lossy(), first_write_time])?;
        Ok(())
    }
}
