use super::Database;
use rusqlite::{TransactionBehavior, params};
use std::fs;
use std::path::Path;

use crate::paths::normalize_cloud_path;
use crate::{FileRecord, FileState, now_unix};

impl Database {
    pub fn mark_state(&self, remote_id: &str, state: FileState) -> anyhow::Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE files SET state = ?1 WHERE remote_id = ?2",
            params![state.as_str(), remote_id],
        )?;
        Ok(())
    }

    /// Claim only the snapshot inspected by the worker. Network preflight can
    /// overlap a rename/replacement or a reopen on the FUSE thread.
    pub fn begin_upload(&self, record: &FileRecord) -> anyhow::Result<bool> {
        if !matches!(record.state, FileState::Dirty | FileState::Uploading) {
            return Ok(false);
        }
        let changed = self.connect()?.execute(
            "UPDATE files SET state = 'uploading'
             WHERE remote_id = ?1 AND path = ?2 AND cache_path IS ?3
               AND state = ?4 AND size = ?5 AND modified_unix = ?6
               AND etag = ?7 AND cloud_remote_id IS ?8
               AND NOT EXISTS (SELECT 1 FROM pending_metadata_operations op
                   JOIN files parent ON parent.remote_id = op.local_id
                   WHERE ?2 = parent.path
                      OR substr(?2, 1, length(parent.path) + 1) = parent.path || '/')",
            params![
                record.metadata.remote_id,
                record.metadata.path,
                record
                    .cache_path
                    .as_ref()
                    .map(|path| path.to_string_lossy()),
                record.state.as_str(),
                i64::try_from(record.metadata.size).unwrap_or(i64::MAX),
                record.metadata.modified_unix,
                record.metadata.etag,
                record.cloud_remote_id,
            ],
        )?;
        Ok(changed == 1)
    }

    pub fn mark_dirty_with_size(&self, remote_id: &str, size: u64) -> anyhow::Result<()> {
        let conn = self.connect()?;
        let changed = conn.execute(
            "UPDATE files SET state = ?1, size = ?2, modified_unix = ?3 WHERE remote_id = ?4",
            params![
                FileState::Dirty.as_str(),
                i64::try_from(size).unwrap_or(i64::MAX),
                now_unix(),
                remote_id
            ],
        )?;
        if changed == 0 {
            anyhow::bail!("cannot mark unknown item {remote_id} dirty");
        }
        Ok(())
    }

    pub fn download_generation(&self, remote_id: &str) -> anyhow::Result<i64> {
        Ok(self.connect()?.query_row(
            "SELECT download_generation FROM files WHERE remote_id = ?1",
            params![remote_id],
            |row| row.get(0),
        )?)
    }

    pub fn begin_hydration(&self, remote_id: &str, generation: i64) -> anyhow::Result<bool> {
        Ok(self.connect()?.execute(
            "UPDATE files SET state = CASE WHEN pin_explicit = 1 OR pin_origin_remote_id IS NOT NULL THEN 'pinned' ELSE 'hydrating' END, cache_path = NULL, cache_accessed_unix = NULL
             WHERE remote_id = ?1 AND download_generation = ?2 AND release_pending = 0
             AND state IN ('online_only', 'hydrating', 'pinned', 'cached', 'error') AND cloud_remote_id IS NOT NULL",
            params![remote_id, generation])? == 1)
    }

    pub fn finish_hydration(
        &self,
        remote_id: &str,
        generation: i64,
        temporary: &Path,
        cache: &Path,
    ) -> anyhow::Result<bool> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let eligible: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM files WHERE remote_id = ?1 AND download_generation = ?2
             AND release_pending = 0 AND state IN ('online_only', 'hydrating', 'pinned'))",
            params![remote_id, generation],
            |row| row.get(0),
        )?;
        if !eligible {
            return Ok(false);
        }
        // Serialize publication with release and local writes, including the final-byte race.
        fs::rename(temporary, cache)?;
        tx.execute("UPDATE files SET state = CASE WHEN pin_explicit = 1 OR pin_origin_remote_id IS NOT NULL THEN 'pinned' ELSE 'cached' END,
            cache_path = ?2, cache_accessed_unix = ?3 WHERE remote_id = ?1",
            params![remote_id, cache.to_string_lossy(), now_unix()])?;
        tx.commit()?;
        Ok(true)
    }

    pub fn fail_hydration(&self, remote_id: &str, generation: i64) -> anyhow::Result<()> {
        self.connect()?.execute(
            "UPDATE files SET state = 'online_only' WHERE remote_id = ?1
            AND download_generation = ?2 AND state = 'hydrating' AND cache_path IS NULL",
            params![remote_id, generation],
        )?;
        Ok(())
    }

    pub fn mark_cached(&self, remote_id: &str, cache_path: &Path) -> anyhow::Result<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            UPDATE files
            SET state = CASE
                    WHEN pin_explicit = 1 OR pin_origin_remote_id IS NOT NULL THEN 'pinned'
                    ELSE ?1
                END,
                cache_path = ?2,
                cache_accessed_unix = ?3
            WHERE remote_id = ?4
            "#,
            params![
                FileState::Cached.as_str(),
                cache_path.to_string_lossy(),
                now_unix(),
                remote_id
            ],
        )?;
        Ok(())
    }

    pub fn mark_online_only(&self, remote_id: &str) -> anyhow::Result<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            UPDATE files
            SET state = CASE
                    WHEN pin_explicit = 1 OR pin_origin_remote_id IS NOT NULL THEN 'hydrating'
                    ELSE ?1
                END,
                cache_path = NULL,
                cache_accessed_unix = NULL
            WHERE remote_id = ?2
            "#,
            params![FileState::OnlineOnly.as_str(), remote_id],
        )?;
        Ok(())
    }

    pub fn mark_cache_accessed(&self, remote_id: &str) -> anyhow::Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE files SET cache_accessed_unix = ?1 WHERE remote_id = ?2",
            params![now_unix(), remote_id],
        )?;
        Ok(())
    }

    pub fn release_record(&self, record: &FileRecord) -> anyhow::Result<bool> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = self.query_record(&tx,
            "SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id, local_mode FROM files WHERE remote_id = ?1",
            params![record.metadata.remote_id])?;
        let Some(record) = current else {
            return Ok(false);
        };
        if record.metadata.is_dir
            || record.effective_pinned()
            || !matches!(record.state, FileState::Cached)
            || record.cloud_remote_id.is_none()
        {
            return Ok(false);
        }
        let cache_file = match record.cache_path.as_ref().map(fs::File::open).transpose() {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => return Err(err.into()),
        };
        if let Some(file) = &cache_file {
            match file.try_lock() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
                Err(err) => return Err(err.into()),
            }
        }
        if let Some(cache_path) = &record.cache_path {
            match fs::remove_file(cache_path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        }
        tx.execute(
            "UPDATE files SET state = 'online_only', cache_path = NULL, cache_accessed_unix = NULL, release_pending = 0 WHERE remote_id = ?1",
            params![record.metadata.remote_id],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn finish_pending_releases(&self) -> anyhow::Result<usize> {
        let conn = self.connect()?;
        let ids = conn
            .prepare("SELECT remote_id FROM files WHERE release_pending = 1")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(conn);
        let mut count = 0;
        for id in ids {
            if let Some(record) = self.get_by_remote_id(&id)? {
                count += usize::from(self.release_record(&record)?);
            }
        }
        Ok(count)
    }

    pub fn pending_release_count(&self, path: &str) -> anyhow::Result<usize> {
        let path = normalize_cloud_path(path);
        Ok(self.connect()?.query_row(
            "SELECT count(*) FROM files WHERE release_pending = 1 AND (path = ?1 OR ?1 = '/' OR substr(path, 1, length(?1) + 1) = ?1 || '/')",
            params![path], |row| row.get(0))?)
    }

    pub fn release_path(&self, path: &str) -> anyhow::Result<usize> {
        let Some(record) = self.get_by_path(path)? else {
            anyhow::bail!(
                "path is not in twodrive metadata: {}",
                normalize_cloud_path(path)
            );
        };

        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // An explicit release overrides Always Keep for exactly this subtree.
        // Change policy in the same transaction as download cancellation so a
        // concurrent pin/download cannot observe an unpinned but active transfer.
        tx.execute(
            "UPDATE files SET pin_explicit = 0, pin_origin_remote_id = NULL,
             pin_inheritance_blocked = 1,
             state = CASE WHEN state = 'pinned' THEN
                 CASE WHEN is_dir = 1 THEN 'online_only'
                      WHEN cache_path IS NOT NULL THEN 'cached' ELSE 'hydrating' END
                 ELSE state END
             WHERE path = ?1 OR ?1 = '/' OR substr(path, 1, length(?1) + 1) = ?1 || '/'",
            params![record.metadata.path],
        )?;
        let cancelled: usize = tx.query_row(
            "SELECT count(*) FROM files WHERE is_dir = 0 AND pin_explicit = 0 AND pin_origin_remote_id IS NULL
             AND state = 'hydrating' AND cache_path IS NULL
             AND (path = ?1 OR ?1 = '/' OR substr(path, 1, length(?1) + 1) = ?1 || '/')",
            params![record.metadata.path], |row| row.get(0))?;
        tx.execute(
            "UPDATE files SET download_generation = download_generation + 1,
             release_pending = CASE WHEN state = 'online_only' OR (state = 'hydrating' AND cache_path IS NULL) THEN 0 ELSE 1 END,
             state = CASE WHEN state = 'hydrating' AND cache_path IS NULL THEN 'online_only' ELSE state END
             WHERE is_dir = 0 AND pin_explicit = 0 AND pin_origin_remote_id IS NULL
             AND (path = ?1 OR ?1 = '/' OR substr(path, 1, length(?1) + 1) = ?1 || '/')",
            params![record.metadata.path],
        )?;
        tx.commit()?;
        drop(conn);
        let records = if record.metadata.is_dir {
            self.list_descendants(&record.metadata.path)?
        } else {
            vec![record]
        };
        let mut released = cancelled;
        for record in records {
            released += usize::from(self.release_record(&record)?);
        }
        Ok(released)
    }

    pub fn prune_cache(&self, older_than_seconds: i64) -> anyhow::Result<usize> {
        let cutoff = now_unix().saturating_sub(older_than_seconds);
        let records = self
            .all_records()?
            .into_iter()
            .filter(|record| {
                record.state == FileState::Cached
                    && !record.effective_pinned()
                    && record
                        .cache_accessed_unix
                        .map(|accessed| accessed <= cutoff)
                        .unwrap_or(true)
            })
            .collect::<Vec<_>>();

        let mut pruned = 0;
        for record in records {
            if self.release_record(&record)? {
                pruned += 1;
            }
        }
        Ok(pruned)
    }
}
