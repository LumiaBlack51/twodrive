use super::Database;
use rusqlite::{TransactionBehavior, params};
use std::collections::HashMap;

use crate::paths::parent_cloud_path;

impl Database {
    pub fn mark_pinned(&self, remote_id: &str) -> anyhow::Result<()> {
        self.set_explicit_pin(remote_id, true)
    }

    pub fn mark_unpinned(&self, remote_id: &str) -> anyhow::Result<()> {
        self.set_explicit_pin(remote_id, false)
    }

    pub fn set_explicit_pin(&self, remote_id: &str, pinned: bool) -> anyhow::Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = self.query_record(
            &tx,
            "SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id, local_mode FROM files WHERE remote_id = ?1",
            params![remote_id],
        )?;
        let Some(record) = record else {
            anyhow::bail!("cannot change pin policy for unknown item {remote_id}");
        };
        let changed = tx.execute(
            r#"
            UPDATE files
            SET pin_explicit = ?1,
                pin_inheritance_blocked = CASE WHEN ?1 = 1 THEN 0 ELSE pin_inheritance_blocked END
            WHERE remote_id = ?2
            "#,
            params![pinned as i64, remote_id],
        )?;
        if changed == 0 {
            anyhow::bail!("cannot change pin policy for unknown item {remote_id}");
        }
        if pinned && record.metadata.is_dir {
            tx.execute(
                r#"
                UPDATE files
                SET pin_inheritance_blocked = 0
                WHERE substr(path, 1, length(?1) + 1) = ?1 || '/'
                "#,
                params![record.metadata.path],
            )?;
        }
        tx.commit()?;
        self.recompute_pin_inheritance()
    }

    pub fn recompute_pin_inheritance(&self) -> anyhow::Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut stmt = tx.prepare(
            "SELECT remote_id, path, pin_explicit, pin_inheritance_blocked FROM files ORDER BY length(path), path",
        )?;
        let records = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? != 0,
                    row.get::<_, i64>(3)? != 0,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);

        let explicit_roots = records
            .iter()
            .filter(|(_, _, explicit, _)| *explicit)
            .map(|(remote_id, path, _, _)| (path.clone(), remote_id.clone()))
            .collect::<HashMap<_, _>>();

        for (remote_id, path, explicit, inheritance_blocked) in records {
            let origin = if explicit || inheritance_blocked {
                None
            } else {
                let mut ancestor = parent_cloud_path(&path);
                let mut origin = None;
                loop {
                    if let Some(remote_id) = explicit_roots.get(&ancestor) {
                        origin = Some(remote_id.clone());
                        break;
                    }
                    if ancestor == "/" {
                        break;
                    }
                    ancestor = parent_cloud_path(&ancestor);
                }
                origin
            };
            let effective = explicit || origin.is_some();
            tx.execute(
                r#"
                UPDATE files
                SET pin_origin_remote_id = ?1,
                    release_pending = CASE WHEN ?2 = 1 THEN 0 ELSE release_pending END,
                    state = CASE
                        WHEN state IN ('writing', 'dirty', 'uploading', 'conflict', 'error') THEN state
                        WHEN ?2 = 1 AND is_dir = 1 THEN 'pinned'
                        WHEN ?2 = 1 AND cache_path IS NOT NULL THEN 'pinned'
                        WHEN ?2 = 1 THEN 'hydrating'
                        WHEN cache_path IS NOT NULL THEN 'cached'
                        ELSE 'online_only'
                    END
                WHERE remote_id = ?3
                "#,
                params![origin, effective as i64, remote_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn allow_pin_inheritance(&self, remote_id: &str) -> anyhow::Result<()> {
        let conn = self.connect()?;
        let record = self.query_record(
            &conn,
            "SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id, local_mode FROM files WHERE remote_id = ?1",
            params![remote_id],
        )?;
        let Some(record) = record else {
            anyhow::bail!("cannot change pin inheritance for unknown item {remote_id}");
        };
        conn.execute(
            r#"
            UPDATE files
            SET pin_inheritance_blocked = 0
            WHERE remote_id = ?1
               OR substr(path, 1, length(?2) + 1) = ?2 || '/'
            "#,
            params![remote_id, record.metadata.path],
        )?;
        drop(conn);
        self.recompute_pin_inheritance()
    }
}
