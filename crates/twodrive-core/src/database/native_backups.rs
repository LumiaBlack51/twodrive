//! A retained ReplaceFile backup is a separate local copy, not a cloud rename
//! of the logical target. Persist both snapshots before removing its old tag.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NativeBackupCopy {
    pub sequence: i64,
    #[serde(default)]
    pub root_file_id: u64,
    #[serde(default)]
    pub history_through: i64,
    pub source: NativeProjection,
    pub path: String,
    pub physical: NativeFileIdentity,
    pub target: NativeFileIdentity,
    pub cache_path: PathBuf,
    pub digest: String,
    pub size: u64,
    pub modified_unix: i64,
    pub state: String,
}
fn consume_backup_history(
    conn: &rusqlite::Connection,
    copy: &NativeBackupCopy,
) -> anyhow::Result<()> {
    ensure!(copy.root_file_id != 0, "backup event root missing");
    let mut statement = conn.prepare(
        "SELECT sequence,payload FROM native_event_log WHERE binding=?1 AND sequence<=?2",
    )?;
    let rows = statement
        .query_map(
            params![copy.source.root_binding, copy.history_through],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (sequence, payload) in rows {
        let event: serde_json::Value = serde_json::from_str(&payload)?;
        if event["schema_version"] == 1
            && event["root_file_id"].as_u64() == Some(copy.root_file_id)
            && event["volume_serial"].as_u64() == Some(copy.physical.volume_serial as u64)
            && event["file_id"].as_u64() == Some(copy.physical.file_id)
            && event["identity"] == serde_json::to_value(&copy.source.tag)?
            && event["kind"].as_str().is_some_and(|k| {
                matches!(
                    k,
                    "rename_requested"
                        | "rename_completed"
                        | "delete_requested"
                        | "delete_completed"
                )
            })
        {
            conn.execute(
                "DELETE FROM native_event_log WHERE binding=?1 AND sequence=?2 AND payload=?3",
                params![copy.source.root_binding, sequence, payload],
            )?;
        }
    }
    Ok(())
}
impl CacheJournal {
    /// Backfill only receipts from before event fences existed. A completed
    /// legacy receipt replays its old-object ACK without touching cloud data.
    pub fn recover_legacy_backup_history(&self, root: &str, root_id: u64) -> anyhow::Result<()> {
        ensure!(root_id != 0, "backup root missing");
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement=tx.prepare("SELECT sequence,payload,state FROM local_native_backup_copies WHERE root_binding=?1 AND state IN ('prepared','reverted') AND COALESCE(json_extract(payload,'$.root_file_id'),0)=0")?;
        let rows = statement
            .query_map([root], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for (sequence, payload, state) in rows {
            let mut copy: NativeBackupCopy = serde_json::from_str(&payload)?;
            ensure!(
                copy.source.root_binding == root,
                "legacy backup root changed"
            );
            copy.root_file_id = root_id;
            copy.history_through = tx.query_row(
                "SELECT COALESCE(MAX(sequence),0) FROM native_event_log WHERE binding=?1",
                [root],
                |r| r.get(0),
            )?;
            tx.execute(
                "UPDATE local_native_backup_copies SET payload=?2 WHERE sequence=?1 AND payload=?3",
                params![sequence, serde_json::to_string(&copy)?, payload],
            )?;
            if state == "reverted" {
                consume_backup_history(&tx, &copy)?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    pub fn native_backup_copies(&self, root: &str) -> anyhow::Result<Vec<NativeBackupCopy>> {
        let conn = journal_connection(&self.0.db)?;
        let mut stmt=conn.prepare("SELECT sequence,payload,state FROM local_native_backup_copies WHERE root_binding=?1 AND state='prepared' ORDER BY sequence")?;
        stmt.query_map([root], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .map(|row| {
            let (sequence, payload, state) = row?;
            let mut copy: NativeBackupCopy = serde_json::from_str(&payload)?;
            copy.sequence = sequence;
            copy.state = state;
            Ok(copy)
        })
        .collect()
    }
    /// The target's complete captured generation must already be durable. Call
    /// while retaining both native file leases and the fsynced backup snapshot.
    pub fn prepare_native_backup_copy(
        &self,
        mut copy: NativeBackupCopy,
        verify: impl FnOnce(&mut File) -> anyhow::Result<String>,
    ) -> anyhow::Result<NativeBackupCopy> {
        valid_digest(&copy.digest)?;
        ensure!(
            copy.sequence == 0
                && copy.root_file_id != 0
                && !copy.source.directory
                && copy.path != copy.source.path
                && copy.physical.volume_serial == copy.target.volume_serial
                && copy.physical.file_id != copy.target.file_id
                && copy.source.volume_serial == Some(copy.physical.volume_serial),
            "invalid native backup split"
        );
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut record = self.record(&copy.source.local_id)?;
        record.cache_path = Some(copy.cache_path.clone());
        // FlushFileBuffers requires write access on Windows; the source is an
        // internal immutable snapshot, never the visible Office/native file.
        let mut file = self.file(&record, true)?;
        file.try_lock()?;
        ensure!(
            file.metadata()?.len() == copy.size,
            "native backup snapshot size changed"
        );
        file.sync_all().context("flush native backup snapshot")?;
        ensure!(
            verify(&mut file)? == copy.digest,
            "native backup digest changed before durable ownership"
        );
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current =
            projection::get(&tx, &copy.source.local_id)?.context("native backup source missing")?;
        ensure!(
            current.root_binding == copy.source.root_binding
                && current.tag == copy.source.tag
                && current.cloud_id == copy.source.cloud_id
                && current.path == copy.source.path,
            "native backup logical owner changed"
        );
        let captured = projection::capture(&tx, &copy.source.local_id)?;
        let state = snapshot(&tx, &copy.source.local_id)?;
        ensure!(
            captured.is_some_and(|c| c.physical == copy.target && c.generation == state.generation)
                && state.upload_generation.is_none()
                && !state.uncertain_upload
                && state.readers == 0
                && state.writers == 0,
            "native backup target is not durably captured"
        );
        tx.execute("UPDATE local_native_backup_copies SET state='superseded' WHERE root_binding=?1 AND volume_serial=?2 AND file_id=?3 AND state='prepared'",params![copy.source.root_binding,copy.physical.volume_serial,format!("{:016x}",copy.physical.file_id)])?;
        copy.history_through = tx.query_row(
            "SELECT COALESCE(MAX(sequence),0) FROM native_event_log WHERE binding=?1",
            [&copy.source.root_binding],
            |r| r.get(0),
        )?;
        copy.state = "prepared".into();
        tx.execute("INSERT INTO local_native_backup_copies(root_binding,volume_serial,file_id,payload,state) VALUES(?1,?2,?3,?4,'prepared')",params![copy.source.root_binding,copy.physical.volume_serial,format!("{:016x}",copy.physical.file_id),serde_json::to_string(&copy)?])?;
        copy.sequence = tx.last_insert_rowid();
        let archive = format!("native-backup-copy-{}", copy.sequence);
        tx.execute("INSERT INTO local_cache_journal(local_id,generation,acknowledged_generation,dirty_intent,base_etag,acknowledged_digest,cache_path,cloud_id,cloud_path)
            VALUES(?1,1,0,1,?2,'',?3,?4,?5)",params![archive,copy.source.etag,copy.cache_path.to_string_lossy(),copy.source.cloud_id,copy.path])?;
        tx.commit()?;
        Ok(copy)
    }
    pub fn complete_native_backup_copy(
        &self,
        sequence: i64,
        verify: impl FnOnce(&mut File) -> anyhow::Result<String>,
    ) -> anyhow::Result<()> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let payload: String = tx.query_row(
            "SELECT payload FROM local_native_backup_copies WHERE sequence=?1 AND state='prepared'",
            [sequence],
            |r| r.get(0),
        )?;
        let copy: NativeBackupCopy = serde_json::from_str(&payload)?;
        let mut archive_record = self.record(&copy.source.local_id)?;
        archive_record.cache_path = Some(copy.cache_path.clone());
        let mut archive = self.file(&archive_record, false)?;
        archive.try_lock()?;
        ensure!(
            archive.metadata()?.len() == copy.size && verify(&mut archive)? == copy.digest,
            "backup archive changed before recovery"
        );
        let current =
            projection::get(&tx, &copy.source.local_id)?.context("native backup owner missing")?;
        ensure!(
            current.root_binding == copy.source.root_binding
                && current.tag == copy.source.tag
                && current.cloud_id == copy.source.cloud_id,
            "native backup logical owner changed"
        );
        let captured = projection::capture(&tx, &copy.source.local_id)?;
        let state = snapshot(&tx, &copy.source.local_id)?;
        ensure!(
            captured.is_some_and(|c| c.physical == copy.target && c.generation == state.generation)
                && state.readers == 0
                && state.writers == 0
                && state.upload_generation.is_none()
                && !state.uncertain_upload,
            "backup target snapshot changed before local completion"
        );
        // Release the reverted old binding; publish the captured new physical
        // identity only after full cloud/native acknowledgement.
        tx.execute("UPDATE local_native_projections SET native_file_id=NULL WHERE local_id=?1 AND volume_serial=?2 AND native_file_id=?3",params![copy.source.local_id,copy.physical.volume_serial,format!("{:016x}",copy.physical.file_id)])?;
        ensure!(tx.execute("UPDATE local_native_backup_copies SET state='reverted' WHERE sequence=?1 AND state='prepared'",[sequence])?==1,"native backup completion changed");
        consume_backup_history(&tx, &copy)?;
        tx.commit()?;
        Ok(())
    }
}
