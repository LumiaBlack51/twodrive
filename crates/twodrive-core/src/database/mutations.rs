use super::Database;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::path::{Path, PathBuf};

use crate::paths::{cloud_name, normalize_cloud_path, parent_cloud_path};
use crate::{
    FileRecord, FileState, MetadataEntry, PendingDelete, PendingMetadataKind,
    PendingMetadataOperation, now_unix,
};

impl Database {
    pub fn move_subtree(&self, remote_id: &str, new_path: &str) -> anyhow::Result<()> {
        let record = self
            .get_by_remote_id(remote_id)?
            .ok_or_else(|| anyhow::anyhow!("cannot move unknown item {remote_id}"))?;
        let new_path = normalize_cloud_path(new_path);
        let entry = if record.metadata.is_dir {
            MetadataEntry::new_dir(
                remote_id,
                &new_path,
                record.metadata.modified_unix,
                &record.metadata.etag,
            )
        } else {
            MetadataEntry::new_file(
                remote_id,
                &new_path,
                record.metadata.size,
                record.metadata.modified_unix,
                &record.metadata.etag,
            )
        };
        self.upsert_metadata(&entry)?;
        self.allow_pin_inheritance(remote_id)
    }

    /// Persist a local create in Writing state before exposing it to workers.
    /// Cloud delta guards must not suppress user writes beneath pending folders.
    pub fn create_local_file(
        &self,
        local_id: &str,
        path: &str,
        cache_path: &Path,
    ) -> anyhow::Result<FileRecord> {
        self.create_local_file_with_mode(local_id, path, cache_path, 0o644)
    }

    pub fn create_local_file_with_mode(
        &self,
        local_id: &str,
        path: &str,
        cache_path: &Path,
        mode: u32,
    ) -> anyhow::Result<FileRecord> {
        let path = normalize_cloud_path(path);
        let conn = self.connect()?;
        conn.execute(
            r#"
            INSERT INTO files(
                remote_id, cloud_remote_id, path, parent_path, name, is_dir, size,
                modified_unix, etag, state, cache_path, cache_accessed_unix,
                pin_inheritance_blocked, local_mode
            ) VALUES (?1, NULL, ?2, ?3, ?4, 0, 0, ?5, '', ?8, ?6, ?5, 1, ?7)
            "#,
            params![
                local_id,
                path,
                parent_cloud_path(&path),
                cloud_name(&path),
                now_unix(),
                cache_path.to_string_lossy(),
                mode & 0o777,
                if self.excludes_file_name(&cloud_name(&path)) {
                    "local_only"
                } else {
                    "writing"
                }
            ],
        )?;
        self.get_by_remote_id(local_id)?
            .ok_or_else(|| anyhow::anyhow!("created local file disappeared"))
    }

    pub fn create_local_directory(&self, local_id: &str, path: &str) -> anyhow::Result<FileRecord> {
        self.create_local_directory_with_mode(local_id, path, 0o755)
    }

    pub fn create_local_directory_with_mode(
        &self,
        local_id: &str,
        path: &str,
        mode: u32,
    ) -> anyhow::Result<FileRecord> {
        let path = normalize_cloud_path(path);
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx
            .query_row(
                "SELECT 1 FROM files WHERE path = ?1 LIMIT 1",
                params![path],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            anyhow::bail!("path already exists: {path}");
        }
        tx.execute(
            r#"
            INSERT INTO files(
                remote_id, cloud_remote_id, path, parent_path, name, is_dir, size,
                modified_unix, etag, state, pin_inheritance_blocked, local_mode
            ) VALUES (?1, NULL, ?2, ?3, ?4, 1, 0, ?5, '', 'dirty', 0, ?6)
            "#,
            params![
                local_id,
                path,
                parent_cloud_path(&path),
                cloud_name(&path),
                now_unix(),
                mode & 0o777
            ],
        )?;
        tx.execute(
            "INSERT INTO pending_metadata_operations(local_id, kind, path, queued_unix) VALUES (?1, 'create_folder', ?2, ?3)",
            params![local_id, path, now_unix()],
        )?;
        tx.commit()?;
        self.recompute_pin_inheritance()?;
        self.get_by_remote_id(local_id)?
            .ok_or_else(|| anyhow::anyhow!("created local directory disappeared"))
    }

