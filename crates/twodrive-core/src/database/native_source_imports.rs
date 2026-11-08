//! Verified source replicas retain an independent private owner until their
//! native metadata/residency step completes. No synthetic cloud ID is created.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NativeSourceImport {
    pub sequence: i64,
    pub expected: NativeProjection,
    pub source_root_receipt: String,
    pub source_file: NativeFileIdentity,
    pub cache_path: PathBuf,
    pub digest: String,
    pub source_filetime: u64,
    pub source_pinned: bool,
    pub state: String,
}
impl CacheJournal {
    pub fn native_source_imports(&self, root: &str) -> anyhow::Result<Vec<NativeSourceImport>> {
        let conn = journal_connection(&self.0.db)?;
        let mut statement=conn.prepare("SELECT sequence,payload,state FROM local_native_source_imports WHERE root_binding=?1 AND state IN ('verified','data_ready') ORDER BY sequence")?;
        statement
            .query_map([root], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .map(|row| {
                let (sequence, payload, state) = row?;
                let mut import: NativeSourceImport = serde_json::from_str(&payload)?;
                import.sequence = sequence;
                import.state = state;
                Ok(import)
            })
            .collect()
    }
    pub fn prepare_native_source_import(
        &self,
        mut import: NativeSourceImport,
        verify: impl FnOnce(&mut File) -> anyhow::Result<String>,
    ) -> anyhow::Result<NativeSourceImport> {
        valid_digest(&import.digest)?;
        let source: serde_json::Value = serde_json::from_str(&import.source_root_receipt)?;
        ensure!(
            import.sequence == 0
                && import.expected.state == "published"
                && !import.expected.directory
                && import.source_file.file_id != 0
                && source["schema_version"] == 1
                && source["volume_serial"].as_u64()
                    == Some(import.source_file.volume_serial as u64)
                && source["file_id"]
                    .as_u64()
                    .is_some_and(|id| id != 0 && id != import.source_file.file_id)
                && import.source_filetime >= 116_444_736_000_000_000,
            "invalid verified source receipt"
        );
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut record = self.record(&import.expected.local_id)?;
        ensure!(
            record.cloud_remote_id.as_deref() == Some(import.expected.cloud_id.as_str())
                && record.metadata.etag == import.expected.etag
                && record.metadata.path == import.expected.path
                && record.metadata.size == import.expected.size,
            "source import baseline changed"
        );
        record.cache_path = Some(import.cache_path.clone());
        let mut file = self.file(&record, true)?;
        file.try_lock()?;
        file.sync_all()?;
        ensure!(
            file.metadata()?.len() == import.expected.size && verify(&mut file)? == import.digest,
            "source replica private bytes changed"
        );
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(
            projection::get(&tx, &import.expected.local_id)?.as_ref() == Some(&import.expected),
            "source import publication changed"
        );
        if let Some((sequence,payload,state))=tx.query_row("SELECT sequence,payload,state FROM local_native_source_imports WHERE local_id=?1 AND state IN ('verified','data_ready')",[&import.expected.local_id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional()? {
            let mut existing:NativeSourceImport=serde_json::from_str(&payload)?;
            ensure!(existing.expected==import.expected && existing.digest==import.digest && existing.source_filetime==import.source_filetime,"another source import needs recovery");
            existing.sequence=sequence;existing.state=state;return Ok(existing);
        }
        import.state = "verified".into();
        tx.execute("INSERT INTO local_native_source_imports(local_id,root_binding,payload,state) VALUES(?1,?2,?3,'verified')",params![import.expected.local_id,import.expected.root_binding,serde_json::to_string(&import)?])?;
        import.sequence = tx.last_insert_rowid();
        tx.execute("INSERT INTO local_cache_journal(local_id,generation,acknowledged_generation,dirty_intent,base_etag,acknowledged_digest,cache_path,cloud_id,cloud_path) VALUES(?1,1,0,1,?2,'',?3,?4,?5)",params![format!("source-replica-{}",import.sequence),import.expected.etag,import.cache_path.to_string_lossy(),import.expected.cloud_id,import.expected.path])?;
        tx.commit()?;
        Ok(import)
    }
    pub fn mark_native_source_data_ready(&self, import: &NativeSourceImport) -> anyhow::Result<()> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let payload:String=tx.query_row("SELECT payload FROM local_native_source_imports WHERE sequence=?1 AND state IN ('verified','data_ready')",[import.sequence],|r|r.get(0))?;
        let original: NativeSourceImport = serde_json::from_str(&payload)?;
        ensure!(
            original.expected == import.expected
                && original.digest == import.digest
                && original.cache_path == import.cache_path
                && original.source_filetime == import.source_filetime,
            "source data decision changed"
        );
        let record = self.record(&import.expected.local_id)?;
        let state = snapshot(&tx, &import.expected.local_id)?;
        ensure!(
            record.cloud_remote_id.as_deref() == Some(import.expected.cloud_id.as_str())
                && record.metadata.etag == import.expected.etag
                && record.metadata.size == import.expected.size
                && state.sync_confirmed()
                && state.acknowledged_digest == import.digest,
            "source replica was not confirmed as this cloud version"
        );
        ensure!(tx.execute("UPDATE local_native_source_imports SET state='data_ready' WHERE sequence=?1 AND state IN ('verified','data_ready')",[import.sequence])?==1,"source receipt changed");
        tx.commit()?;
        Ok(())
    }
    pub fn finish_native_source_import(
        &self,
        import: &NativeSourceImport,
        verify: impl FnOnce(&mut File) -> anyhow::Result<String>,
    ) -> anyhow::Result<()> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let record = self.record(&import.expected.local_id)?;
        let mut file = self.file(&record, false)?;
        file.try_lock()?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (payload, state): (String, String) = tx.query_row(
            "SELECT payload,state FROM local_native_source_imports WHERE sequence=?1",
            [import.sequence],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let original: NativeSourceImport = serde_json::from_str(&payload)?;
        ensure!(
            original.expected == import.expected
                && original.digest == import.digest
                && original.cache_path == import.cache_path
                && original.source_filetime == import.source_filetime
                && state == "data_ready",
            "source receipt is not ready for native completion"
        );
        let published = projection::get(&tx, &import.expected.local_id)?
            .context("source publication missing")?;
        ensure!(
            published.state == "published"
                && published.cloud_id == import.expected.cloud_id
                && published.etag == import.expected.etag
                && published.size == import.expected.size
                && published.volume_serial == import.expected.volume_serial
                && published.native_file_id == import.expected.native_file_id
                && published.modified_unix
                    == (import.source_filetime / 10_000_000) as i64 - 11_644_473_600,
            "native source metadata not confirmed"
        );
        let snapshot = snapshot(&tx, &import.expected.local_id)?;
        ensure!(
            snapshot.sync_confirmed()
                && snapshot.readers == 0
                && snapshot.acknowledged_digest == import.digest
                && verify(&mut file)? == import.digest,
            "source completion preserves changed/private data"
        );
        tx.execute(
            "DELETE FROM local_cache_journal WHERE local_id=?1",
            [format!("source-replica-{}", import.sequence)],
        )?;
        if record.cache_path.as_ref() != Some(&import.cache_path) {
            tx.execute(
                "INSERT OR IGNORE INTO retired_cache(cache_path) VALUES(?1)",
                [import.cache_path.to_string_lossy().as_ref()],
            )?;
        }
        tx.execute("UPDATE local_native_source_imports SET state='applied' WHERE sequence=?1 AND state='data_ready'",[import.sequence])?;
        tx.commit()?;
        Ok(())
    }
    /// A changed target keeps its source revision as a recovery owner; this
    /// does not delete or overwrite the target or dispatch a cloud mutation.
    pub fn isolate_native_source_import(&self, sequence: i64) -> anyhow::Result<()> {
        journal_connection(&self.0.db)?.execute("UPDATE local_native_source_imports SET state='isolated' WHERE sequence=?1 AND state IN ('verified','data_ready')",[sequence])?;
        Ok(())
    }
}
