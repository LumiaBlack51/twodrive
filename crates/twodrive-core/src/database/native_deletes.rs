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
/// Canonicalize accepted older representations in the same schema-upgrade
/// transaction, before any recovery CAS uses their serialized form.
pub(super) fn upgrade_intents(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let mut statement = conn.prepare("SELECT local_id,intent FROM local_native_deletes")?;
    let rows = statement
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (id, raw) in rows {
        let intent: NativeDeleteIntent = serde_json::from_str(&raw)?;
        ensure!(
            intent.publication.local_id == id,
            "legacy delete identity changed"
        );
        conn.execute(
            "UPDATE local_native_deletes SET intent=?2 WHERE local_id=?1 AND intent=?3",
            params![id, serde_json::to_string(&intent)?, raw],
        )?;
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

    /// The adapter has freshly observed no surviving/replacement native object.
    /// Retain late kernel hints as inactive audit evidence after the exact
    /// cloud retirement, without authorizing another cloud operation.
    pub fn archive_retired_native_events(
        &self,
        intent: &NativeDeleteIntent,
        decisions: &[(i64, String)],
    ) -> anyhow::Result<()> {
        proof(intent)?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = &intent.publication.local_id;
        ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM local_native_deletes WHERE local_id=?1 AND state='applied' AND intent=?2)",params![id,serde_json::to_string(intent)?],|r|r.get::<_,bool>(0))?,"retired native decision changed");
        if let Some(live) = projection::get(&tx, id)? {
            ensure!(
                live.root_binding == intent.publication.root_binding
                    && live.tag != intent.publication.tag
                    && ((live.state == "preparing"
                        && live.volume_serial.is_none()
                        && live.native_file_id.is_none())
                        || (live.state == "published"
                            && live.volume_serial.is_some()
                            && live.native_file_id.is_some()
                            && (live.volume_serial != Some(intent.physical.volume_serial)
                                || live.native_file_id.as_deref()
                                    != Some(
                                        format!("{:016x}", intent.physical.file_id).as_str()
                                    )))),
                "retired identity has a live owner"
            );
        } else {
            // Re-imported clean cloud metadata may precede a new publication;
            // it must not be starved by an old physical identity's hints.
            ensure!(!has_journal_record(&tx,id)? && !tx.query_row("SELECT EXISTS(SELECT 1 FROM files WHERE remote_id=?1 AND state IN ('dirty','writing','uploading','conflict')) OR EXISTS(SELECT 1 FROM local_native_creations WHERE local_id=?1) OR EXISTS(SELECT 1 FROM local_native_captures WHERE local_id=?1)",[id],|r|r.get::<_,bool>(0))?,"retired identity has an unpublished local owner");
        }
        let archive = format!("native-deleted-archive-{}", intent.completion_sequence);
        let raw:String=tx.query_row("SELECT record_json FROM local_native_deleted_archives WHERE archive_id=?1 AND local_id=?2",params![archive,id],|r|r.get(0))?;
        let mut record: serde_json::Value = serde_json::from_str(&raw)?;
        let root: serde_json::Value = serde_json::from_str(&intent.root_receipt)?;
        let audit = record
            .as_object_mut()
            .context("invalid retired archive")?
            .entry("post_retirement_events")
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .context("invalid retired event audit")?;
        for (sequence, payload) in decisions {
            let e: serde_json::Value = serde_json::from_str(payload)?;
            ensure!(
                *sequence > intent.completion_sequence
                    && e["schema_version"] == 1
                    && e["root_file_id"] == root["file_id"]
                    && e["volume_serial"] == root["volume_serial"]
                    && e["file_id"].as_u64() == Some(intent.physical.file_id)
                    && e["identity"] == serde_json::to_value(&intent.publication.tag)?
                    && matches!(
                        e["kind"].as_str(),
                        Some(
                            "rename_requested"
                                | "rename_completed"
                                | "delete_requested"
                                | "delete_completed"
                        )
                    ),
                "late event belongs to another native identity"
            );
            ensure!(
                tx.execute(
                    "DELETE FROM native_event_log WHERE binding=?1 AND sequence=?2 AND payload=?3",
                    params![intent.publication.root_binding, sequence, payload]
                )? == 1,
                "late native event changed"
            );
            audit.push(serde_json::json!({"sequence":sequence,"event":e}));
        }
        tx.execute("UPDATE local_native_deleted_archives SET record_json=?2 WHERE archive_id=?1 AND record_json=?3",params![archive,record.to_string(),raw])?;
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
        // The source replica owns its bytes independently of the retired target.
        tx.execute("UPDATE local_native_source_imports SET state='isolated' WHERE local_id=?1 AND state IN ('verified','data_ready')", [id])?;
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_four_delete_json_recovers_prepared_dispatched_and_received() {
        for phase in ["prepared", "dispatched", "received"] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let cache = root.join("cache");
            fs::create_dir(&cache).unwrap();
            let db = Database::new(root.join("db.sqlite3"));
            db.init().unwrap();
            let version = MetadataEntry::new_file("generated-id", "/generated.txt", 0, 1, "v1");
            db.upsert_metadata(&version).unwrap();
            let j = CacheJournal::open(db.clone(), &cache, &"a".repeat(64)).unwrap();
            let tag = b"generated-legacy-delete-tag";
            j.prepare_native_projection("generated-id", "generated-root", tag)
                .unwrap();
            j.complete_native_projection("generated-id", "generated-root", tag, 7, 9)
                .unwrap();
            let event = |kind: &str| {
                serde_json::json!({"schema_version":1,"kind":kind,"identity":tag,"volume_serial":7,"root_file_id":8,"file_id":9,"connection_key":1,"request_key":2}).to_string()
            };
            let request = event("delete_requested");
            let completed = event("delete_completed");
            let first = db.append_native_event("generated-root", &request).unwrap();
            let last = db
                .append_native_event("generated-root", &completed)
                .unwrap();
            let intent=NativeDeleteIntent {directory_parent_id:None,publication:j.native_projection_for_id("generated-id").unwrap().unwrap(),version:version.clone(),root_receipt:serde_json::json!({"schema_version":1,"identity":b"generated-root","volume_serial":7,"file_id":8}).to_string(),physical:NativeFileIdentity{volume_serial:7,file_id:9},request_sequence:first,request_payload:request,completion_sequence:last,completion_payload:completed,not_before_unix:0};
            // Seed the actual legacy representation, not a newly serialized struct.
            let mut legacy = serde_json::to_value(&intent).unwrap();
            legacy
                .as_object_mut()
                .unwrap()
                .remove("directory_parent_id");
            let raw = legacy.to_string();
            assert!(!raw.contains("directory_parent_id"));
            let conn = journal_connection(&db).unwrap();
            conn.execute("INSERT INTO local_native_deletes(local_id,root_binding,intent,state) VALUES('generated-id','generated-root',?1,?2)",params![raw,phase]).unwrap();
            db.set_state_value("local_cache_schema", "4").unwrap();
            drop(conn);
            drop(j);
            let j = CacheJournal::open(db.clone(), &cache, &"a".repeat(64)).unwrap();
            let (restored, state) = j.native_delete("generated-id").unwrap().unwrap();
            assert_eq!(restored, intent);
            assert_eq!(state, phase);
            j.acknowledge_native_delete_history(&restored).unwrap();
            assert!(
                db.pending_native_events("generated-root", 100)
                    .unwrap()
                    .is_empty()
            );
            if phase == "dispatched" {
                j.retry_native_delete_unchanged("generated-id", &version)
                    .unwrap();
            }
            if phase != "received" {
                assert!(j.dispatch_native_delete("generated-id").unwrap());
                j.receive_native_delete("generated-id").unwrap();
            }
            j.apply_native_delete("generated-id").unwrap();
            assert!(db.get_by_remote_id("generated-id").unwrap().is_none());
        }
    }
}
