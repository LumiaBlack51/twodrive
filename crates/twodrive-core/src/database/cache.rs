use super::Database;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::fs;
use std::path::Path;

use crate::paths::normalize_cloud_path;
use crate::{FileRecord, FileState, now_unix};

impl Database {
    /// Claim the inspected cache/remote baseline before a delayed first write.
    /// A concurrent delta or upload completion must not supply a newer ETag
    /// for bytes that were opened from an older cached version.
    pub fn begin_cached_write(&self, record: &FileRecord) -> anyhow::Result<bool> {
        Ok(self.connect()?.execute(
            "UPDATE files SET state = ?8
             WHERE remote_id = ?1 AND cache_path IS ?2 AND cloud_remote_id IS ?3
               AND etag = ?4 AND state = ?5 AND size = ?6 AND modified_unix = ?7",
            params![
                record.metadata.remote_id,
                record
                    .cache_path
                    .as_ref()
                    .map(|path| path.to_string_lossy()),
                record.cloud_remote_id,
                record.metadata.etag,
                record.state.as_str(),
                record.metadata.size as i64,
                record.metadata.modified_unix,
                if self.excludes_upload(record) {
                    "local_only"
                } else {
                    "writing"
                }
            ],
        )? == 1)
    }

    /// Claim a truncating open before touching cache bytes. Publish its zero
    /// size as Writing, never as an uploadable handle-less generation.
    pub fn begin_truncated_write(&self, record: &FileRecord, cache: &Path) -> anyhow::Result<bool> {
        Ok(self.connect()?.execute(
            "UPDATE files SET state = ?1, size = 0,
            cache_path = ?2, cache_accessed_unix = ?3, modified_unix = ?3
            WHERE remote_id = ?4 AND state = ?5 AND cache_path IS ?6
                AND cloud_remote_id IS ?7 AND etag = ?8 AND size = ?9 AND modified_unix = ?10",
            params![
                if self.excludes_upload(record) {
                    "local_only"
                } else {
                    "writing"
                },
                cache.to_string_lossy(),
                now_unix(),
                record.metadata.remote_id,
                record.state.as_str(),
                record.cache_path.as_ref().map(|p| p.to_string_lossy()),
                record.cloud_remote_id,
                record.metadata.etag,
                record.metadata.size as i64,
                record.metadata.modified_unix
            ],
        )? == 1)
    }

    /// Do not let a failed, older upload turn a reopened/new generation into a
    /// conflict. The successful claim must still describe the same snapshot.
    pub fn mark_upload_conflict(&self, record: &FileRecord) -> anyhow::Result<bool> {
        Ok(self.connect()?.execute(
            "UPDATE files SET state = 'conflict'
             WHERE remote_id = ?1 AND state = 'uploading' AND path = ?2
               AND cache_path IS ?3 AND cloud_remote_id IS ?4
               AND size = ?5 AND modified_unix = ?6 AND etag = ?7",
            params![
                record.metadata.remote_id,
                record.metadata.path,
                record.cache_path.as_ref().map(|p| p.to_string_lossy()),
                record.cloud_remote_id,
                record.metadata.size as i64,
                record.metadata.modified_unix,
                record.metadata.etag
            ],
        )? == 1)
    }

    /// The backend call has returned an error. A known target cannot become an
    /// orphan; keep its old ETag so an uncertain server commit still conflicts.
    /// Unknown create claims and deletion tombstones retain their uncertainty.
    pub fn retire_failed_known_upload_claim(&self, record: &FileRecord) -> anyhow::Result<()> {
        if record.cloud_remote_id.is_none() {
            return Ok(());
        }
        self.connect()?.execute(
            "UPDATE files SET upload_path = NULL, upload_target_id = NULL,
            upload_cache_path = NULL WHERE remote_id = ?1 AND cloud_remote_id IS ?2
                AND upload_target_id IS ?2 AND upload_path = ?3 AND upload_cache_path IS ?4",
            params![
                record.metadata.remote_id,
                record.cloud_remote_id,
                record.metadata.path,
                record.cache_path.as_ref().map(|p| p.to_string_lossy())
            ],
        )?;
        Ok(())
    }

