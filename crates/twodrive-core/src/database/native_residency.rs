//! Native residency decisions share the cache owner and generation fences.
use super::*;

impl CacheJournal {
    /// Observations were bound to native root/tag/physical identities by the
    /// caller. Apply a batch before recomputing shared directory inheritance.
    pub fn mirror_native_pins(&self, changes: &[(String, bool, bool)]) -> anyhow::Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (id, pinned, unpin) in changes {
            ensure!(!(*pinned && *unpin), "conflicting native pin intent");
            ensure!(tx.execute("UPDATE files SET pin_explicit=?2,pin_inheritance_blocked=?3 WHERE remote_id=?1",params![id,pinned,unpin])?==1,"native pin owner missing");
        }
        tx.commit()?;
        self.0.db.recompute_native_pin_inheritance()
    }

    /// Caller retains the native exclusive oplock and verifies physical/root
    /// identity. Native dehydration occurs while the private digest, generation,
    /// pin policy and SQLite write transaction remain leased together.
    pub fn release_native_cache(
        &self,
        id: &str,
        generation: i64,
        digest: impl FnOnce(&mut File) -> anyhow::Result<String>,
        dehydrate: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<bool> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = snapshot(&tx, id)?;
        if state.generation != generation || !state.engine_release_ready() {
            return Ok(false);
        }
        let record = self.record(id)?;
        let path = record
            .cache_path
            .as_ref()
            .context("native release cache missing")?;
        let mut file = self.file(&record, false)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
            Err(e) => return Err(e.into()),
        }
        if digest(&mut file)? != state.acknowledged_digest {
            tx.execute("UPDATE local_cache_journal SET generation=generation+1,dirty_intent=1,recovery_required=1 WHERE local_id=?1",[id])?;
            tx.execute("UPDATE files SET state='dirty' WHERE remote_id=?1", [id])?;
            tx.commit()?;
            anyhow::bail!("native release preserves changed private bytes");
        }
        dehydrate()?;
        tx.execute(
            "INSERT OR IGNORE INTO retired_cache(cache_path) VALUES(?1)",
            [path.to_string_lossy().as_ref()],
        )?;
        tx.execute("DELETE FROM local_cache_journal WHERE local_id=?1", [id])?;
        tx.execute("DELETE FROM local_native_captures WHERE local_id=?1", [id])?;
        tx.execute("UPDATE files SET state='online_only',cache_path=NULL,cache_accessed_unix=NULL,download_generation=download_generation+1 WHERE remote_id=?1",[id])?;
        tx.commit()?;
        drop(file);
        self.0.db.remove_unreferenced_cache(path)?;
        Ok(true)
    }
}
