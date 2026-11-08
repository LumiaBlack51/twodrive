//! Conditional recycle requires a matching durable kernel request/completion.
//! Absence/error 87 alone is never delete authority. Private revisions remain
//! independently owned after the visible metadata is retired.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NativeDeleteIntent {
    #[serde(default)]
    pub directory_parent_id: Option<String>,
    pub publication: NativeProjection,
    pub version: MetadataEntry,
    pub root_receipt: String,
    pub physical: NativeFileIdentity,
    pub request_sequence: i64,
    pub request_payload: String,
    pub completion_sequence: i64,
    pub completion_payload: String,
    pub not_before_unix: i64,
}
fn proof(intent: &NativeDeleteIntent) -> anyhow::Result<()> {
    let p = &intent.publication;
    let root: serde_json::Value = serde_json::from_str(&intent.root_receipt)?;
    let request: serde_json::Value = serde_json::from_str(&intent.request_payload)?;
    let complete: serde_json::Value = serde_json::from_str(&intent.completion_payload)?;
    ensure!(
        root["schema_version"] == 1
            && root["identity"] == serde_json::to_value(p.root_binding.as_bytes())?
            && root["volume_serial"].as_u64() == Some(intent.physical.volume_serial as u64)
            && root["file_id"].as_u64().is_some_and(|id| id != 0)
            && intent.request_sequence > 0
            && intent.completion_sequence > intent.request_sequence,
        "native delete root/order proof invalid"
    );
    for (event, kind) in [
        (&request, "delete_requested"),
        (&complete, "delete_completed"),
    ] {
        ensure!(
            event["schema_version"] == 1
                && event["kind"] == kind
                && event["volume_serial"] == root["volume_serial"]
                && event["root_file_id"] == root["file_id"]
                && event["file_id"].as_u64() == Some(intent.physical.file_id)
                && event["identity"] == serde_json::to_value(&p.tag)?,
            "native delete kernel identity proof changed"
        );
    }
    ensure!(
        request["connection_key"].is_i64()
            && request["request_key"].is_i64()
            && complete["request_key"].is_i64()
            && request["connection_key"] == complete["connection_key"]
            && (request["request_key"] == complete["request_key"] || complete["request_key"] == 0),
        "native delete completion does not match request"
    );
    Ok(())
}
fn consume_history(conn: &rusqlite::Connection, intent: &NativeDeleteIntent) -> anyhow::Result<()> {
    proof(intent)?;
    let root: serde_json::Value = serde_json::from_str(&intent.root_receipt)?;
    let mut stmt = conn.prepare(
        "SELECT sequence,payload FROM native_event_log WHERE binding=?1 AND sequence<=?2",
    )?;
    let rows = stmt
        .query_map(
            params![intent.publication.root_binding, intent.completion_sequence],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    for (sequence, payload) in rows {
        let event: serde_json::Value = serde_json::from_str(&payload)?;
        if event["schema_version"] == 1
            && event["root_file_id"] == root["file_id"]
            && event["volume_serial"].as_u64() == Some(intent.physical.volume_serial as u64)
            && event["file_id"].as_u64() == Some(intent.physical.file_id)
            && event["identity"] == serde_json::to_value(&intent.publication.tag)?
            && event["kind"].as_str().is_some_and(|kind| {
                matches!(
                    kind,
                    "rename_requested"
                        | "rename_completed"
                        | "delete_requested"
                        | "delete_completed"
                )
            })
        {
            conn.execute(
                "DELETE FROM native_event_log WHERE binding=?1 AND sequence=?2 AND payload=?3",
                params![intent.publication.root_binding, sequence, payload],
            )?;
        }
    }
    Ok(())
}
impl CacheJournal {
    pub fn native_delete_for_tag(
        &self,
        root: &str,
        tag: &[u8],
    ) -> anyhow::Result<Option<(NativeDeleteIntent, String)>> {
        let conn = journal_connection(&self.0.db)?;
        let row=conn.query_row("SELECT intent,state FROM local_native_deletes WHERE root_binding=?1 AND json_extract(intent,'$.publication.tag')=?2",params![root,serde_json::to_string(tag)?],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()?;
        row.map(|(intent, state)| Ok((serde_json::from_str(&intent)?, state)))
            .transpose()
    }
    pub fn acknowledge_native_delete_history(
        &self,
        intent: &NativeDeleteIntent,
    ) -> anyhow::Result<()> {
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(
            tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM local_native_deletes WHERE local_id=?1 AND intent=?2)",
                params![intent.publication.local_id, serde_json::to_string(intent)?],
                |r| r.get::<_, bool>(0)
            )?,
            "retained delete decision changed"
        );
        consume_history(&tx, intent)?;
        tx.commit()?;
        Ok(())
    }

    pub fn native_delete(&self, id: &str) -> anyhow::Result<Option<(NativeDeleteIntent, String)>> {
        let raw = journal_connection(&self.0.db)?
            .query_row(
                "SELECT intent,state FROM local_native_deletes WHERE local_id=?1",
                [id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
        raw.map(|(intent, state)| Ok((serde_json::from_str(&intent)?, state)))
            .transpose()
    }
    pub fn native_pending_deletes(&self) -> anyhow::Result<Vec<(NativeDeleteIntent, String)>> {
        let conn = journal_connection(&self.0.db)?;
        let mut stmt=conn.prepare("SELECT intent,state FROM local_native_deletes WHERE state NOT IN ('applied','cancelled') ORDER BY local_id")?;
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .map(|row| {
                let (intent, state) = row?;
                Ok((serde_json::from_str(&intent)?, state))
            })
            .collect()
    }
    pub fn native_deletes(&self) -> anyhow::Result<Vec<(NativeDeleteIntent, String)>> {
        let conn = journal_connection(&self.0.db)?;
        let mut stmt =
            conn.prepare("SELECT intent,state FROM local_native_deletes ORDER BY local_id")?;
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .map(|row| {
                let (intent, state) = row?;
                Ok((serde_json::from_str(&intent)?, state))
            })
            .collect()
    }
    pub fn prepare_native_delete(&self, intent: &NativeDeleteIntent) -> anyhow::Result<()> {
        let p = &intent.publication;
        let v = &intent.version;
        ensure!(
            p.state == "published"
                && p.path != "/"
                && v.remote_id == p.cloud_id
                && v.path == p.path
                && v.is_dir == p.directory
                && (!p.directory
                    || intent
                        .directory_parent_id
                        .as_ref()
                        .is_some_and(|id| !id.is_empty()))
                && !v.etag.is_empty()
                && intent.physical.file_id != 0,
            "invalid native file delete intent"
        );
        proof(intent)?;
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((old, state)) = tx
            .query_row(
                "SELECT intent,state FROM local_native_deletes WHERE local_id=?1",
                [&p.local_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
            && state != "cancelled"
        {
            ensure!(
                serde_json::from_str::<NativeDeleteIntent>(&old)? == *intent,
                "another native delete needs recovery"
            );
            consume_history(&tx, intent)?;
            tx.commit()?;
            return Ok(());
        }
        ensure!(
            projection::get(&tx, &p.local_id)?.as_ref() == Some(p),
            "native delete publication changed"
        );
        let physical = p.volume_serial == Some(intent.physical.volume_serial)
            && p.native_file_id.as_deref()
                == Some(format!("{:016x}", intent.physical.file_id).as_str());
        let captured = projection::capture(&tx, &p.local_id)?;
        ensure!(
            physical || captured.is_some_and(|c| c.physical == intent.physical),
            "native delete physical identity changed"
        );
        ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM files WHERE remote_id=?1 AND cloud_remote_id=?2 AND path=?3 AND (is_dir=1 OR etag=?4) AND is_dir=?5)",params![p.local_id,p.cloud_id,p.path,v.etag,p.directory],|r|r.get::<_,bool>(0))?,"native delete cloud version changed");
        if p.directory {
            ensure!(
                !tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM files WHERE substr(path,1,length(?1)+1)=?1||'/')",
                    [&p.path],
                    |r| r.get::<_, bool>(0)
                )?,
                "directory delete waits for owned descendants"
            );
        }
        if has_journal_record(&tx, &p.local_id)? {
            let state = snapshot(&tx, &p.local_id)?;
            ensure!(
                state.readers == 0
                    && state.writers == 0
                    && state.upload_generation.is_none()
                    && !state.uncertain_upload
                    && !state.pending_operations,
                "native delete has active or uncertain cache work"
            );
            ensure!(
                captured.is_none_or(|c| c.generation == state.generation),
                "native delete captured generation changed"
            );
        }
        ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM pending_metadata_operations op JOIN files f ON f.remote_id=op.local_id WHERE ?1=f.path OR substr(?1,1,length(f.path)+1)=f.path||'/')",[&p.path],|r|r.get::<_,bool>(0))?,"native delete has pending parent metadata");
        for (sequence, payload) in [
            (intent.request_sequence, &intent.request_payload),
            (intent.completion_sequence, &intent.completion_payload),
        ] {
            ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM native_event_log WHERE sequence=?1 AND binding=?2 AND payload=?3)",params![sequence,p.root_binding,payload],|r|r.get::<_,bool>(0))?,"native delete kernel proof no longer durable");
        }
        tx.execute("INSERT INTO local_native_deletes(local_id,root_binding,intent,state) VALUES(?1,?2,?3,'prepared') ON CONFLICT(local_id) DO UPDATE SET root_binding=excluded.root_binding,intent=excluded.intent,state='prepared'",params![p.local_id,p.root_binding,serde_json::to_string(intent)?])?;
        consume_history(&tx, intent)?;
        tx.commit()?;
        Ok(())
    }
    pub fn cancel_prepared_native_delete(&self, id: &str) -> anyhow::Result<bool> {
        Ok(journal_connection(&self.0.db)?.execute("UPDATE local_native_deletes SET state='cancelled' WHERE local_id=?1 AND state='prepared'",[id])?==1)
    }
    pub fn dispatch_native_delete(&self, id: &str) -> anyhow::Result<bool> {
        let conn = journal_connection(&self.0.db)?;
        let (raw, state): (String, String) = conn.query_row(
            "SELECT intent,state FROM local_native_deletes WHERE local_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if matches!(state.as_str(), "received" | "applied") {
            return Ok(false);
        }
        let intent: NativeDeleteIntent = serde_json::from_str(&raw)?;
        ensure!(
            state == "prepared" && crate::now_unix() >= intent.not_before_unix,
            "native delete is deferred or its outcome is uncertain"
        );
        ensure!(conn.execute("UPDATE local_native_deletes SET state='dispatched' WHERE local_id=?1 AND state='prepared' AND intent=?2",params![id,raw])?==1,"native delete dispatch changed");
        Ok(true)
    }
    /// Call only for a definite rejection before any mutation request.
    pub fn native_delete_not_dispatched(&self, id: &str) -> anyhow::Result<()> {
        ensure!(journal_connection(&self.0.db)?.execute("UPDATE local_native_deletes SET state='prepared' WHERE local_id=?1 AND state='dispatched'",[id])?==1,"native delete dispatch fence changed");
        Ok(())
    }
    /// A successful conditional recycle or an authority-bound missing-ID readback.
    pub fn receive_native_delete(&self, id: &str) -> anyhow::Result<()> {
        ensure!(journal_connection(&self.0.db)?.execute("UPDATE local_native_deletes SET state='received' WHERE local_id=?1 AND state='dispatched'",[id])?==1,"native delete receipt fence changed");
        Ok(())
    }
    /// A bound-ID read proves the originally compared version still exists.
    pub fn retry_native_delete_unchanged(
        &self,
        id: &str,
        current: &MetadataEntry,
    ) -> anyhow::Result<()> {
        let (intent, state) = self.native_delete(id)?.context("native delete missing")?;
        ensure!(
            state == "dispatched" && current == &intent.version,
            "native delete readback is not the original version"
        );
        ensure!(journal_connection(&self.0.db)?.execute("UPDATE local_native_deletes SET state='prepared' WHERE local_id=?1 AND state='dispatched' AND intent=?2",params![id,serde_json::to_string(&intent)?])?==1,"native delete reconciliation fence changed");
        Ok(())
    }
    /// The adapter has re-observed the same owned directory as empty, including
    /// its original parent identity. Refresh only this collection's recycle CAS.
    pub fn retry_native_empty_directory(
        &self,
        id: &str,
        current: &MetadataEntry,
    ) -> anyhow::Result<()> {
        let (mut intent, state) = self
            .native_delete(id)?
            .context("native directory delete missing")?;
        ensure!(
            state == "dispatched"
                && intent.publication.directory
                && current.is_dir
                && current.remote_id == intent.version.remote_id
                && current.path == intent.version.path
                && !current.etag.is_empty(),
            "native directory recycle readback changed scope"
        );
        let old = serde_json::to_string(&intent)?;
        intent.version = current.clone();
        ensure!(journal_connection(&self.0.db)?.execute("UPDATE local_native_deletes SET intent=?2,state='prepared' WHERE local_id=?1 AND state='dispatched' AND intent=?3",params![id,serde_json::to_string(&intent)?,old])?==1,"native directory reconciliation fence changed");
        Ok(())
    }
    pub fn apply_native_delete(&self, id: &str) -> anyhow::Result<()> {
        let (intent, state) = self.native_delete(id)?.context("native delete missing")?;
        if state == "applied" {
            return Ok(());
        }
        ensure!(state == "received", "native delete has no receipt");
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let record = self.record(id)?;
        let _file = if record.cache_path.is_some() {
            let file = self.file(&record, false)?;
            file.try_lock()?;
            Some(file)
        } else {
            None
        };
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(
            projection::get(&tx, id)?.as_ref() == Some(&intent.publication)
                && record.metadata.path == intent.version.path
                && record.cloud_remote_id.as_deref() == Some(intent.version.remote_id.as_str())
                && (record.metadata.is_dir || record.metadata.etag == intent.version.etag),
            "native delete owner changed before retirement"
        );
        if has_journal_record(&tx, id)? {
            let s = snapshot(&tx, id)?;
            ensure!(
                s.readers == 0
                    && s.writers == 0
                    && s.upload_generation.is_none()
                    && !s.uncertain_upload,
                "native delete has active cache leases"
            );
        }
        let archive = format!("native-deleted-archive-{}", intent.completion_sequence);
        let record_json = serde_json::json!({"metadata":record.metadata,"cloud_id":record.cloud_remote_id,"state":format!("{:?}",record.state),"local_mode":record.local_mode,"receipt":intent.root_receipt,"physical":intent.physical});
        tx.execute("INSERT INTO local_native_deleted_archives(archive_id,local_id,root_binding,record_json,cache_path,deleted_unix) VALUES(?1,?2,?3,?4,?5,?6)",params![archive,id,intent.publication.root_binding,record_json.to_string(),record.cache_path.as_ref().map(|p|p.to_string_lossy()),crate::now_unix()])?;
        // Keep an independent journal reference under a private archive key:
        // pruning cannot discard revisions or collide with a restored cloud ID.
        tx.execute(
            "UPDATE local_cache_journal SET local_id=?2 WHERE local_id=?1",
            params![id, archive],
        )?;
        tx.execute(
            "DELETE FROM local_native_dirty WHERE cloud_id=?1 OR cloud_id=?2",
            params![id, intent.version.remote_id],
        )?;
        for table in [
            "local_native_projections",
            "local_native_captures",
            "local_native_creations",
            "local_native_directory_transfers",
            "local_native_moves",
        ] {
            tx.execute(&format!("DELETE FROM {table} WHERE local_id=?1"), [id])?;
        }
        ensure!(
            tx.execute("DELETE FROM files WHERE remote_id=?1", [id])? == 1,
            "native deleted file changed"
        );
        ensure!(tx.execute("UPDATE local_native_deletes SET state='applied' WHERE local_id=?1 AND state='received' AND intent=?2",params![id,serde_json::to_string(&intent)?])?==1,"native delete completion fence changed");
        tx.commit()?;
        Ok(())
    }
    pub fn native_deleted_archive_count(&self) -> anyhow::Result<usize> {
        Ok(journal_connection(&self.0.db)?.query_row(
            "SELECT count(*) FROM local_native_deleted_archives",
            [],
            |r| r.get(0),
        )?)
    }
}
