use super::Database;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::path::Path;

use crate::paths::{cloud_name, normalize_cloud_path, parent_cloud_path};
use crate::{FileRecord, FileState, MetadataEntry, now_unix};

impl Database {
    pub fn upsert_metadata(&self, entry: &MetadataEntry) -> anyhow::Result<()> {
        self.upsert_metadata_inner(entry, true)
    }

    pub fn upsert_metadata_batch<'a>(
        &self,
        entries: impl IntoIterator<Item = &'a MetadataEntry>,
    ) -> anyhow::Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for entry in entries {
            let pending_delete = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM pending_deletes WHERE remote_id = ?1 OR path = ?2)",
                params![entry.remote_id, normalize_cloud_path(&entry.path)],
                |row| row.get::<_, bool>(0),
            )?;
            if pending_delete {
                continue;
            }

            let local_id = tx
                .query_row(
                    "SELECT remote_id FROM files WHERE cloud_remote_id = ?1",
                    params![entry.remote_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .unwrap_or_else(|| entry.remote_id.clone());
            let pending_metadata = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM pending_metadata_operations op
                 JOIN files ancestor ON ancestor.remote_id = op.local_id
                 WHERE op.local_id = ?1
                    OR ?2 = ancestor.path
                    OR substr(?2, 1, length(ancestor.path) + 1) = ancestor.path || '/'
                    OR EXISTS(SELECT 1 FROM files current WHERE current.remote_id = ?1
                        AND substr(current.path, 1, length(ancestor.path) + 1) = ancestor.path || '/'))",
                params![local_id, entry.path],
                |row| row.get::<_, bool>(0),
            )?;
            if pending_metadata {
                continue;
            }

            let protected_path = tx.query_row(
                r#"
                SELECT EXISTS(
                    SELECT 1 FROM files
                    WHERE path = ?1
                      AND remote_id <> ?2
                      AND state IN ('writing', 'dirty', 'uploading')
                )
                "#,
                params![entry.path, local_id],
                |row| row.get::<_, bool>(0),
            )?;
            let protected_id = tx.query_row(
                r#"
                SELECT EXISTS(
                    SELECT 1 FROM files
                    WHERE remote_id = ?1
                      AND state IN ('writing', 'dirty', 'uploading')
                )
                "#,
                params![local_id],
                |row| row.get::<_, bool>(0),
            )?;
            if protected_path || protected_id {
                continue;
            }

            let old_path = tx
                .query_row(
                    "SELECT path FROM files WHERE remote_id = ?1",
                    params![local_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            tx.execute(
                "DELETE FROM files WHERE path = ?1 AND remote_id <> ?2",
                params![entry.path, local_id],
            )?;
            tx.execute(
                r#"
                INSERT INTO files (
                    remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state,
                    cloud_remote_id,
                    pin_inheritance_blocked
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1)
                ON CONFLICT(remote_id) DO UPDATE SET
                    path = excluded.path,
                    parent_path = excluded.parent_path,
                    name = excluded.name,
                    is_dir = excluded.is_dir,
                    size = excluded.size,
                    modified_unix = CASE
                        WHEN files.is_dir = 0 AND files.etag <> ''
                             AND files.etag = excluded.etag AND files.size = excluded.size
                        THEN files.modified_unix
                        ELSE excluded.modified_unix
                    END,
                    etag = excluded.etag,
                    cloud_remote_id = excluded.cloud_remote_id
                "#,
                params![
                    local_id,
                    entry.path,
                    entry.parent_path,
                    entry.name,
                    entry.is_dir as i64,
                    entry.size as i64,
                    entry.modified_unix,
                    entry.etag,
                    FileState::OnlineOnly.as_str(),
                    entry.remote_id,
                ],
            )?;

            if let Some(old_path) = old_path.filter(|old_path| old_path != &entry.path) {
                let descendants = {
                    let mut stmt = tx.prepare(
                        r#"
                        SELECT remote_id, path FROM files
                        WHERE (
                            ?1 = '/' AND path <> '/' AND substr(path, 1, 1) = '/'
                        ) OR (
                            ?1 <> '/' AND substr(path, 1, length(?1) + 1) = ?1 || '/'
                        )
                        ORDER BY path
                        "#,
                    )?;
                    stmt.query_map(params![old_path], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?
                };
                for (remote_id, old_descendant_path) in descendants {
                    let suffix = old_descendant_path
                        .strip_prefix(&old_path)
                        .ok_or_else(|| anyhow::anyhow!("descendant path lost its parent prefix"))?;
                    let path = normalize_cloud_path(&format!("{}{suffix}", entry.path));
                    tx.execute(
                        "UPDATE files SET path = ?1, parent_path = ?2, name = ?3 WHERE remote_id = ?4",
                        params![path, parent_cloud_path(&path), cloud_name(&path), remote_id],
                    )?;
                }
            }
        }
        tx.commit()?;
        self.recompute_pin_inheritance()
    }

    pub(super) fn upsert_metadata_inner(
        &self,
        entry: &MetadataEntry,
        recompute_pins: bool,
    ) -> anyhow::Result<()> {
        if self.is_pending_delete(&entry.remote_id, &entry.path)? {
            return Ok(());
        }
        let incoming_cloud_id =
            (!entry.remote_id.starts_with("local-upload-")).then_some(entry.remote_id.clone());
        let existing = match incoming_cloud_id.as_deref() {
            Some(cloud_remote_id) => self.get_by_cloud_remote_id(cloud_remote_id)?,
            None => self.get_by_remote_id(&entry.remote_id)?,
        };
        let local_id = existing
            .as_ref()
            .map(|record| record.metadata.remote_id.clone())
            .unwrap_or_else(|| entry.remote_id.clone());
        if self.has_pending_metadata_at_or_above(&entry.path)?
            || self.has_pending_metadata_at_or_above(
                existing
                    .as_ref()
                    .map(|record| record.metadata.path.as_str())
                    .unwrap_or(&entry.path),
            )?
        {
            return Ok(());
        }
        if self.get_by_path(&entry.path)?.is_some_and(|record| {
            record.metadata.remote_id != local_id
                && matches!(
                    record.state,
                    FileState::Writing | FileState::Dirty | FileState::Uploading
                )
        }) {
            return Ok(());
        }
        if self.get_by_remote_id(&local_id)?.is_some_and(|record| {
            matches!(
                record.state,
                FileState::Writing | FileState::Dirty | FileState::Uploading
            )
        }) {
            if recompute_pins {
                self.recompute_pin_inheritance()?;
            }
            return Ok(());
        }
        let old_path = self
            .get_by_remote_id(&local_id)?
            .map(|record| record.metadata.path);
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM files WHERE path = ?1 AND remote_id <> ?2",
            params![entry.path, local_id],
        )?;
        tx.execute(
            r#"
            INSERT INTO files (
                remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state,
                cloud_remote_id
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(remote_id) DO UPDATE SET
                path = excluded.path,
                parent_path = excluded.parent_path,
                name = excluded.name,
                is_dir = excluded.is_dir,
                size = excluded.size,
                modified_unix = CASE
                        WHEN files.is_dir = 0 AND files.etag <> ''
                             AND files.etag = excluded.etag AND files.size = excluded.size
                        THEN files.modified_unix
                        ELSE excluded.modified_unix
                    END,
                etag = excluded.etag,
                cloud_remote_id = excluded.cloud_remote_id
            "#,
            params![
                local_id,
                entry.path,
                entry.parent_path,
                entry.name,
                entry.is_dir as i64,
                entry.size as i64,
                entry.modified_unix,
                entry.etag,
                FileState::OnlineOnly.as_str(),
                incoming_cloud_id,
            ],
        )?;
        tx.commit()?;
        if let Some(old_path) = old_path.filter(|old_path| old_path != &entry.path) {
            self.move_descendants(&old_path, &entry.path)?;
        }
        if recompute_pins {
            self.recompute_pin_inheritance()?;
        }
        Ok(())
    }

    pub fn bind_cloud_identity(
        &self,
        local_id: &str,
        cloud_remote_id: &str,
        etag: &str,
    ) -> anyhow::Result<()> {
        let conn = self.connect()?;
        let changed = conn.execute(
            "UPDATE files SET cloud_remote_id = ?1, etag = ?2 WHERE remote_id = ?3",
            params![cloud_remote_id, etag, local_id],
        )?;
        if changed == 0 {
            anyhow::bail!("cannot bind cloud identity for unknown item {local_id}");
        }
        Ok(())
    }

    pub fn commit_uploaded(
        &self,
        local_id: &str,
        requested_path: &str,
        uploaded: &MetadataEntry,
        cache_path: &Path,
    ) -> anyhow::Result<FileRecord> {
        // Upload acknowledgement changes the cloud version, not the local
        // content's mtime. Readers such as Poppler reject saves if that mtime
        // changes after open. Matching delta echoes must preserve it as well.
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = self
            .query_record(
                &tx,
                "SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id, local_mode FROM files WHERE remote_id = ?1",
                params![local_id],
            )?
            .ok_or_else(|| anyhow::anyhow!("uploaded local item disappeared: {local_id}"))?;
        let is_current_generation = current.metadata.path == normalize_cloud_path(requested_path)
            && current.state == FileState::Uploading
            && current.cache_path.as_deref() == Some(cache_path);
        let cloud_identity_matches = current
            .cloud_remote_id
            .as_deref()
            .is_none_or(|remote_id| remote_id == uploaded.remote_id);
        let needs_cloud_move = current.metadata.path != normalize_cloud_path(requested_path)
            && current.cloud_remote_id.is_none();
        tx.execute(
            r#"
            UPDATE files
            SET cloud_remote_id = CASE
                    WHEN ?3 = 1 OR cloud_remote_id IS NULL THEN ?1
                    ELSE cloud_remote_id
                END,
                etag = CASE WHEN ?9 = 1 THEN ?2 ELSE etag END,
                modified_unix = CASE WHEN ?3 = 1 THEN ?4 ELSE modified_unix END,
                size = CASE WHEN ?3 = 1 THEN ?5 ELSE size END,
                state = CASE
                    WHEN ?3 = 0 THEN state
                    WHEN pin_explicit = 1 OR pin_origin_remote_id IS NOT NULL THEN 'pinned'
                    ELSE 'cached'
                END,
                cache_path = CASE WHEN ?3 = 1 THEN ?6 ELSE cache_path END,
                cache_accessed_unix = CASE WHEN ?3 = 1 THEN ?7 ELSE cache_accessed_unix END
            WHERE remote_id = ?8
            "#,
            params![
                uploaded.remote_id,
                uploaded.etag,
                is_current_generation as i64,
                current.metadata.modified_unix,
                i64::try_from(uploaded.size).unwrap_or(i64::MAX),
                cache_path.to_string_lossy(),
                now_unix(),
                local_id,
                cloud_identity_matches as i64,
            ],
        )?;
        if needs_cloud_move {
            tx.execute(
                r#"
                INSERT INTO pending_metadata_operations(local_id, kind, path, queued_unix)
                VALUES (?1, 'move', ?2, ?3)
                ON CONFLICT(local_id) DO UPDATE SET
                    kind = 'move',
                    path = excluded.path,
                    queued_unix = excluded.queued_unix
                "#,
                params![local_id, current.metadata.path, now_unix()],
            )?;
        }
        tx.commit()?;
        self.get_by_remote_id(local_id)?
            .ok_or_else(|| anyhow::anyhow!("committed upload disappeared: {local_id}"))
    }
}
