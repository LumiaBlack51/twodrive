use super::Database;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::path::Path;

use crate::paths::{cloud_name, normalize_cloud_path, parent_cloud_path};
use crate::{FileRecord, FileState, MetadataEntry, now_unix};

impl Database {
    /// Publish the acknowledged recovery copy before retiring only the exact
    /// conflicted generation. A concurrent new save/move must remain pending.
    pub fn complete_missing_conflict_upload(
        &self,
        original: &FileRecord,
        uploaded: &MetadataEntry,
        cache_path: &Path,
    ) -> anyhow::Result<bool> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let present = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM files WHERE remote_id = ?1)
             AND NOT EXISTS(SELECT 1 FROM pending_deletes WHERE local_id = ?1)",
            params![original.metadata.remote_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !present {
            return Ok(false);
        }
        let copy_id = tx
            .query_row(
                "SELECT remote_id FROM files WHERE cloud_remote_id = ?1",
                params![uploaded.remote_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_else(|| uploaded.remote_id.clone());
        anyhow::ensure!(
            copy_id != original.metadata.remote_id,
            "recovery copy reused the missing identity"
        );
        let copy_delete: Option<String> = tx.query_row(
            "SELECT remote_id FROM pending_deletes WHERE cloud_remote_id=?1 OR remote_id=?1 LIMIT 1",
            params![uploaded.remote_id], |row| row.get(0)).optional()?;
        if let Some(deleted_id) = copy_delete {
            // This is the COPY's deletion intent, not the original's. Never
            // recreate it or clear an unrelated newer upload on its tombstone.
            let settled = tx.execute(
                "UPDATE pending_deletes SET cache_path=COALESCE(cache_path,?2),
                uploaded_path=COALESCE(uploaded_path,?3) WHERE remote_id=?1",
                params![deleted_id, cache_path.to_string_lossy(), uploaded.path],
            )?;
            anyhow::ensure!(settled == 1, "recovery copy deletion settlement rejected");
        } else {
            let existing = self.query_record(&tx,
                "SELECT remote_id,path,parent_path,name,is_dir,size,modified_unix,etag,state,cache_path,cache_accessed_unix,pin_explicit,pin_origin_remote_id,pin_inheritance_blocked,cloud_remote_id,local_mode FROM files WHERE remote_id=?1",
                params![copy_id])?;
            if let Some(copy) = &existing {
                anyhow::ensure!(
                    !copy.metadata.is_dir
                        && copy.cloud_remote_id.as_deref() == Some(uploaded.remote_id.as_str()),
                    "recovery copy identity changed"
                );
                anyhow::ensure!(
                    matches!(
                        copy.state,
                        FileState::OnlineOnly
                            | FileState::Cached
                            | FileState::Pinned
                            | FileState::Hydrating
                    ),
                    "recovery copy has a protected local generation"
                );
            }
            let same_version = existing.as_ref().is_none_or(|copy| {
                copy.metadata.etag == uploaded.etag && copy.metadata.size == uploaded.size
            });
            if same_version {
                tx.execute(
                    "INSERT OR IGNORE INTO retired_cache(cache_path)
                     SELECT cache_path FROM files WHERE remote_id=?1
                       AND cache_path IS NOT NULL AND cache_path<>?2",
                    params![copy_id, cache_path.to_string_lossy()],
                )?;
                let published = tx.execute(
                    "INSERT INTO files(remote_id,cloud_remote_id,path,parent_path,name,
                        is_dir,size,modified_unix,etag,state,cache_path,cache_accessed_unix,local_mode)
                     VALUES(?1,?2,?3,?4,?5,0,?6,?7,?8,'cached',?9,?10,?11)
                     ON CONFLICT(remote_id) DO UPDATE SET
                        state=CASE WHEN files.pin_explicit=1 OR files.pin_origin_remote_id IS NOT NULL
                            THEN 'pinned' ELSE 'cached' END,
                        cache_path=excluded.cache_path,cache_accessed_unix=excluded.cache_accessed_unix,
                        download_generation=files.download_generation+1",
                    params![copy_id,uploaded.remote_id,uploaded.path,uploaded.parent_path,
                        uploaded.name,uploaded.size as i64,original.metadata.modified_unix,
                        uploaded.etag,cache_path.to_string_lossy(),now_unix(),original.local_mode])?;
                anyhow::ensure!(published == 1, "recovery copy publication rejected");
            }
            // A different observed version is opaque, not permission to roll it
            // back. Preserve its metadata/cache and retire only the acknowledged
            // original claim below. The unattached old cache remains recoverable.
        }
        tx.execute(
            "DELETE FROM files WHERE remote_id = ?1 AND state = 'conflict'
               AND cache_path IS ?2 AND cloud_remote_id IS ?3
               AND size = ?4 AND modified_unix = ?5 AND etag = ?6
               AND upload_path = ?7 AND upload_cache_path IS ?2 AND upload_target_id IS NULL
               AND NOT EXISTS(SELECT 1 FROM pending_metadata_operations WHERE local_id = ?1)",
            params![
                original.metadata.remote_id,
                cache_path.to_string_lossy(),
                original.cloud_remote_id,
                original.metadata.size as i64,
                original.metadata.modified_unix,
                original.metadata.etag,
                uploaded.path
            ],
        )?;
        // If a newer local generation kept this row, retire just the failed
        // generation's acknowledged claim, never its newer cache or state.
        tx.execute(
            "UPDATE files SET upload_path = NULL, upload_target_id = NULL, upload_cache_path = NULL
             WHERE remote_id = ?1 AND upload_path = ?2 AND upload_cache_path IS ?3
               AND upload_target_id IS NULL",
            params![
                original.metadata.remote_id,
                uploaded.path,
                cache_path.to_string_lossy()
            ],
        )?;
        tx.commit()?;
        self.recompute_pin_inheritance()?;
        Ok(true)
    }

    pub fn upsert_metadata(&self, entry: &MetadataEntry) -> anyhow::Result<()> {
        self.upsert_metadata_entries([entry], false)
    }

    pub fn upsert_metadata_batch<'a>(
        &self,
        entries: impl IntoIterator<Item = &'a MetadataEntry>,
    ) -> anyhow::Result<()> {
        self.upsert_metadata_entries(entries, true)
    }

    fn upsert_metadata_entries<'a>(
        &self,
        entries: impl IntoIterator<Item = &'a MetadataEntry>,
        block_new_pin_inheritance: bool,
    ) -> anyhow::Result<()> {
        // Keep identity resolution, local-generation protection and cache
        // invalidation in one writer transaction for both import paths.
        let _writer = self
            .metadata_write_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("metadata write lock is poisoned"))?;
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for entry in entries {
            let pending_delete = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM pending_deletes WHERE remote_id = ?1 OR cloud_remote_id = ?1 OR path = ?2 OR upload_path = ?2 OR uploaded_path = ?2)",
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
                      AND state IN ('writing', 'dirty', 'uploading', 'conflict')
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
                      AND state IN ('writing', 'dirty', 'uploading', 'conflict')
                )
                "#,
                params![local_id],
                |row| row.get::<_, bool>(0),
            )?;
            if protected_path || protected_id {
                continue;
            }

            // Detach and retire the old version atomically. Reclamation waits
            // for every durable reference and locked reader/upload to finish.
            let obsolete = tx
                .query_row(
                    "SELECT cache_path, size FROM files WHERE remote_id = ?1 AND is_dir = 0
                   AND cache_path IS NOT NULL AND state IN ('cached', 'pinned')
                   AND (etag <> ?2 OR size <> ?3 OR is_dir <> ?4)",
                    params![local_id, entry.etag, entry.size as i64, entry.is_dir as i64],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)),
                )
                .optional()?;
            if let Some((path, size)) = obsolete
                && std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && m.len() == size)
            {
                tx.execute(
                    "INSERT OR IGNORE INTO retired_cache(cache_path) VALUES (?1)",
                    params![path],
                )?;
            }
            tx.execute(
                "UPDATE files SET cache_path = NULL, cache_accessed_unix = NULL,
                    state = 'online_only', download_generation = download_generation + 1
                 WHERE remote_id = ?1 AND is_dir = 0
                   AND (etag <> ?2 OR size <> ?3 OR is_dir <> ?4)",
                params![local_id, entry.etag, entry.size as i64, entry.is_dir as i64],
            )?;
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
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
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
                    (!entry.remote_id.starts_with("local-upload-")).then_some(&entry.remote_id),
                    block_new_pin_inheritance as i64,
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
        if current.cloud_remote_id.is_none() {
            // A first PUT can be visible to delta before its acknowledgement.
            // If the user has already renamed the unbound local record, delta
            // may import a clean alias at the old name. Coalesce only an exact
            // server echo, never another edited/pinned/moving local identity.
            let alias: Option<(String, Option<String>)> = tx
                .query_row(
                    "SELECT remote_id, cache_path FROM files
                 WHERE cloud_remote_id = ?1 AND remote_id <> ?2 AND path = ?3
                   AND etag = ?4 AND size = ?5 AND is_dir = 0 AND pin_explicit = 0
                   AND state IN ('online_only', 'cached', 'pinned', 'hydrating')
                   AND NOT EXISTS(SELECT 1 FROM pending_metadata_operations op
                       WHERE op.local_id = files.remote_id)",
                    params![
                        uploaded.remote_id,
                        local_id,
                        normalize_cloud_path(requested_path),
                        uploaded.etag,
                        uploaded.size as i64
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((alias_id, cache)) = alias {
                if let Some(cache) = cache {
                    tx.execute(
                        "INSERT OR IGNORE INTO retired_cache(cache_path) VALUES (?1)",
                        params![cache],
                    )?;
                }
                tx.execute("DELETE FROM files WHERE remote_id = ?1", params![alias_id])?;
            }
        }
        tx.execute(
            r#"
            UPDATE files
            SET upload_path = NULL, upload_target_id = NULL, upload_cache_path = NULL,
                cloud_remote_id = CASE
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
