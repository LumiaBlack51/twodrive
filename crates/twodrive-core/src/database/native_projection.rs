//! Publication claims precede filesystem changes. A prepared claim alone never
//! authorizes hydration; completion binds its exact version to a physical file.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeProjection {
    pub local_id: String,
    pub root_binding: String,
    pub tag: Vec<u8>,
    pub cloud_id: String,
    pub path: String,
    pub etag: String,
    pub size: u64,
    pub modified_unix: i64,
    pub directory: bool,
    pub state: String,
    pub volume_serial: Option<u32>,
    pub native_file_id: Option<String>,
}
pub(super) fn init(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS local_native_projections(
        local_id TEXT PRIMARY KEY,root_binding TEXT NOT NULL,tag BLOB NOT NULL UNIQUE,
        cloud_id TEXT NOT NULL,path TEXT NOT NULL,etag TEXT NOT NULL,size INTEGER NOT NULL,
        modified_unix INTEGER NOT NULL,directory INTEGER NOT NULL,state TEXT NOT NULL,
        volume_serial INTEGER,native_file_id TEXT);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_native_projection_file
            ON local_native_projections(root_binding,volume_serial,native_file_id)
            WHERE native_file_id IS NOT NULL;",
    )?;
    Ok(())
}
fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NativeProjection> {
    Ok(NativeProjection {
        local_id: row.get(0)?,
        root_binding: row.get(1)?,
        tag: row.get(2)?,
        cloud_id: row.get(3)?,
        path: row.get(4)?,
        etag: row.get(5)?,
        size: row.get(6)?,
        modified_unix: row.get(7)?,
        directory: row.get(8)?,
        state: row.get(9)?,
        volume_serial: row.get(10)?,
        native_file_id: row.get(11)?,
    })
}
const COLUMNS: &str = "local_id,root_binding,tag,cloud_id,path,etag,size,modified_unix,directory,state,volume_serial,native_file_id";
fn get(conn: &rusqlite::Connection, id: &str) -> anyhow::Result<Option<NativeProjection>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM local_native_projections WHERE local_id=?1"),
            [id],
            row,
        )
        .optional()?)
}
fn baseline_valid(conn: &rusqlite::Connection, id: &str) -> anyhow::Result<bool> {
    Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM files f JOIN local_native_projections p ON p.local_id=f.remote_id
        WHERE p.local_id=?1 AND f.cloud_remote_id=p.cloud_id AND f.path=p.path AND f.etag=p.etag
        AND f.size=p.size AND f.modified_unix=p.modified_unix AND f.is_dir=p.directory
        AND f.cache_path IS NULL AND f.state='online_only' AND f.release_pending=0
        AND NOT EXISTS(SELECT 1 FROM local_cache_journal WHERE local_id=f.remote_id)
        AND NOT EXISTS(SELECT 1 FROM local_native_dirty WHERE cloud_id=f.remote_id OR cloud_id=f.cloud_remote_id)
        AND NOT EXISTS(SELECT 1 FROM pending_deletes WHERE local_id=f.remote_id OR remote_id=f.remote_id OR cloud_remote_id=f.cloud_remote_id OR path=f.path)
        AND NOT EXISTS(SELECT 1 FROM pending_metadata_operations op JOIN files parent ON parent.remote_id=op.local_id WHERE f.path=parent.path OR substr(f.path,1,length(parent.path)+1)=parent.path||'/')
        AND NOT EXISTS(SELECT 1 FROM native_event_log WHERE binding=p.root_binding))",[id],|row|row.get(0))?)
}
impl CacheJournal {
    /// Idempotent only for the same private publication tag and root. Tags must
    /// be unique per publication; do not reuse cloud IDs as registration tags.
    pub fn prepare_native_projection(
        &self,
        id: &str,
        root: &str,
        tag: &[u8],
    ) -> anyhow::Result<NativeProjection> {
        ensure!(
            !root.is_empty() && root.len() <= 16384 && (16..=4096).contains(&tag.len()),
            "invalid native publication claim"
        );
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = get(&tx, id)? {
            ensure!(
                existing.root_binding == root && existing.tag == tag && baseline_valid(&tx, id)?,
                "native publication claim changed or needs recovery"
            );
            return Ok(existing);
        }
        ensure!(tx.execute("INSERT INTO local_native_projections(local_id,root_binding,tag,cloud_id,path,etag,size,modified_unix,directory,state)
            SELECT remote_id,?2,?3,cloud_remote_id,path,etag,size,modified_unix,is_dir,'preparing' FROM files
            WHERE remote_id=?1 AND path<>'/' AND cloud_remote_id IS NOT NULL AND etag<>''",params![id,root,tag])?==1,"native publication metadata unavailable");
        ensure!(
            baseline_valid(&tx, id)?,
            "native publication baseline is not clean"
        );
        let projection = get(&tx, id)?.context("native publication claim disappeared")?;
        tx.commit()?;
        Ok(projection)
    }

    /// Supply the physical binding only while holding the owned native metadata
    /// lease. This commit does not replace a later edit, rename or cloud version.
    pub fn complete_native_projection(
        &self,
        id: &str,
        root: &str,
        tag: &[u8],
        volume: u32,
        file_id: u64,
    ) -> anyhow::Result<()> {
        ensure!(file_id != 0, "native file identity missing");
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = get(&tx, id)?.context("no durable native publication claim")?;
        ensure!(
            existing.root_binding == root && existing.tag == tag && baseline_valid(&tx, id)?,
            "native publication changed before completion"
        );
        let file_id = format!("{file_id:016x}");
        if existing.state == "published" {
            ensure!(
                existing.volume_serial == Some(volume)
                    && existing.native_file_id.as_deref() == Some(file_id.as_str()),
                "native publication physical identity changed"
            );
            return Ok(());
        }
        ensure!(
            existing.state == "preparing",
            "native publication needs explicit recovery"
        );
        ensure!(tx.execute("UPDATE local_native_projections SET state='published',volume_serial=?2,native_file_id=?3 WHERE local_id=?1 AND state='preparing'",params![id,volume,file_id])?==1,"native publication completion rejected");
        tx.commit()?;
        Ok(())
    }

    pub fn native_projections(&self, root: &str) -> anyhow::Result<Vec<NativeProjection>> {
        let conn = journal_connection(&self.0.db)?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM local_native_projections WHERE root_binding=?1 ORDER BY local_id"
        ))?;
        Ok(stmt.query_map([root], row)?.collect::<Result<_, _>>()?)
    }
    pub fn native_projection(
        &self,
        root: &str,
        tag: &[u8],
    ) -> anyhow::Result<Option<NativeProjection>> {
        Ok(journal_connection(&self.0.db)?.query_row(&format!("SELECT {COLUMNS} FROM local_native_projections WHERE root_binding=?1 AND tag=?2"),params![root,tag],row).optional()?)
    }
}
