//! Directory requests are fenced before dispatch. An ambiguous create cannot be
//! retried by pathname or adopted from an unrelated same-name remote folder.
use super::*;

impl CacheJournal {
    pub fn stage_native_directory(&self, id: &str, parent: &str) -> anyhow::Result<()> {
        ensure!(!parent.is_empty(), "native directory parent missing");
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM files f JOIN local_native_creations c ON c.local_id=f.remote_id
            WHERE f.remote_id=?1 AND f.is_dir=1 AND f.path=c.path AND c.state IN ('preparing','directory-staged','directory-bound'))",[id],|r|r.get::<_,bool>(0))?,"native directory claim missing or changed");
        tx.execute("INSERT INTO local_native_directory_transfers(local_id,parent_id,state) VALUES(?1,?2,'prepared') ON CONFLICT(local_id) DO NOTHING",params![id,parent])?;
        ensure!(
            tx.query_row(
                "SELECT parent_id=?2 FROM local_native_directory_transfers WHERE local_id=?1",
                params![id, parent],
                |r| r.get::<_, bool>(0)
            )?,
            "native directory parent changed during creation"
        );
        tx.execute("UPDATE local_native_creations SET state='directory-staged' WHERE local_id=?1 AND state='preparing'",[id])?;
        tx.commit()?;
        Ok(())
    }
    /// False means a durable receipt already exists. A dispatched request without
    /// a receipt is deliberately an error, including after reopening the owner.
    pub fn dispatch_native_directory(&self, id: &str) -> anyhow::Result<bool> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state: String = tx.query_row(
            "SELECT state FROM local_native_directory_transfers WHERE local_id=?1",
            [id],
            |r| r.get(0),
        )?;
        if state == "received" {
            return Ok(false);
        }
        ensure!(
            state == "prepared",
            "native directory create outcome is uncertain; reconciliation required"
        );
        ensure!(tx.execute("UPDATE local_native_directory_transfers SET state='dispatched' WHERE local_id=?1 AND state='prepared'",[id])? == 1,"native directory dispatch changed");
        tx.commit()?;
        Ok(true)
    }
    pub fn native_directory_not_dispatched(&self, id: &str) -> anyhow::Result<()> {
        // The adapter may call this ONLY when no data mutation was dispatched.
        let conn = journal_connection(&self.0.db)?;
        ensure!(conn.execute("UPDATE local_native_directory_transfers SET state='prepared' WHERE local_id=?1 AND state='dispatched' AND receipt IS NULL",[id])? == 1,"native directory dispatch fence changed");
        Ok(())
    }
    pub fn receive_native_directory(&self, id: &str, entry: &MetadataEntry) -> anyhow::Result<()> {
        ensure!(
            entry.is_dir && !entry.remote_id.is_empty() && !entry.etag.is_empty(),
            "invalid native directory receipt"
        );
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM files f JOIN local_native_creations c ON c.local_id=f.remote_id
            JOIN local_native_directory_transfers t ON t.local_id=f.remote_id
            WHERE f.remote_id=?1 AND f.path=?2 AND c.path=f.path AND f.is_dir=1 AND t.state='dispatched' AND t.receipt IS NULL)",params![id,entry.path],|r|r.get::<_,bool>(0))?,"native directory receipt target changed");
        tx.execute("UPDATE local_native_directory_transfers SET state='received',receipt=?2 WHERE local_id=?1",params![id,serde_json::to_string(&(&entry.remote_id, &entry.path, entry.modified_unix, &entry.etag))?])?;
        tx.execute("UPDATE local_native_creations SET state='directory-bound',cloud_id=?2 WHERE local_id=?1",params![id,entry.remote_id])?;
        tx.commit()?;
        Ok(())
    }
    pub fn native_directory_receipt(&self, id: &str) -> anyhow::Result<MetadataEntry> {
        let value: String = journal_connection(&self.0.db)?.query_row("SELECT receipt FROM local_native_directory_transfers WHERE local_id=?1 AND state='received'",[id],|r|r.get(0))?;
        let (cloud, path, modified, etag): (String, String, i64, String) =
            serde_json::from_str(&value)?;
        Ok(MetadataEntry::new_dir(cloud, path, modified, etag))
    }
    pub fn with_native_directory_ack(
        &self,
        id: &str,
        root: &str,
        receipt: &str,
        physical: NativeFileIdentity,
        acknowledge: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let binding = format!("{:08x}:{:016x}", physical.volume_serial, physical.file_id);
        let cloud = self.native_directory_receipt(id)?;
        let record = self.record(id)?;
        ensure!(
            record.metadata.is_dir
                && record.metadata.path == cloud.path
                && record.cloud_remote_id.as_deref() == Some(cloud.remote_id.as_str())
                && record.metadata.etag == cloud.etag
                && record.metadata.modified_unix == cloud.modified_unix,
            "native directory data no longer matches its create receipt"
        );
        let root_receipt: serde_json::Value = serde_json::from_str(receipt)?;
        ensure!(
            root_receipt["identity"] == serde_json::to_value(root.as_bytes())?
                && root_receipt["volume_serial"].as_u64() == Some(physical.volume_serial as u64)
                && physical.file_id != 0,
            "native directory root changed"
        );
        ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM files f JOIN local_native_creations c ON c.local_id=f.remote_id
            JOIN local_native_directory_transfers t ON t.local_id=f.remote_id
            WHERE f.remote_id=?1 AND f.path=c.path AND f.is_dir=1 AND f.cloud_remote_id=c.cloud_id AND f.etag<>''
            AND c.root_binding=?2 AND c.native_binding=?3 AND c.state='directory-bound' AND t.state='received'
            AND NOT EXISTS(SELECT 1 FROM pending_metadata_operations WHERE local_id=f.remote_id)
            AND NOT EXISTS(SELECT 1 FROM native_event_log WHERE binding=?4))",params![id,receipt,binding,root],|r|r.get::<_,bool>(0))?,"native directory publication changed or has pending namespace work");
        if let Some(existing) = projection::get(&tx, id)? {
            ensure!(
                existing.root_binding == root
                    && existing.tag == id.as_bytes()
                    && existing.cloud_id == cloud.remote_id
                    && existing.path == cloud.path
                    && existing.etag == cloud.etag
                    && existing.modified_unix == cloud.modified_unix
                    && existing.directory
                    && existing.state == "published"
                    && existing.size == 0
                    && existing.volume_serial == Some(physical.volume_serial)
                    && existing.native_file_id.as_deref()
                        == Some(format!("{:016x}", physical.file_id).as_str()),
                "native directory physical publication changed"
            );
        }
        acknowledge()?;
        tx.execute("INSERT INTO local_native_projections(local_id,root_binding,tag,cloud_id,path,etag,size,modified_unix,directory,state,volume_serial,native_file_id)
            SELECT remote_id,?2,?3,cloud_remote_id,path,etag,0,modified_unix,1,'published',?4,?5 FROM files WHERE remote_id=?1
            ON CONFLICT(local_id) DO NOTHING",params![id,root,id.as_bytes(),physical.volume_serial,format!("{:016x}",physical.file_id)])?;
        tx.commit()?;
        Ok(())
    }
}