    /// Startup only, before workers exist. Preserve unbound/unknown create
    /// claims and all tombstones; known targets retain their version baseline.
    pub fn retire_interrupted_known_upload_claims(&self) -> anyhow::Result<()> {
        self.connect()?.execute(
            "UPDATE files SET upload_path = NULL, upload_target_id = NULL,
            upload_cache_path = NULL WHERE upload_target_id IS NOT NULL
                AND cloud_remote_id = upload_target_id",
            [],
        )?;
        Ok(())
    }

    pub fn finish_failed_upload(
        &self,
        record: &FileRecord,
        state: FileState,
    ) -> anyhow::Result<()> {
        self.connect()?.execute(
            "UPDATE files SET state = ?1
            WHERE remote_id = ?2 AND state = 'uploading' AND path = ?3
                AND cache_path IS ?4 AND etag = ?5",
            params![
                state.as_str(),
                record.metadata.remote_id,
                record.metadata.path,
                record.cache_path.as_ref().map(|p| p.to_string_lossy()),
                record.metadata.etag
            ],
        )?;
        Ok(())
    }

    pub fn mark_state(&self, remote_id: &str, state: FileState) -> anyhow::Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE files SET state = ?1 WHERE remote_id = ?2",
            params![state.as_str(), remote_id],
        )?;
        Ok(())
    }

    /// A backup-style save can leave the original cloud identity on an excluded
    /// local backup. Hand it to the new original without accepting a newer
    /// remote version as the user's baseline or uploading the backup itself.
    pub fn adopt_excluded_backup_identity(
        &self,
        record: &FileRecord,
        remote: &crate::MetadataEntry,
    ) -> anyhow::Result<Option<FileRecord>> {
        if record.cloud_remote_id.is_some()
            || self.excludes_upload(record)
            || record.metadata.path != remote.path
            || record.state != FileState::Dirty
        {
            return Ok(None);
        }
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let owner: Option<(String, String)> = tx
            .query_row(
                "SELECT remote_id, etag FROM files WHERE cloud_remote_id = ?1
                AND remote_id <> ?2 AND state = 'local_only' AND is_dir = 0
                AND cache_path IS NOT NULL AND etag <> '' AND upload_path IS NULL
                AND NOT EXISTS (SELECT 1 FROM pending_metadata_operations WHERE local_id = files.remote_id)",
                params![remote.remote_id, record.metadata.remote_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((owner_id, baseline_etag)) = owner else {
            return Ok(None);
        };
        let deleting: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM pending_deletes
            WHERE cloud_remote_id = ?1 OR remote_id = ?1 OR local_id IN (?2, ?3))",
            params![remote.remote_id, owner_id, record.metadata.remote_id],
            |row| row.get(0),
        )?;
        if deleting {
            return Ok(None);
        }
        tx.execute(
            "UPDATE files SET cloud_remote_id = NULL, etag = '' WHERE remote_id = ?1",
            params![owner_id],
        )?;
        let claimed = tx.execute(
            "UPDATE files SET cloud_remote_id = ?1, etag = ?2
            WHERE remote_id = ?3 AND cloud_remote_id IS NULL AND state = 'dirty'
                AND path = ?4 AND cache_path IS ?5 AND size = ?6 AND modified_unix = ?7
                AND etag = ?8 AND upload_path IS NULL",
            params![
                remote.remote_id,
                baseline_etag,
                record.metadata.remote_id,
                record.metadata.path,
                record.cache_path.as_ref().map(|p| p.to_string_lossy()),
                record.metadata.size as i64,
                record.metadata.modified_unix,
                record.metadata.etag
            ],
        )?;
        if claimed != 1 {
            return Ok(None);
        } // Roll back the detach too.
        tx.execute(
            "DELETE FROM pending_metadata_operations WHERE local_id = ?1",
            params![owner_id],
        )?;
        tx.commit()?;
        self.get_by_remote_id(&record.metadata.remote_id)
    }

    /// Claim only the snapshot inspected by the worker. Network preflight can
    /// overlap a rename/replacement or a reopen on the FUSE thread.
    pub fn begin_upload(&self, record: &FileRecord) -> anyhow::Result<bool> {
        if self.excludes_upload(record)
            || !matches!(record.state, FileState::Dirty | FileState::Uploading)
        {
            return Ok(false);
        }
        let changed = self.connect()?.execute(
            "UPDATE files SET state = 'uploading', upload_path = path,
                upload_target_id = cloud_remote_id, upload_cache_path = cache_path
             WHERE remote_id = ?1 AND path = ?2 AND cache_path IS ?3
               AND state = ?4 AND size = ?5 AND modified_unix = ?6
               AND etag = ?7 AND cloud_remote_id IS ?8
               AND NOT EXISTS (SELECT 1 FROM pending_deletes WHERE path = ?2 OR upload_path = ?2 OR uploaded_path = ?2)
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
            "UPDATE files SET state = CASE WHEN state = 'local_only' THEN state ELSE ?1 END, size = ?2, modified_unix = ?3 WHERE remote_id = ?4",
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

    /// Detach an inconsistent clean cache without deleting recovery material or
    /// touching a local write that started after the caller inspected it.
    pub fn invalidate_cached_snapshot(&self, record: &FileRecord) -> anyhow::Result<bool> {
        Ok(self.connect()?.execute(
            "UPDATE files SET cache_path = NULL, cache_accessed_unix = NULL,
                state = CASE WHEN pin_explicit = 1 OR pin_origin_remote_id IS NOT NULL
                    THEN 'hydrating' ELSE 'online_only' END,
                download_generation = download_generation + 1
             WHERE remote_id = ?1 AND cache_path IS ?2 AND etag = ?3 AND size = ?4
               AND state IN ('cached', 'pinned') AND cloud_remote_id IS NOT NULL",
            params![
                record.metadata.remote_id,
                record
                    .cache_path
                    .as_ref()
                    .map(|path| path.to_string_lossy()),
                record.metadata.etag,
                record.metadata.size as i64
            ],
        )? == 1)
    }

    /// Reclaim only an explicitly retired cache, never arbitrary files found
    /// in the cache directory. Old untracked files can be recovery material.
    pub fn prune_retired_cache(&self, cache_dir: &Path) -> anyhow::Result<usize> {
        let paths = self
            .connect()?
            .prepare("SELECT cache_path FROM retired_cache")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut removed = 0;
        for path in paths {
            let path = Path::new(&path);
            if path.parent() == Some(cache_dir) && self.remove_unreferenced_cache(path)? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// The write transaction protects all durable references; file locks also
    /// protect readers, writers and uploads after their database path changes.
    pub fn remove_unreferenced_cache(&self, path: &Path) -> anyhow::Result<bool> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let referenced: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM files WHERE cache_path = ?1 OR upload_cache_path = ?1)
                OR EXISTS(SELECT 1 FROM pending_deletes WHERE cache_path = ?1 OR upload_cache_path = ?1)",
            params![path.to_string_lossy()], |row| row.get(0))?;
        if referenced {
            return Ok(false);
        }
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() => Some(metadata),
            Ok(_) => return Ok(false),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => return Err(err.into()),
        };
        let file = if metadata.is_some() {
            Some(fs::File::open(path)?)
        } else {
            None
        };
        if let Some(file) = &file {
            match file.try_lock() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
                Err(err) => return Err(err.into()),
            }
            fs::remove_file(path)?;
        }
        tx.execute(
            "DELETE FROM retired_cache WHERE cache_path = ?1",
            params![path.to_string_lossy()],
        )?;
        tx.commit()?;
        Ok(file.is_some())
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
        let expected_size: u64 = tx.query_row(
            "SELECT size FROM files WHERE remote_id = ?1",
            params![remote_id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            fs::metadata(temporary)?.len() == expected_size,
            "download length does not match the current metadata"
        );
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
                    WHEN state = 'local_only' THEN state
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
                cache_accessed_unix = NULL,
                upload_path = NULL, upload_target_id = NULL, upload_cache_path = NULL
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
        self.release_cached_record(record, None)
    }

    fn release_cached_record(
        &self,
        record: &FileRecord,
        cutoff: Option<i64>,
    ) -> anyhow::Result<bool> {
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
            || cutoff.is_some_and(|cutoff| {
                // A file can move into the trash after the retention scan.
                // Check its current path under the same lock as cache removal.
                is_mount_trash_path(&record.metadata.path)
                    || record
                        .cache_accessed_unix
                        .is_some_and(|accessed| accessed > cutoff)
            })
        {
            return Ok(false);
        }
        if let Some(path) = &record.cache_path {
            let protected: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM files WHERE upload_cache_path=?1 OR (remote_id<>?2 AND cache_path=?1))
                 OR EXISTS(SELECT 1 FROM pending_deletes WHERE cache_path=?1 OR upload_cache_path=?1)",
                params![path.to_string_lossy(), record.metadata.remote_id], |row| row.get(0))?;
            if protected {
                return Ok(false);
            }
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
             release_pending = CASE WHEN state = 'local_only' OR state = 'online_only' OR (state = 'hydrating' AND cache_path IS NULL) THEN 0 ELSE 1 END,
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

    /// Expire ordinary cache while leaving on-mount trash to explicit actions.
    pub fn prune_cache(&self, older_than_seconds: i64) -> anyhow::Result<usize> {
        let cutoff = now_unix().saturating_sub(older_than_seconds);
        let records = self
            .all_records()?
            .into_iter()
            .filter(|record| {
                record.state == FileState::Cached
                    && !record.effective_pinned()
                    && !is_mount_trash_path(&record.metadata.path)
                    && record
                        .cache_accessed_unix
                        .map(|accessed| accessed <= cutoff)
                        .unwrap_or(true)
            })
            .collect::<Vec<_>>();

        let mut pruned = 0;
        for record in records {
            if self.release_cached_record(&record, Some(cutoff))? {
                pruned += 1;
            }
        }
        Ok(pruned)
    }
}

