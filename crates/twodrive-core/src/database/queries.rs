use super::{Database, row_to_record};
use rusqlite::{OptionalExtension, params};

use crate::FileRecord;
use crate::paths::normalize_cloud_path;

impl Database {
    pub fn get_by_path(&self, path: &str) -> anyhow::Result<Option<FileRecord>> {
        let conn = self.connect()?;
        let normalized = normalize_cloud_path(path);
        self.query_record(
            &conn,
            "SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id, local_mode FROM files WHERE path = ?1",
            params![normalized],
        )
    }

    pub fn get_by_remote_id(&self, remote_id: &str) -> anyhow::Result<Option<FileRecord>> {
        let conn = self.connect()?;
        self.query_record(
            &conn,
            "SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id, local_mode FROM files WHERE remote_id = ?1",
            params![remote_id],
        )
    }

    pub fn get_by_cloud_remote_id(
        &self,
        cloud_remote_id: &str,
    ) -> anyhow::Result<Option<FileRecord>> {
        let conn = self.connect()?;
        self.query_record(
            &conn,
            "SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id, local_mode FROM files WHERE cloud_remote_id = ?1",
            params![cloud_remote_id],
        )
    }

    pub fn list_children(&self, parent_path: &str) -> anyhow::Result<Vec<FileRecord>> {
        let conn = self.connect()?;
        let normalized = normalize_cloud_path(parent_path);
        let mut stmt = conn.prepare(
            r#"
            SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id, local_mode
            FROM files
            WHERE parent_path = ?1
            ORDER BY is_dir DESC, lower(name), name
            "#,
        )?;
        let records = stmt
            .query_map(params![normalized], row_to_record)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(records)
    }

    pub fn list_descendants(&self, path: &str) -> anyhow::Result<Vec<FileRecord>> {
        let normalized = normalize_cloud_path(path);
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id, local_mode
            FROM files
            WHERE (
                ?1 = '/' AND path <> '/' AND substr(path, 1, 1) = '/'
            ) OR (
                ?1 <> '/' AND substr(path, 1, length(?1) + 1) = ?1 || '/'
            )
            ORDER BY path
            "#,
        )?;
        let records = stmt
            .query_map(params![normalized], row_to_record)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(records)
    }

    pub fn all_records(&self) -> anyhow::Result<Vec<FileRecord>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id, local_mode
            FROM files
            ORDER BY path
            "#,
        )?;
        let records = stmt
            .query_map([], row_to_record)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(records)
    }

    pub fn get_state_value(&self, key: &str) -> anyhow::Result<Option<String>> {
        let conn = self.connect()?;
        Ok(conn
            .query_row(
                "SELECT value FROM app_state WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn set_state_value(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            INSERT INTO app_state(key, value)
            VALUES (?1, ?2)
            ON CONFLICT(key) DO UPDATE SET value = excluded.value
            "#,
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delta_link(&self) -> anyhow::Result<Option<String>> {
        self.get_state_value("onedrive_delta_link")
    }

    pub fn set_delta_link(&self, value: &str) -> anyhow::Result<()> {
        self.set_state_value("onedrive_delta_link", value)
    }
}
