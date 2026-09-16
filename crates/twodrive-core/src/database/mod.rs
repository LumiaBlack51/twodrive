use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use crate::{FileRecord, FileState, MetadataEntry};

mod cache;
mod metadata;
mod mutations;
mod pins;
mod queries;
mod schema;

#[derive(Debug, Clone)]
pub struct Database {
    db_path: PathBuf,
}

impl Database {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.db_path
    }

    fn connect(&self) -> anyhow::Result<Connection> {
        let conn = Connection::open(&self.db_path)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        Ok(conn)
    }

    fn query_record<P: rusqlite::Params>(
        &self,
        conn: &Connection,
        sql: &str,
        params: P,
    ) -> anyhow::Result<Option<FileRecord>> {
        Ok(conn.query_row(sql, params, row_to_record).optional()?)
    }
}
fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileRecord> {
    let state_value: String = row.get(8)?;
    let cache_path: Option<String> = row.get(9)?;
    Ok(FileRecord {
        metadata: MetadataEntry {
            remote_id: row.get(0)?,
            path: row.get(1)?,
            parent_path: row.get(2)?,
            name: row.get(3)?,
            is_dir: row.get::<_, i64>(4)? != 0,
            size: row.get::<_, i64>(5)? as u64,
            modified_unix: row.get(6)?,
            etag: row.get(7)?,
        },
        cloud_remote_id: row.get(14)?,
        local_mode: row.get(15)?,
        state: FileState::from_str(&state_value).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(err))
        })?,
        cache_path: cache_path.map(PathBuf::from),
        cache_accessed_unix: row.get(10)?,
        pin_explicit: row.get::<_, i64>(11)? != 0,
        pin_origin_remote_id: row.get(12)?,
        pin_inheritance_blocked: row.get::<_, i64>(13)? != 0,
    })
}
