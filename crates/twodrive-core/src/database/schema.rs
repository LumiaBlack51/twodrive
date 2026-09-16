use super::Database;
use rusqlite::{Connection, OptionalExtension};
use std::fs;
use std::sync::{Mutex, OnceLock};

impl Database {
    pub fn init(&self) -> anyhow::Result<()> {
        let _migration_guard = database_init_lock()
            .lock()
            .map_err(|_| anyhow::anyhow!("database initialization lock is poisoned"))?;
        if let Some(parent) = self.db_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let conn = self.connect()?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS files (
                remote_id TEXT PRIMARY KEY,
                cloud_remote_id TEXT,
                path TEXT NOT NULL UNIQUE,
                parent_path TEXT NOT NULL,
                name TEXT NOT NULL,
                is_dir INTEGER NOT NULL,
                size INTEGER NOT NULL,
                modified_unix INTEGER NOT NULL,
                etag TEXT NOT NULL,
                state TEXT NOT NULL,
                cache_path TEXT,
                cache_accessed_unix INTEGER,
                pin_explicit INTEGER NOT NULL DEFAULT 0,
                pin_origin_remote_id TEXT,
                pin_inheritance_blocked INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_files_parent_path ON files(parent_path);
            CREATE TABLE IF NOT EXISTS app_state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS pending_deletes (
                remote_id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                cache_path TEXT,
                queued_unix INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS pending_metadata_operations (
                local_id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                path TEXT NOT NULL,
                queued_unix INTEGER NOT NULL
            );
            "#,
        )?;
        ensure_column(
            &conn,
            "files",
            "cloud_remote_id",
            "ALTER TABLE files ADD COLUMN cloud_remote_id TEXT",
        )?;
        conn.execute(
            "UPDATE files SET cloud_remote_id = remote_id WHERE cloud_remote_id IS NULL AND remote_id NOT LIKE 'local-upload-%'",
            [],
        )?;
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_files_cloud_remote_id ON files(cloud_remote_id) WHERE cloud_remote_id IS NOT NULL",
            [],
        )?;
        ensure_column(
            &conn,
            "files",
            "cache_accessed_unix",
            "ALTER TABLE files ADD COLUMN cache_accessed_unix INTEGER",
        )?;
        ensure_column(
            &conn,
            "files",
            "pin_explicit",
            "ALTER TABLE files ADD COLUMN pin_explicit INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "files",
            "pin_origin_remote_id",
            "ALTER TABLE files ADD COLUMN pin_origin_remote_id TEXT",
        )?;
        ensure_column(
            &conn,
            "files",
            "pin_inheritance_blocked",
            "ALTER TABLE files ADD COLUMN pin_inheritance_blocked INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "files",
            "release_pending",
            "ALTER TABLE files ADD COLUMN release_pending INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "files",
            "download_generation",
            "ALTER TABLE files ADD COLUMN download_generation INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "files",
            "local_mode",
            "ALTER TABLE files ADD COLUMN local_mode INTEGER",
        )?;
        let migrated = conn
            .query_row(
                "SELECT value FROM app_state WHERE key = 'pin_policy_v1_migrated'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .is_some();
        if !migrated {
            conn.execute_batch(
                r#"
                UPDATE files AS child
                SET pin_explicit = 1
                WHERE child.state = 'pinned'
                  AND NOT EXISTS (
                    SELECT 1
                    FROM files AS parent
                    WHERE parent.path = child.parent_path
                      AND parent.state = 'pinned'
                  );
                INSERT INTO app_state(key, value)
                VALUES ('pin_policy_v1_migrated', '1')
                ON CONFLICT(key) DO UPDATE SET value = excluded.value;
                "#,
            )?;
        }
        drop(conn);
        if !migrated {
            self.recompute_pin_inheritance()?;
        }
        Ok(())
    }
}
fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    alter_sql: &str,
) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !columns.iter().any(|name| name == column)
        && let Err(err) = conn.execute_batch(alter_sql)
    {
        let mut retry = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let now_exists = retry
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|name| name == column);
        if !now_exists {
            return Err(err.into());
        }
    }
    Ok(())
}

fn database_init_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}
