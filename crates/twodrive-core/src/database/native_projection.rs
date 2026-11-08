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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeFileIdentity {
    pub volume_serial: u32,
    pub file_id: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeCaptureBinding {
    pub generation: i64,
    pub physical: NativeFileIdentity,
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
            WHERE native_file_id IS NOT NULL;
        CREATE TABLE IF NOT EXISTS local_native_captures(local_id TEXT PRIMARY KEY,generation INTEGER NOT NULL,volume_serial INTEGER NOT NULL,file_id TEXT NOT NULL);",
    )?;
    Ok(())
}
pub(super) fn record_capture(
    conn: &rusqlite::Connection,
    id: &str,
    generation: i64,
    expected: &NativeProjection,
    physical: NativeFileIdentity,
) -> anyhow::Result<()> {
    ensure!(
        physical.file_id != 0
            && expected.volume_serial == Some(physical.volume_serial)
            && get(conn, id)?.as_ref() == Some(expected)
            && expected.state == "published",
        "native capture publication changed"
    );
    ensure!(conn.query_row("SELECT EXISTS(SELECT 1 FROM files WHERE remote_id=?1 AND cloud_remote_id=?2 AND path=?3)",params![id,expected.cloud_id,expected.path],|row|row.get::<_,bool>(0))?,"native capture target changed");
    conn.execute("INSERT INTO local_native_captures(local_id,generation,volume_serial,file_id) VALUES(?1,?2,?3,?4)
        ON CONFLICT(local_id) DO UPDATE SET generation=excluded.generation,volume_serial=excluded.volume_serial,file_id=excluded.file_id",params![id,generation,physical.volume_serial,format!("{:016x}",physical.file_id)])?;
    Ok(())
}
fn capture(conn: &rusqlite::Connection, id: &str) -> anyhow::Result<Option<NativeCaptureBinding>> {
    let row = conn
        .query_row(
            "SELECT generation,volume_serial,file_id FROM local_native_captures WHERE local_id=?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    row.map(|(generation, volume_serial, file_id)| {
        Ok(NativeCaptureBinding {
            generation,
            physical: NativeFileIdentity {
                volume_serial,
                file_id: u64::from_str_radix(&file_id, 16)?,
            },
        })
    })
    .transpose()
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
    /// Publish a newly created native file only after its create receipt and
    /// retained native/private full-byte verification agree on this generation.
    pub fn with_created_projection_ack(
        &self,
        id: &str,
        generation: i64,
        root: &str,
        creation_receipt: &str,
        physical: NativeFileIdentity,
        action: impl FnOnce(&mut File, &str) -> anyhow::Result<bool>,
    ) -> anyhow::Result<Option<bool>> {
        ensure!(
            id.starts_with("local-upload-") && physical.file_id != 0 && !root.is_empty(),
            "invalid native creation publication"
        );
        let receipt: serde_json::Value = serde_json::from_str(creation_receipt)?;
        let identity = receipt["identity"]
            .as_array()
            .context("creation root identity missing")?
            .iter()
            .map(|v| {
                v.as_u64()
                    .filter(|v| *v <= 255)
                    .map(|v| v as u8)
                    .context("invalid creation root byte")
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        ensure!(
            identity == root.as_bytes()
                && receipt["volume_serial"].as_u64() == Some(physical.volume_serial as u64),
            "creation root binding changed"
        );
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = snapshot(&tx, id)?;
        if state.generation != generation || !state.sync_confirmed() || !state.resident {
            return Ok(None);
        }
        let record = self.record(id)?;
        let cloud = record
            .cloud_remote_id
            .as_deref()
            .context("native creation has no cloud receipt")?;
        ensure!(get(&tx,id)?.is_none() && tx.query_row("SELECT EXISTS(SELECT 1 FROM local_native_creations WHERE local_id=?1 AND path=?2 AND root_binding=?3 AND cloud_id=?4 AND state='bound')",params![id,record.metadata.path,creation_receipt,cloud],|row|row.get::<_,bool>(0))?,"native creation receipt changed before publication");
        let mut file = self.file(&record, false)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let confirmed = action(&mut file, &state.acknowledged_digest)?;
        if confirmed {
            tx.execute("INSERT INTO local_native_projections(local_id,root_binding,tag,cloud_id,path,etag,size,modified_unix,directory,state,volume_serial,native_file_id)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,0,'published',?9,?10)",params![id,root,id.as_bytes(),cloud,record.metadata.path,record.metadata.etag,i64::try_from(record.metadata.size)?,record.metadata.modified_unix,physical.volume_serial,format!("{:016x}",physical.file_id)])?;
            tx.execute(
                "UPDATE local_native_creations SET native_binding=?2 WHERE local_id=?1",
                params![
                    id,
                    format!("{:08x}:{:016x}", physical.volume_serial, physical.file_id)
                ],
            )?;
        }
        tx.commit()?;
        Ok(Some(confirmed))
    }
    /// Advance only after the callback verifies the current native bytes and
    /// acknowledges them while retaining its physical native lease.
    pub fn with_native_projection_ack(
        &self,
        id: &str,
        generation: i64,
        expected: &NativeProjection,
        action: impl FnOnce(&mut File, &str) -> anyhow::Result<bool>,
    ) -> anyhow::Result<Option<bool>> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = snapshot(&tx, id)?;
        if state.generation != generation || !state.sync_confirmed() || !state.resident {
            return Ok(None);
        }
        ensure!(
            get(&tx, id)?.as_ref() == Some(expected)
                && expected.state == "published"
                && !expected.directory,
            "native projection changed before acknowledgement"
        );
        let record = self.record(id)?;
        ensure!(
            record.cloud_remote_id.as_deref() == Some(expected.cloud_id.as_str())
                && record.metadata.path == expected.path,
            "native acknowledgement target changed"
        );
        let mut file = self.file(&record, false)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let captured = capture(&tx, id)?;
        ensure!(
            captured.is_none_or(|binding| binding.generation == generation),
            "native capture generation changed before acknowledgement"
        );
        let confirmed = action(&mut file, &state.acknowledged_digest)?;
        if confirmed {
            let volume = captured
                .map(|c| c.physical.volume_serial)
                .or(expected.volume_serial)
                .context("native publication volume missing")?;
            let file_id = captured
                .map(|c| format!("{:016x}", c.physical.file_id))
                .or_else(|| expected.native_file_id.clone())
                .context("native publication file identity missing")?;
            ensure!(tx.execute("UPDATE local_native_projections SET etag=?2,size=?3,modified_unix=?4,volume_serial=?5,native_file_id=?6 WHERE local_id=?1 AND state='published'",params![id,record.metadata.etag,i64::try_from(record.metadata.size)?,record.metadata.modified_unix,volume,file_id])?==1,"native projection acknowledgement rejected");
            tx.execute("DELETE FROM local_native_captures WHERE local_id=?1", [id])?;
        }
        tx.commit()?;
        Ok(Some(confirmed))
    }
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
    pub fn native_projection_for_id(&self, id: &str) -> anyhow::Result<Option<NativeProjection>> {
        get(&journal_connection(&self.0.db)?, id)
    }
    pub fn native_capture_binding(&self, id: &str) -> anyhow::Result<Option<NativeCaptureBinding>> {
        capture(&journal_connection(&self.0.db)?, id)
    }
}