    /// Changing permissions must not dirty content or enqueue a cloud upload.
    pub fn set_local_mode(&self, local_id: &str, mode: u32) -> anyhow::Result<()> {
        let changed = self.connect()?.execute(
            "UPDATE files SET local_mode = ?1 WHERE remote_id = ?2",
            params![mode & 0o777, local_id],
        )?;
        anyhow::ensure!(changed == 1, "cannot chmod an unknown local item");
        Ok(())
    }

    /// Pending operations on this item or its ancestors must settle before
    /// uploading content at this path. Compare literal path prefixes, not LIKE.
    pub fn has_pending_metadata_at_or_above(&self, path: &str) -> anyhow::Result<bool> {
        Ok(self.connect()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM pending_metadata_operations op
             JOIN files f ON f.remote_id = op.local_id
             WHERE ?1 = f.path OR substr(?1, 1, length(f.path) + 1) = f.path || '/')",
            params![normalize_cloud_path(path)],
            |row| row.get(0),
        )?)
    }

    pub fn move_subtree_and_queue(&self, local_id: &str, new_path: &str) -> anyhow::Result<()> {
        let record = self
            .get_by_remote_id(local_id)?
            .ok_or_else(|| anyhow::anyhow!("cannot move unknown item {local_id}"))?;
        let old_path = record.metadata.path.clone();
        let new_path = normalize_cloud_path(new_path);
        let descendants = self.list_descendants(&old_path)?;
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE files SET path = ?1, parent_path = ?2, name = ?3, pin_inheritance_blocked = 0, state = CASE WHEN is_dir = 0 AND ?5 AND cache_path IS NOT NULL THEN 'local_only' WHEN state = 'local_only' THEN 'dirty' WHEN state = 'error' AND cloud_remote_id IS NULL AND cache_path IS NOT NULL THEN 'dirty' ELSE state END WHERE remote_id = ?4",
            params![new_path, parent_cloud_path(&new_path), cloud_name(&new_path), local_id, self.excludes_file_name(&cloud_name(&new_path))],
        )?;
        for descendant in descendants {
            let suffix = descendant
                .metadata
                .path
                .strip_prefix(&old_path)
                .ok_or_else(|| anyhow::anyhow!("descendant path lost its parent prefix"))?;
            let descendant_path = normalize_cloud_path(&format!("{new_path}{suffix}"));
            tx.execute(
                "UPDATE files SET path = ?1, parent_path = ?2, name = ?3, pin_inheritance_blocked = 0, state = CASE WHEN is_dir = 0 AND ?5 AND cache_path IS NOT NULL THEN 'local_only' WHEN state = 'local_only' THEN 'dirty' WHEN state = 'error' AND cloud_remote_id IS NULL AND cache_path IS NOT NULL THEN 'dirty' ELSE state END WHERE remote_id = ?4",
                params![
                    descendant_path,
                    parent_cloud_path(&descendant_path),
                    cloud_name(&descendant_path),
                    descendant.metadata.remote_id,
                    self.excludes_file_name(&cloud_name(&descendant_path))
                ],
            )?;
        }
        // Descendant creates and moves are durable jobs too: replay them at
        // their new paths after the parent has settled.
        tx.execute(
            "UPDATE pending_metadata_operations SET path =
                (SELECT path FROM files WHERE remote_id = local_id)
             WHERE local_id IN (SELECT remote_id FROM files
                WHERE substr(path, 1, length(?1) + 1) = ?1 || '/')",
            params![new_path],
        )?;
        if record.cloud_remote_id.is_some()
            && (record.metadata.is_dir || !self.excludes_file_name(&cloud_name(&new_path)))
        {
            tx.execute(
                r#"
                INSERT INTO pending_metadata_operations(local_id, kind, path, queued_unix)
                VALUES (?1, 'move', ?2, ?3)
                ON CONFLICT(local_id) DO UPDATE SET
                    kind = CASE
                        WHEN pending_metadata_operations.kind = 'create_folder' THEN 'create_folder'
                        ELSE 'move'
                    END,
                    path = excluded.path,
                    queued_unix = excluded.queued_unix
                "#,
                params![local_id, new_path, now_unix()],
            )?;
        } else if record.metadata.is_dir {
            tx.execute(
                "UPDATE pending_metadata_operations SET path = ?1, queued_unix = ?2 WHERE local_id = ?3",
                params![new_path, now_unix(), local_id],
            )?;
        }
        tx.commit()?;
        self.recompute_pin_inheritance()
    }

    pub fn replace_file_locally(
        &self,
        source_local_id: &str,
        target_local_id: &str,
        new_path: &str,
    ) -> anyhow::Result<FileRecord> {
        let new_path = normalize_cloud_path(new_path);
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Read both identities under the write transaction: a delta or upload
        // acknowledgement must not change the replacement preconditions.
        let source = self
            .query_record(&tx,
                "SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id, local_mode FROM files WHERE remote_id = ?1",
                params![source_local_id])?
            .ok_or_else(|| anyhow::anyhow!("replacement source disappeared"))?;
        let target = self
            .query_record(&tx,
                "SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id, local_mode FROM files WHERE remote_id = ?1",
                params![target_local_id])?
            .ok_or_else(|| anyhow::anyhow!("replacement target disappeared"))?;
        if source.metadata.is_dir || target.metadata.is_dir {
            anyhow::bail!("cannot replace a directory as a file");
        }
        anyhow::ensure!(target.metadata.path == new_path, "replacement target moved");
        if source_local_id == target_local_id {
            // Preserve content, state, upload claims and pending metadata jobs.
            return Ok(target);
        }
        // An atomic save replaces the target's bytes, not its cloud identity or
        // destination. Keep its pending rename; only the source job is obsolete.
        tx.execute(
            "DELETE FROM pending_metadata_operations WHERE local_id = ?1",
            params![source_local_id],
        )?;
        if !self.excludes_file_name(&cloud_name(&new_path))
            && let Some(source_cloud_id) = source
                .cloud_remote_id
                .as_deref()
                .filter(|source_id| Some(*source_id) != target.cloud_remote_id.as_deref())
        {
            tx.execute(
                r#"
                INSERT INTO pending_deletes(remote_id, path, cache_path, queued_unix)
                VALUES (?1, ?2, NULL, ?3)
                ON CONFLICT(remote_id) DO UPDATE SET path = excluded.path, queued_unix = excluded.queued_unix
                "#,
                params![source_cloud_id, source.metadata.path, now_unix()],
            )?;
        }
        let removed = tx.execute(
            "DELETE FROM files WHERE remote_id = ?1",
            params![source_local_id],
        )?;
        anyhow::ensure!(removed == 1, "replacement source was not removed");
        if target.cache_path != source.cache_path
            && let Some(path) = &target.cache_path
        {
            tx.execute(
                "INSERT OR IGNORE INTO retired_cache(cache_path) VALUES (?1)",
                params![path.to_string_lossy()],
            )?;
        }
        let updated = tx.execute(
            r#"
            UPDATE files
            SET path = ?1,
                parent_path = ?2,
                name = ?3,
                size = ?4,
                modified_unix = ?5,
                state = CASE WHEN ?11 THEN 'local_only' WHEN ?6 = 'writing' THEN 'writing' ELSE 'dirty' END,
                cache_path = ?7,
                cache_accessed_unix = ?8,
                local_mode = ?10
            WHERE remote_id = ?9
            "#,
            params![
                new_path,
                parent_cloud_path(&new_path),
                cloud_name(&new_path),
                i64::try_from(source.metadata.size).unwrap_or(i64::MAX),
                source.metadata.modified_unix,
                source.state.as_str(),
                source
                    .cache_path
                    .as_ref()
                    .map(|path| path.to_string_lossy()),
                now_unix(),
                target_local_id,
                source.local_mode,
                self.excludes_file_name(&cloud_name(&new_path)),
            ],
        )?;
        anyhow::ensure!(updated == 1, "replacement target was not updated");
        tx.commit()?;
        self.recompute_pin_inheritance()?;
        self.get_by_remote_id(target_local_id)?
            .ok_or_else(|| anyhow::anyhow!("local replacement disappeared"))
    }

    pub fn pending_metadata_operation(
        &self,
        local_id: &str,
    ) -> anyhow::Result<Option<PendingMetadataOperation>> {
        let conn = self.connect()?;
        Ok(conn
            .query_row(
                "SELECT local_id, kind, path, queued_unix FROM pending_metadata_operations WHERE local_id = ?1",
                params![local_id],
                |row| {
                    let kind: String = row.get(1)?;
                    let kind = match kind.as_str() {
                        "create_folder" => PendingMetadataKind::CreateFolder,
                        "move" => PendingMetadataKind::Move,
                        _ => return Err(rusqlite::Error::InvalidQuery),
                    };
                    Ok(PendingMetadataOperation {
                        local_id: row.get(0)?,
                        kind,
                        path: row.get(2)?,
                        queued_unix: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn pending_metadata_operations(&self) -> anyhow::Result<Vec<PendingMetadataOperation>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT local_id FROM pending_metadata_operations ORDER BY queued_unix, local_id",
        )?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        ids.into_iter()
            .filter_map(|local_id| self.pending_metadata_operation(&local_id).transpose())
            .collect()
    }

    pub fn complete_metadata_operation(
        &self,
        local_id: &str,
        expected_path: &str,
        entry: &MetadataEntry,
    ) -> anyhow::Result<bool> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_path = tx
            .query_row(
                "SELECT path FROM files WHERE remote_id = ?1",
                params![local_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let still_current = current_path.as_deref() == Some(&normalize_cloud_path(expected_path));
        tx.execute(
            "UPDATE files SET cloud_remote_id = ?1, etag = ?2, modified_unix = CASE WHEN is_dir = 1 THEN ?3 ELSE modified_unix END, state = CASE WHEN ?4 = 1 AND state = 'dirty' THEN 'cached' ELSE state END WHERE remote_id = ?5",
            params![
                entry.remote_id,
                entry.etag,
                entry.modified_unix,
                entry.is_dir as i64,
                local_id
            ],
        )?;
        if still_current {
            tx.execute(
                "DELETE FROM pending_metadata_operations WHERE local_id = ?1 AND path = ?2",
                params![local_id, normalize_cloud_path(expected_path)],
            )?;
        }
        tx.commit()?;
        Ok(still_current)
    }

    // Re-read under a write transaction: the caller's snapshot may predate a save.
    pub fn remove_by_remote_id(&self, remote_id: &str) -> anyhow::Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM pending_metadata_operations WHERE local_id = ?1",
            params![remote_id],
        )?;
        tx.execute("DELETE FROM files WHERE remote_id = ?1", params![remote_id])?;
        tx.commit()?;
        self.recompute_pin_inheritance()
    }

    /// Return false when a cloud identity or transfer claim requires a tombstone.
    pub fn remove_local_only_file(&self, local_id: &str) -> anyhow::Result<bool> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let safe: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM files
            WHERE remote_id = ?1 AND state = 'local_only' AND cloud_remote_id IS NULL AND upload_path IS NULL)", params![local_id], |row| row.get(0))?;
        if !safe {
            return Ok(false);
        }
        tx.execute(
            "INSERT OR IGNORE INTO retired_cache(cache_path)
            SELECT cache_path FROM files WHERE remote_id = ?1 AND cache_path IS NOT NULL",
            params![local_id],
        )?;
        let changed = tx.execute(
            "DELETE FROM files WHERE remote_id = ?1 AND state = 'local_only'",
            params![local_id],
        )?;
        anyhow::ensure!(changed == 1, "local-only file changed during unlink");
        tx.execute(
            "DELETE FROM pending_metadata_operations WHERE local_id = ?1",
            params![local_id],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn queue_pending_delete(&self, record: &FileRecord) -> anyhow::Result<PendingDelete> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Upload acknowledgement may have bound a cloud ID since the FUSE snapshot.
        // Claiming an upload and hiding a file must serialize in SQLite as well.
        let record = self.query_record(&tx,
            "SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id, local_mode FROM files WHERE remote_id = ?1",
            params![record.metadata.remote_id])?
            .ok_or_else(|| anyhow::anyhow!("cannot delete an unknown local item"))?;
        let (upload_path, upload_target_id, upload_cache_path): (Option<String>, Option<String>, Option<String>) = tx.query_row(
            "SELECT upload_path, upload_target_id, upload_cache_path FROM files WHERE remote_id = ?1",
            params![record.metadata.remote_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        // Legacy interrupted Uploading records predate the durable transfer claim.
        let upload_target_id = if upload_path.is_none() && record.state == FileState::Uploading {
            record.cloud_remote_id.clone()
        } else {
            upload_target_id
        };
        let upload_path = upload_path.or_else(|| {
            (record.state == FileState::Uploading).then(|| record.metadata.path.clone())
        });
        let upload_cache_path = upload_cache_path
            .map(PathBuf::from)
            .or_else(|| upload_path.as_ref().and(record.cache_path.clone()));
        let queued_unix = now_unix();
        tx.execute(
            "DELETE FROM pending_metadata_operations WHERE local_id = ?1",
            params![record.metadata.remote_id],
        )?;
        tx.execute(
            r#"
            INSERT INTO pending_deletes(remote_id, path, cache_path, queued_unix, local_id,
                cloud_remote_id, upload_path, upload_target_id, upload_cache_path)
            VALUES (?1, ?2, ?3, ?4, ?1, ?5, ?6, ?7, ?8)
            ON CONFLICT(remote_id) DO UPDATE SET
                path = excluded.path,
                cache_path = excluded.cache_path,
                queued_unix = excluded.queued_unix,
                local_id = excluded.local_id,
                cloud_remote_id = excluded.cloud_remote_id,
                upload_path = excluded.upload_path,
                upload_target_id = excluded.upload_target_id,
                upload_cache_path = excluded.upload_cache_path
            "#,
            params![
                record.metadata.remote_id,
                record.metadata.path,
                record
                    .cache_path
                    .as_ref()
                    .map(|path| path.to_string_lossy()),
                queued_unix,
                record.cloud_remote_id,
                upload_path,
                upload_target_id,
                upload_cache_path
                    .as_ref()
                    .map(|path| path.to_string_lossy()),
            ],
        )?;
        if record.metadata.is_dir {
            tx.execute(
                r#"
                DELETE FROM files
                WHERE remote_id = ?1
                   OR path = ?2
                   OR substr(path, 1, length(?2) + 1) = ?2 || '/'
                "#,
                params![record.metadata.remote_id, record.metadata.path],
            )?;
        } else {
            tx.execute(
                "DELETE FROM files WHERE remote_id = ?1",
                params![record.metadata.remote_id],
            )?;
        }
        tx.commit()?;
        self.recompute_pin_inheritance()?;
        Ok(PendingDelete {
            remote_id: record.metadata.remote_id.clone(),
            local_id: Some(record.metadata.remote_id),
            cloud_remote_id: record.cloud_remote_id,
            path: record.metadata.path,
            cache_path: record.cache_path,
            queued_unix,
            upload_path,
            upload_target_id,
            upload_cache_path,
            uploaded_path: None,
            attempt_count: 0,
            next_retry_unix: 0,
            last_status: None,
            failure_kind: None,
        })
    }

    pub fn queue_remote_delete(&self, cloud_remote_id: &str, path: &str) -> anyhow::Result<()> {
        // Cleanup of an obsolete upload acknowledgement is not a user unlink.
        // A live record may already have adopted the returned cloud identity.
        let conn = self.connect()?;
        conn.execute(
            r#"
            INSERT INTO pending_deletes(remote_id, cloud_remote_id, path, cache_path, queued_unix)
            SELECT ?1, ?1, ?2, NULL, ?3
            WHERE NOT EXISTS (SELECT 1 FROM files WHERE cloud_remote_id = ?1)
            ON CONFLICT(remote_id) DO UPDATE SET cloud_remote_id = excluded.cloud_remote_id,
                path = excluded.path, queued_unix = excluded.queued_unix
            "#,
            params![cloud_remote_id, normalize_cloud_path(path), now_unix()],
        )?;
        Ok(())
    }

    pub fn pending_deletes(&self) -> anyhow::Result<Vec<PendingDelete>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT remote_id, path, cache_path, queued_unix, local_id,
                COALESCE(cloud_remote_id, CASE WHEN local_id IS NULL AND remote_id NOT LIKE 'local-upload-%' THEN remote_id END),
                upload_path, upload_target_id, upload_cache_path, uploaded_path, attempt_count, next_retry_unix, last_status, failure_kind FROM pending_deletes ORDER BY queued_unix, path",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(PendingDelete {
                    remote_id: row.get(0)?,
                    path: row.get(1)?,
                    cache_path: row.get::<_, Option<String>>(2)?.map(PathBuf::from),
                    queued_unix: row.get(3)?,
                    local_id: row.get(4)?,
                    cloud_remote_id: row.get(5)?,
                    upload_path: row.get(6)?,
                    upload_target_id: row.get(7)?,
                    upload_cache_path: row.get::<_, Option<String>>(8)?.map(PathBuf::from),
                    uploaded_path: row.get(9)?,
                    attempt_count: row.get(10)?,
                    next_retry_unix: row.get(11)?,
                    last_status: row.get(12)?,
                    failure_kind: row
                        .get::<_, Option<String>>(13)?
                        .as_deref()
                        .map(crate::DeleteFailureKind::parse),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn remove_pending_delete(&self, remote_id: &str) -> anyhow::Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "DELETE FROM pending_deletes WHERE remote_id = ?1",
            params![remote_id],
        )?;
        Ok(())
    }

    /// Call only after the remote deletion succeeded. Unlike cancellation of a
    /// stale cleanup job, completion leaves durable identity evidence: a delayed
    /// upload receipt may still refer to this now-absent object.
    pub fn complete_pending_delete(&self, remote_id: &str) -> anyhow::Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO completed_cloud_deletes(cloud_remote_id,completed_unix)
             SELECT cloud_remote_id,?2 FROM pending_deletes
             WHERE remote_id=?1 AND cloud_remote_id IS NOT NULL",
            params![remote_id, now_unix()],
        )?;
        tx.execute(
            "DELETE FROM pending_deletes WHERE remote_id=?1",
            params![remote_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn is_pending_delete(&self, remote_id: &str, path: &str) -> anyhow::Result<bool> {
        let conn = self.connect()?;
        let path = normalize_cloud_path(path);
        Ok(conn
            .query_row(
                "SELECT 1 FROM pending_deletes WHERE remote_id = ?1 OR cloud_remote_id = ?1 OR path = ?2 OR upload_path = ?2 OR uploaded_path = ?2 LIMIT 1",
                params![remote_id, path],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn pending_delete_for_local_id(
        &self,
        local_id: &str,
    ) -> anyhow::Result<Option<PendingDelete>> {
        Ok(self
            .pending_deletes()?
            .into_iter()
            .find(|delete| delete.local_id.as_deref() == Some(local_id)))
    }

    /// A late acknowledgement belongs to the deleted generation, never to a new
    /// file at the same path. Bind it to the tombstone in a single transaction.
    pub fn complete_deleted_upload(
        &self,
        local_id: &str,
        uploaded: &MetadataEntry,
    ) -> anyhow::Result<bool> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let cloud_id: Option<Option<String>> = tx
            .query_row(
                "SELECT cloud_remote_id FROM pending_deletes WHERE local_id = ?1",
                params![local_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(cloud_id) = cloud_id else {
            return Ok(false);
        };
        if cloud_id
            .as_deref()
            .is_some_and(|id| id != uploaded.remote_id)
        {
            tx.execute(
                "INSERT INTO pending_deletes(remote_id, cloud_remote_id, path, queued_unix)
                 VALUES (?1, ?1, ?2, ?3) ON CONFLICT(remote_id) DO NOTHING",
                params![uploaded.remote_id, uploaded.path, now_unix()],
            )?;
        } else {
            tx.execute(
                "UPDATE pending_deletes SET cloud_remote_id = ?2 WHERE local_id = ?1",
                params![local_id, uploaded.remote_id],
            )?;
        }
        tx.execute("UPDATE pending_deletes SET upload_path = NULL, upload_target_id = NULL, uploaded_path = ?2 WHERE local_id = ?1",
            params![local_id, uploaded.path])?;
        tx.commit()?;
        Ok(true)
    }

    pub fn begin_conflict_upload(
        &self,
        record: &FileRecord,
        path: &str,
        cache_path: &Path,
    ) -> anyhow::Result<bool> {
        if self.excludes_upload(record) {
            return Ok(false);
        }
        Ok(self.connect()?.execute(
            "UPDATE files SET upload_path = ?2, upload_target_id = NULL, upload_cache_path = ?3
             WHERE remote_id = ?1 AND state = 'conflict' AND path = ?4
                 AND cache_path IS ?3 AND etag = ?5",
            params![
                record.metadata.remote_id,
                path,
                cache_path.to_string_lossy(),
                record.metadata.path,
                record.metadata.etag
            ],
        )? == 1)
    }
}
