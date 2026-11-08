//! Opaque-ID conditional moves, composed with the existing Linux subtree/pin
//! mutation routines. Every intermediate step remains blocked and recoverable.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NativeMoveIntent {
    pub publication: NativeProjection,
    pub version: MetadataEntry,
    pub destination: String,
    pub parent_id: String,
}
impl CacheJournal {
    pub fn native_move(&self, id: &str) -> anyhow::Result<Option<(NativeMoveIntent, String)>> {
        let row = journal_connection(&self.0.db)?
            .query_row(
                "SELECT intent,state FROM local_native_moves WHERE local_id=?1",
                [id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
        row.map(|(intent, state)| Ok((serde_json::from_str(&intent)?, state)))
            .transpose()
    }
    /// Call while retaining the native root/parent/leaf identity lease. The
    /// version comes from an independent bound-ID read and must equal our base.
    pub fn prepare_native_move(&self, intent: &NativeMoveIntent) -> anyhow::Result<()> {
        let p = &intent.publication;
        let v = &intent.version;
        let destination = crate::normalize_cloud_path(&intent.destination);
        ensure!(
            p.path != "/"
                && destination != "/"
                && destination != p.path
                && !destination.starts_with(&format!("{}/", p.path))
                && destination == intent.destination
                && !intent.parent_id.is_empty()
                && v.remote_id == p.cloud_id
                && v.path == p.path
                && v.is_dir == p.directory
                && !v.etag.is_empty(),
            "invalid native move intent"
        );
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((old, state)) = tx
            .query_row(
                "SELECT intent,state FROM local_native_moves WHERE local_id=?1",
                [&p.local_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if state != "applied" {
                ensure!(
                    serde_json::from_str::<NativeMoveIntent>(&old)? == *intent,
                    "another native move needs recovery"
                );
                return Ok(());
            }
        }
        ensure!(
            projection::get(&tx, &p.local_id)?.as_ref() == Some(p) && p.state == "published",
            "native move publication changed"
        );
        ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM files WHERE remote_id=?1 AND cloud_remote_id=?2 AND path=?3 AND is_dir=?5 AND (is_dir=1 OR etag=?4))",params![p.local_id,p.cloud_id,p.path,v.etag,p.directory],|r|r.get::<_,bool>(0))?,"native move version changed");
        ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM files f LEFT JOIN local_cache_journal j ON j.local_id=f.remote_id
            WHERE (f.path=?1 OR substr(f.path,1,length(?1)+1)=?1||'/') AND (j.upload_generation IS NOT NULL OR j.uncertain_upload=1 OR j.writers>0 OR j.recovery_required=1))",[&p.path],|r|r.get::<_,bool>(0))?,"native move has active or uncertain data work");
        ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM pending_metadata_operations m JOIN files f ON f.remote_id=m.local_id
            WHERE f.path=?1 OR substr(f.path,1,length(?1)+1)=?1||'/' OR substr(?1,1,length(f.path)+1)=f.path||'/')",[&p.path],|r|r.get::<_,bool>(0))?,"native move has pending parent/child metadata");
        ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM pending_deletes WHERE path=?1 OR substr(path,1,length(?1)+1)=?1||'/' OR substr(?1,1,length(path)+1)=path||'/')",[&p.path],|r|r.get::<_,bool>(0))?,"native move has pending parent/child deletion");
        ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM files WHERE (path=?2 OR substr(path,1,length(?2)+1)=?2||'/') AND path<>?1 AND substr(path,1,length(?1)+1)<>?1||'/')",params![p.path,destination],|r|r.get::<_,bool>(0))?,"native move destination has another owner");
        tx.execute("INSERT INTO local_native_moves(local_id,root_binding,source_path,intent,state) VALUES(?1,?2,?3,?4,'prepared') ON CONFLICT(local_id) DO UPDATE SET root_binding=excluded.root_binding,source_path=excluded.source_path,intent=excluded.intent,state='prepared',receipt=NULL",params![p.local_id,p.root_binding,p.path,serde_json::to_string(intent)?])?;
        tx.commit()?;
        Ok(())
    }
    pub fn dispatch_native_move(&self, id: &str) -> anyhow::Result<bool> {
        let conn = journal_connection(&self.0.db)?;
        let state: String = conn.query_row(
            "SELECT state FROM local_native_moves WHERE local_id=?1",
            [id],
            |r| r.get(0),
        )?;
        if matches!(state.as_str(), "received" | "applied") {
            return Ok(false);
        }
        ensure!(
            state == "prepared",
            "native move outcome uncertain; reconciliation required"
        );
        ensure!(conn.execute("UPDATE local_native_moves SET state='dispatched' WHERE local_id=?1 AND state='prepared'",[id])?==1,"native move dispatch changed");
        Ok(true)
    }
    pub fn native_move_not_dispatched(&self, id: &str) -> anyhow::Result<()> {
        ensure!(journal_connection(&self.0.db)?.execute("UPDATE local_native_moves SET state='prepared' WHERE local_id=?1 AND state='dispatched' AND receipt IS NULL",[id])?==1,"native move dispatch fence changed");
        Ok(())
    }
    pub fn receive_native_move(&self, id: &str, receipt: &MetadataEntry) -> anyhow::Result<()> {
        let (intent, state) = self.native_move(id)?.context("native move missing")?;
        ensure!(
            state == "dispatched"
                && receipt.remote_id == intent.version.remote_id
                && receipt.path == intent.destination
                && receipt.is_dir == intent.version.is_dir
                && receipt.size == intent.version.size
                && !receipt.etag.is_empty(),
            "native move receipt mismatch"
        );
        ensure!(journal_connection(&self.0.db)?.execute("UPDATE local_native_moves SET state='received',receipt=?2 WHERE local_id=?1 AND state='dispatched' AND intent=?3",params![id,serde_json::to_string(receipt)?,serde_json::to_string(&intent)?])?==1,"native move receipt owner changed");
        Ok(())
    }
    pub fn apply_native_move(&self, id: &str) -> anyhow::Result<()> {
        let (intent, state) = self.native_move(id)?.context("native move missing")?;
        if state == "applied" {
            return Ok(());
        }
        ensure!(state == "received", "native move has no receipt");
        let receipt: String = journal_connection(&self.0.db)?.query_row(
            "SELECT receipt FROM local_native_moves WHERE local_id=?1 AND state='received'",
            [id],
            |r| r.get(0),
        )?;
        let receipt: MetadataEntry = serde_json::from_str(&receipt)?;
        let before = self.record(id)?;
        // These shipped routines preserve descendant pending jobs and pin rules.
        // A crash between their commits is still fenced by local_native_moves
        // and by the journal's path/version mismatch veto.
        if before.metadata.path == intent.publication.path {
            self.0.db.move_subtree_and_queue(id, &intent.destination)?;
        } else {
            ensure!(
                before.metadata.path == intent.destination,
                "native move local path changed"
            );
        }
        ensure!(
            self.0
                .db
                .complete_metadata_operation(id, &intent.destination, &receipt)?,
            "native move completion path changed"
        );
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(
            projection::get(&tx, id)?.as_ref() == Some(&intent.publication),
            "native move publication changed during completion"
        );
        let current = self.record(id)?;
        ensure!(
            current.cloud_remote_id.as_deref() == Some(receipt.remote_id.as_str())
                && current.metadata.etag == receipt.etag
                && current.metadata.path == intent.destination,
            "native move receipt no longer current"
        );
        let source = &intent.publication.path;
        let mut stmt=tx.prepare("SELECT local_id,path FROM local_native_projections WHERE root_binding=?1 AND (path=?2 OR substr(path,1,length(?2)+1)=?2||'/')")?;
        let descendants = stmt
            .query_map(params![intent.publication.root_binding, source], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for (child, old_path) in descendants {
            let new_path = format!("{}{}", intent.destination, &old_path[source.len()..]);
            ensure!(
                tx.execute(
                    "UPDATE local_native_projections SET path=?2 WHERE local_id=?1 AND path=?3",
                    params![child, new_path, old_path]
                )? == 1,
                "native move descendant changed"
            );
            tx.execute("UPDATE local_cache_journal SET cloud_path=?2 WHERE local_id=?1 AND cloud_path=?3 AND upload_generation IS NULL",params![child,new_path,old_path])?;
            tx.execute(
                "UPDATE local_native_creations SET path=?2 WHERE local_id=?1 AND path=?3",
                params![child, new_path, old_path],
            )?;
        }
        tx.execute("UPDATE local_cache_journal SET base_etag=?2 WHERE local_id=?1 AND base_etag=?3 AND upload_generation IS NULL",params![id,receipt.etag,intent.version.etag])?;
        tx.execute("UPDATE local_native_projections SET etag=?2,size=?3,modified_unix=?4 WHERE local_id=?1",params![id,receipt.etag,i64::try_from(current.metadata.size)?,current.metadata.modified_unix])?;
        ensure!(tx.execute("UPDATE local_native_moves SET state='applied' WHERE local_id=?1 AND state='received' AND intent=?2",params![id,serde_json::to_string(&intent)?])?==1,"native move completion fence changed");
        tx.commit()?;
        Ok(())
    }
}