fn is_mount_trash_path(path: &str) -> bool {
    // freedesktop trash lives at the mount root, in .Trash-$uid or .Trash/$uid.
    // Protect the whole subtree (info, payloads and temporary recovery files),
    // without treating every .trashinfo file or similarly named folder as trash.
    let Some(relative) = path.strip_prefix('/') else {
        return false;
    };
    let mut components = relative.split('/');
    let root = components.next().unwrap_or_default();
    let uid = if root == ".Trash" {
        components.next().unwrap_or_default()
    } else if let Some(uid) = root.strip_prefix(".Trash-") {
        uid
    } else {
        return false;
    };
    !uid.is_empty() && uid.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod retention_tests {
    use super::*;
    use crate::MetadataEntry;

    #[test]
    fn retention_keeps_trash_info_and_payloads_without_pinning() {
        let root = tempfile::tempdir().unwrap();
        let db = Database::new(root.path().join("db"));
        db.init().unwrap();
        let paths = [
            ("/.Trash-1000/info/report.trashinfo", true),
            ("/.Trash-1000/info/report.trashinfo.ABC123", true),
            ("/.Trash-1000/files/report", true),
            ("/.Trash-1000/files/project/nested/file", true),
            ("/.Trash/1001/info/report.trashinfo", true),
            ("/.Trash/1001/files/project/nested/file", true),
            ("/.Trash-0/expunged/report", true),
            ("/.Trash-1000/directorysizes", true),
            ("/Documents/report.trashinfo", false),
            ("/Documents/.Trash-1000/files/report", false),
            ("/.Trash-notes/files/report", false),
            ("/.Trash-1000-backup/files/report", false),
            ("/.Trash-/files/report", false),
            ("/.Trash/notes/files/report", false),
            ("/.Trash-backup/1000/files/report", false),
            ("/.trash-1000/files/report", false),
        ];
        for (index, (path, _)) in paths.iter().enumerate() {
            let id = format!("file-{index}");
            let cache = root.path().join(&id);
            fs::write(&cache, b"data").unwrap();
            db.upsert_metadata(&MetadataEntry::new_file(&id, *path, 4, 1, "etag"))
                .unwrap();
            db.mark_cached(&id, &cache).unwrap();
        }
        db.upsert_metadata(&MetadataEntry::new_file(
            "cloud-only",
            "/.Trash-1000/files/never-downloaded",
            4,
            1,
            "etag",
        ))
        .unwrap();
        db.connect()
            .unwrap()
            .execute("UPDATE files SET cache_accessed_unix=1", [])
            .unwrap();
        db.connect()
            .unwrap()
            .execute(
                "UPDATE files SET cache_accessed_unix=NULL WHERE remote_id='file-0'",
                [],
            )
            .unwrap();
        let restarted = Database::new(db.path());
        restarted.init().unwrap();
        assert_eq!(
            restarted.prune_cache(30 * 24 * 60 * 60).unwrap(),
            paths.iter().filter(|(_, keep)| !keep).count()
        );
        // An aggressive prune uses the same exemption without changing pin policy.
        assert_eq!(restarted.prune_cache(0).unwrap(), 0);
        for (index, (path, keep)) in paths.iter().enumerate() {
            let record = restarted.get_by_path(path).unwrap().unwrap();
            assert!(!record.effective_pinned(), "{path}");
            assert_eq!(record.cache_path.is_some(), *keep, "{path}");
            let cache = root.path().join(format!("file-{index}"));
            assert_eq!(cache.exists(), *keep, "{path}");
            if *keep {
                assert_eq!(record.state, FileState::Cached, "{path}");
                assert_eq!(fs::read(cache).unwrap(), b"data");
            } else {
                assert_eq!(record.state, FileState::OnlineOnly, "{path}");
            }
        }
        let cloud_only = restarted.get_by_remote_id("cloud-only").unwrap().unwrap();
        assert_eq!(cloud_only.state, FileState::OnlineOnly);
        assert!(cloud_only.cache_path.is_none());
    }

    #[test]
    fn retention_rechecks_trash_moves_before_releasing_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let db = Database::new(root.path().join("db"));
        db.init().unwrap();
        let cache = root.path().join("cache");
        fs::write(&cache, b"data").unwrap();
        db.upsert_metadata(&MetadataEntry::new_file("file", "/report", 4, 1, "etag"))
            .unwrap();
        db.mark_cached("file", &cache).unwrap();
        db.connect()
            .unwrap()
            .execute("UPDATE files SET cache_accessed_unix=1", [])
            .unwrap();
        let scanned = db.get_by_path("/report").unwrap().unwrap();
        db.move_subtree_and_queue("file", "/.Trash-1000/files/report")
            .unwrap();
        assert!(
            !db.release_cached_record(&scanned, Some(now_unix() - 60))
                .unwrap()
        );
        assert_eq!(fs::read(&cache).unwrap(), b"data");
        db.move_subtree_and_queue("file", "/restored-report")
            .unwrap();
        assert_eq!(db.prune_cache(60).unwrap(), 1);
        assert!(!cache.exists());
        assert_eq!(
            db.get_by_path("/restored-report").unwrap().unwrap().state,
            FileState::OnlineOnly
        );
    }

    #[test]
    fn explicit_release_can_still_free_trash_cache() {
        let root = tempfile::tempdir().unwrap();
        let db = Database::new(root.path().join("db"));
        db.init().unwrap();
        let cache = root.path().join("cache");
        fs::write(&cache, b"data").unwrap();
        let path = "/.Trash-1000/files/report";
        db.upsert_metadata(&MetadataEntry::new_file("file", path, 4, 1, "etag"))
            .unwrap();
        db.mark_cached("file", &cache).unwrap();
        assert_eq!(db.prune_cache(0).unwrap(), 0);
        assert_eq!(db.release_path(path).unwrap(), 1);
        assert!(!cache.exists());
        let record = db.get_by_path(path).unwrap().unwrap();
        assert_eq!(record.state, FileState::OnlineOnly);
        assert!(record.cache_path.is_none());
    }

    #[test]
    fn retention_rechecks_recent_access_and_durable_upload_claims() {
        let root = std::env::temp_dir().join(format!(
            "twodrive-retention-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let db = Database::new(root.join("db"));
        db.init().unwrap();
        let cache = root.join("cache");
        fs::write(&cache, b"data").unwrap();
        db.upsert_metadata(&MetadataEntry::new_file("file", "/file", 4, 1, "etag"))
            .unwrap();
        db.mark_cached("file", &cache).unwrap();
        db.connect()
            .unwrap()
            .execute("UPDATE files SET cache_accessed_unix=1", [])
            .unwrap();
        let stale = db.get_by_path("/file").unwrap().unwrap();
        db.mark_cache_accessed("file").unwrap();
        assert!(
            !db.release_cached_record(&stale, Some(now_unix() - 60))
                .unwrap()
        );
        db.connect()
            .unwrap()
            .execute(
                "UPDATE files SET cache_accessed_unix=1, upload_cache_path=cache_path",
                [],
            )
            .unwrap();
        assert_eq!(db.prune_cache(60).unwrap(), 0);
        db.connect()
            .unwrap()
            .execute("UPDATE files SET upload_cache_path=NULL", [])
            .unwrap();
        assert_eq!(db.prune_cache(60).unwrap(), 1);
        assert!(!cache.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
