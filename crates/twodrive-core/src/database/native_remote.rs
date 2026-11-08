//! Version changes are prepared before native metadata/dehydration. Old private
//! bytes retain an independent owner; a pending publication cannot hydrate.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NativeRemoteUpdate {
    pub sequence: i64,
    pub expected: NativeProjection,
    pub next: MetadataEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_modified_filetime: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_pin: Option<i32>,
    pub state: String,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NativeRemoteRemoval {
    pub sequence: i64,
    pub expected: NativeProjection,
    pub state: String,
}
impl CacheJournal {
    pub fn native_remote_removals(&self, root: &str) -> anyhow::Result<Vec<NativeRemoteRemoval>> {
        let conn = journal_connection(&self.0.db)?;
        let mut stmt=conn.prepare("SELECT sequence,payload,state FROM local_native_remote_removals WHERE root_binding=?1 AND state='prepared' ORDER BY sequence")?;
        stmt.query_map([root], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .map(|row| {
            let (sequence, payload, state) = row?;
            let mut removal: NativeRemoteRemoval = serde_json::from_str(&payload)?;
            removal.sequence = sequence;
            removal.state = state;
            Ok(removal)
        })
        .collect()
    }
    pub fn prepare_native_remote_removal(
        &self,
        expected: &NativeProjection,
        verify: impl FnOnce(&mut File, &str) -> anyhow::Result<()>,
    ) -> anyhow::Result<NativeRemoteRemoval> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let record = self.record(&expected.local_id)?;
        let mut held = None;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(
            expected.state == "published"
                && projection::get(&tx, &expected.local_id)?.as_ref() == Some(expected)
                && record.cloud_remote_id.as_deref() == Some(expected.cloud_id.as_str())
                && record.metadata.path == expected.path
                && record.metadata.etag == expected.etag,
            "remote removal baseline changed"
        );
        if has_journal_record(&tx, &expected.local_id)? {
            let s = snapshot(&tx, &expected.local_id)?;
            ensure!(
                s.sync_confirmed() && s.readers == 0,
                "remote removal preserves local work"
            );
            let mut file = self.file(&record, false)?;
            file.try_lock()?;
            verify(&mut file, &s.acknowledged_digest)?;
            held = Some(file);
        } else {
            ensure!(
                record.cache_path.is_none()
                    && (matches!(record.state, FileState::OnlineOnly | FileState::Cached)
                        || (expected.directory && record.state == FileState::Pinned)),
                "remote removal has local state"
            );
        }
        ensure!(
            !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM files WHERE substr(path,1,length(?1)+1)=?1||'/')",
                [&expected.path],
                |r| r.get::<_, bool>(0)
            )?,
            "remote directory removal waits for descendants"
        );
        ensure!(
            !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM native_event_log WHERE binding=?1)",
                [&expected.root_binding],
                |r| r.get::<_, bool>(0)
            )?,
            "remote removal has native namespace work"
        );
        ensure!(
            !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM local_native_dirty WHERE cloud_id=?1 OR cloud_id=?2)",
                params![expected.local_id, expected.cloud_id],
                |r| r.get::<_, bool>(0)
            )?,
            "remote removal preserves uncaptured local work"
        );
        ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM pending_metadata_operations WHERE local_id=?1) OR EXISTS(SELECT 1 FROM local_native_moves WHERE local_id=?1 AND state<>'applied') OR EXISTS(SELECT 1 FROM local_native_deletes WHERE local_id=?1 AND state NOT IN ('applied','cancelled'))",[&expected.local_id],|r|r.get::<_,bool>(0))?,"remote removal has local namespace work");
        let mut removal = NativeRemoteRemoval {
            sequence: 0,
            expected: expected.clone(),
            state: "prepared".into(),
        };
        tx.execute("INSERT INTO local_native_remote_removals(local_id,root_binding,payload,state) VALUES(?1,?2,?3,'prepared')",params![expected.local_id,expected.root_binding,serde_json::to_string(&removal)?])?;
        removal.sequence = tx.last_insert_rowid();
        tx.execute(
            "UPDATE local_native_projections SET state='removing' WHERE local_id=?1",
            [&expected.local_id],
        )?;
        tx.commit()?;
        drop(held);
        Ok(removal)
    }
    /// The native object is deleted under its owned handle, or the retained
    /// decision is resumed after that exact object has already disappeared.
    pub fn complete_native_remote_removal(
        &self,
        removal: &NativeRemoteRemoval,
    ) -> anyhow::Result<()> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let payload:String=tx.query_row("SELECT payload FROM local_native_remote_removals WHERE sequence=?1 AND state='prepared'",[removal.sequence],|r|r.get(0))?;
        ensure!(
            serde_json::from_str::<NativeRemoteRemoval>(&payload)?.expected == removal.expected,
            "remote removal decision changed"
        );
        let mut expected = removal.expected.clone();
        expected.state = "removing".into();
        ensure!(
            projection::get(&tx, &expected.local_id)?.as_ref() == Some(&expected),
            "remote removal publication changed"
        );
        if has_journal_record(&tx, &expected.local_id)? {
            let s = snapshot(&tx, &expected.local_id)?;
            ensure!(
                s.readers == 0
                    && s.writers == 0
                    && !s.dirty_intent
                    && s.upload_generation.is_none()
                    && !s.uncertain_upload
                    && !s.native_dirty,
                "remote removal retains local revisions"
            );
            let archive = format!("native-remote-deleted-{}", removal.sequence);
            tx.execute(
                "UPDATE local_cache_journal SET local_id=?2 WHERE local_id=?1",
                params![expected.local_id, archive],
            )?;
        }
        for table in [
            "local_native_projections",
            "local_native_captures",
            "local_native_creations",
            "local_native_directory_transfers",
            "local_native_moves",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE local_id=?1"),
                [&expected.local_id],
            )?;
        }
        tx.execute("DELETE FROM files WHERE remote_id=?1", [&expected.local_id])?;
        tx.execute("UPDATE local_native_remote_removals SET state='applied' WHERE sequence=?1 AND state='prepared'",[removal.sequence])?;
        tx.commit()?;
        Ok(())
    }
    pub fn native_remote_updates(&self, root: &str) -> anyhow::Result<Vec<NativeRemoteUpdate>> {
        let conn = journal_connection(&self.0.db)?;
        let mut stmt=conn.prepare("SELECT sequence,payload,state FROM local_native_remote_updates WHERE root_binding=?1 AND state='prepared' ORDER BY sequence")?;
        stmt.query_map([root], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .map(|row| {
            let (sequence, payload, state) = row?;
            let mut update: NativeRemoteUpdate = serde_json::from_str(&payload)?;
            update.sequence = sequence;
            update.state = state;
            Ok(update)
        })
        .collect()
    }
    /// Caller holds the native no-write/no-delete lease and independently
    /// verifies that its bytes/metadata are still clean before this transition.
    pub fn prepare_native_remote_update(
        &self,
        expected: &NativeProjection,
        next: &MetadataEntry,
        verify: impl FnOnce(&mut File, &str) -> anyhow::Result<()>,
    ) -> anyhow::Result<NativeRemoteUpdate> {
        self.prepare_native_update(expected, next, None, None, verify)
    }
    pub fn prepare_native_migration_time(
        &self,
        expected: &NativeProjection,
        filetime: u64,
        verify: impl FnOnce(&mut File, &str) -> anyhow::Result<()>,
    ) -> anyhow::Result<NativeRemoteUpdate> {
        self.prepare_native_migration_time_with_pin(expected, filetime, None, verify)
    }
    pub fn prepare_native_migration_time_with_pin(
        &self,
        expected: &NativeProjection,
        filetime: u64,
        pin: Option<i32>,
        verify: impl FnOnce(&mut File, &str) -> anyhow::Result<()>,
    ) -> anyhow::Result<NativeRemoteUpdate> {
        ensure!(
            pin.is_none_or(|p| matches!(p, 0..=2)),
            "invalid stored target pin"
        );
        ensure!(
            !expected.directory && filetime >= 116_444_736_000_000_000,
            "valid source file timestamp required"
        );
        let next = MetadataEntry::new_file(
            &expected.cloud_id,
            &expected.path,
            expected.size,
            (filetime / 10_000_000) as i64 - 11_644_473_600,
            &expected.etag,
        );
        self.prepare_native_update(expected, &next, Some(filetime), pin, verify)
    }
    fn prepare_native_update(
        &self,
        expected: &NativeProjection,
        next: &MetadataEntry,
        local_modified_filetime: Option<u64>,
        local_pin: Option<i32>,
        verify: impl FnOnce(&mut File, &str) -> anyhow::Result<()>,
    ) -> anyhow::Result<NativeRemoteUpdate> {
        ensure!(
            expected.state == "published"
                && expected.directory == next.is_dir
                && next.remote_id == expected.cloud_id
                && next.path != "/"
                && !next.etag.is_empty()
                && (if local_modified_filetime.is_some() {
                    !expected.directory
                        && next.etag == expected.etag
                        && next.path == expected.path
                        && next.size == expected.size
                } else {
                    next.etag != expected.etag || next.path != expected.path
                }),
            "invalid remote native version change"
        );
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let record = self.record(&expected.local_id)?;
        let mut held = None;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(
            projection::get(&tx, &expected.local_id)?.as_ref() == Some(expected)
                && record.cloud_remote_id.as_deref() == Some(expected.cloud_id.as_str())
                && record.metadata.path == expected.path
                && record.metadata.etag == expected.etag,
            "remote native baseline changed"
        );
        if has_journal_record(&tx, &expected.local_id)? {
            let state = snapshot(&tx, &expected.local_id)?;
            ensure!(
                state.sync_confirmed() && state.readers == 0 && !state.native_dirty,
                "remote update has local/pending work"
            );
            let mut file = self.file(&record, false)?;
            file.try_lock()?;
            verify(&mut file, &state.acknowledged_digest)?;
            held = Some(file);
        } else {
            ensure!(
                local_modified_filetime.is_none(),
                "source timestamp requires confirmed private bytes"
            );
            ensure!((record.state==FileState::OnlineOnly || (record.state==FileState::Hydrating && record.effective_pinned()) || (expected.directory && matches!(record.state,FileState::Cached | FileState::Pinned))) && record.cache_path.is_none()
                && !tx.query_row("SELECT EXISTS(SELECT 1 FROM local_native_dirty WHERE cloud_id=?1 OR cloud_id=?2)",params![expected.local_id,expected.cloud_id],|r|r.get::<_,bool>(0))?,"remote update has uncaptured local work");
        }
        ensure!(
            !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM native_event_log WHERE binding=?1)",
                [&expected.root_binding],
                |r| r.get::<_, bool>(0)
            )?,
            "remote update has unresolved native namespace"
        );
        ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM files f LEFT JOIN local_cache_journal j ON j.local_id=f.remote_id WHERE substr(f.path,1,length(?1)+1)=?1||'/' AND (f.state IN ('dirty','writing','uploading','conflict') OR j.upload_generation IS NOT NULL OR j.uncertain_upload=1 OR j.writers>0))",[&expected.path],|r|r.get::<_,bool>(0))?,"remote directory change preserves local child work");
        ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM pending_metadata_operations WHERE local_id=?1) OR EXISTS(SELECT 1 FROM local_native_moves WHERE local_id=?1 AND state<>'applied') OR EXISTS(SELECT 1 FROM local_native_deletes WHERE local_id=?1 AND state NOT IN ('applied','cancelled'))",[&expected.local_id],|r|r.get::<_,bool>(0))?,"remote update has local namespace work");
        let mut update = NativeRemoteUpdate {
            sequence: 0,
            expected: expected.clone(),
            next: next.clone(),
            local_modified_filetime,
            local_pin,
            state: "prepared".into(),
        };
        tx.execute("INSERT INTO local_native_remote_updates(local_id,root_binding,payload,state) VALUES(?1,?2,?3,'prepared')",params![expected.local_id,expected.root_binding,serde_json::to_string(&update)?])?;
        update.sequence = tx.last_insert_rowid();
        ensure!(tx.execute("UPDATE local_native_projections SET state='updating' WHERE local_id=?1 AND state='published'",[&expected.local_id])?==1,"native update publication changed");
        tx.commit()?;
        drop(held);
        Ok(update)
    }
    /// Persist a later explicit pin observation before native APIs can erase it.
    pub fn refresh_native_migration_pin(
        &self,
        update: &NativeRemoteUpdate,
        pin: i32,
    ) -> anyhow::Result<NativeRemoteUpdate> {
        ensure!(
            update.local_modified_filetime.is_some() && matches!(pin, 1..=2),
            "explicit local pin required"
        );
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw: String = tx.query_row("SELECT payload FROM local_native_remote_updates WHERE sequence=?1 AND state='prepared'", [update.sequence], |r| r.get(0))?;
        let mut stored: NativeRemoteUpdate = serde_json::from_str(&raw)?;
        stored.sequence = update.sequence;
        ensure!(&stored == update, "local pin decision changed");
        stored.local_pin = Some(pin);
        ensure!(tx.execute("UPDATE local_native_remote_updates SET payload=?2 WHERE sequence=?1 AND state='prepared' AND payload=?3", params![update.sequence, serde_json::to_string(&stored)?, raw])? == 1, "local pin fence changed");
        tx.commit()?;
        Ok(stored)
    }
    /// A durable, identity-bound namespace completion supersedes only local
    /// timestamp work. It grants no authority to mutate cloud/native content.
    pub fn cancel_native_migration_namespace(
        &self,
        update: &NativeRemoteUpdate,
        receipt: &str,
        sequence: i64,
        payload: &str,
    ) -> anyhow::Result<()> {
        ensure!(
            update.local_modified_filetime.is_some(),
            "only local metadata may be cancelled"
        );
        let root: serde_json::Value = serde_json::from_str(receipt)?;
        let event: serde_json::Value = serde_json::from_str(payload)?;
        let p = &update.expected;
        ensure!(
            root["schema_version"] == 1
                && root["file_id"].as_u64().is_some_and(|id| id != 0)
                && root["identity"] == serde_json::to_value(p.root_binding.as_bytes())?
                && root["volume_serial"].as_u64() == p.volume_serial.map(u64::from)
                && event["schema_version"] == 1
                && event["root_file_id"] == root["file_id"]
                && event["volume_serial"] == root["volume_serial"]
                && event["identity"] == serde_json::to_value(&p.tag)?
                && event["file_id"].as_u64()
                    == p.native_file_id
                        .as_deref()
                        .and_then(|s| u64::from_str_radix(s, 16).ok())
                && matches!(
                    event["kind"].as_str(),
                    Some("rename_completed" | "delete_completed")
                ),
            "local metadata namespace proof changed"
        );
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM native_event_log WHERE binding=?1 AND sequence=?2 AND payload=?3)",params![p.root_binding,sequence,payload],|r|r.get::<_,bool>(0))?, "namespace completion no longer retained");
        let raw:String=tx.query_row("SELECT payload FROM local_native_remote_updates WHERE sequence=?1 AND state='prepared'",[update.sequence],|r|r.get(0))?;
        let mut stored: NativeRemoteUpdate = serde_json::from_str(&raw)?;
        stored.sequence = update.sequence;
        ensure!(&stored == update, "local metadata decision changed");
        let mut expected = p.clone();
        expected.state = "updating".into();
        ensure!(
            projection::get(&tx, &p.local_id)?.as_ref() == Some(&expected),
            "local metadata publication changed"
        );
        tx.execute(
            "UPDATE local_native_projections SET state='published' WHERE local_id=?1",
            [&p.local_id],
        )?;
        tx.execute(
            "UPDATE local_native_remote_updates SET state='cancelled' WHERE sequence=?1",
            [update.sequence],
        )?;
        tx.execute("UPDATE local_native_source_imports SET state='isolated' WHERE local_id=?1 AND state IN ('verified','data_ready')",[&p.local_id])?;
        tx.commit()?;
        Ok(())
    }
    /// Only a local timestamp transition may be cancelled on positive native
    /// edits. Preserve the newer bytes and require a fresh complete capture.
    pub fn supersede_native_migration_time(
        &self,
        update: &NativeRemoteUpdate,
    ) -> anyhow::Result<()> {
        ensure!(
            update.local_modified_filetime.is_some(),
            "only local metadata intent can be superseded"
        );
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let payload:String=tx.query_row("SELECT payload FROM local_native_remote_updates WHERE sequence=?1 AND state='prepared'",[update.sequence],|r|r.get(0))?;
        let stored: NativeRemoteUpdate = serde_json::from_str(&payload)?;
        ensure!(
            stored.expected == update.expected
                && stored.next == update.next
                && stored.local_modified_filetime == update.local_modified_filetime
                && stored.local_pin == update.local_pin,
            "local metadata intent changed"
        );
        let mut expected = update.expected.clone();
        expected.state = "updating".into();
        ensure!(
            projection::get(&tx, &expected.local_id)?.as_ref() == Some(&expected),
            "local metadata publication changed"
        );
        tx.execute(
            "UPDATE local_native_projections SET state='published' WHERE local_id=?1",
            [&expected.local_id],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO local_native_dirty(cloud_id) VALUES(?1)",
            [&expected.local_id],
        )?;
        tx.execute(
            "UPDATE files SET state='dirty' WHERE remote_id=?1",
            [&expected.local_id],
        )?;
        tx.execute(
            "UPDATE local_native_remote_updates SET state='cancelled' WHERE sequence=?1",
            [update.sequence],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn complete_native_remote_update(
        &self,
        update: &NativeRemoteUpdate,
        physical: NativeFileIdentity,
    ) -> anyhow::Result<()> {
        let before = self.record(&update.expected.local_id)?;
        if update.expected.path != update.next.path {
            if before.metadata.path == update.expected.path {
                self.0
                    .db
                    .move_subtree_and_queue(&update.expected.local_id, &update.next.path)?;
            } else {
                ensure!(
                    before.metadata.path == update.next.path,
                    "remote namespace recovery path changed"
                );
            }
        }
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut expected = update.expected.clone();
        let payload:String=tx.query_row("SELECT payload FROM local_native_remote_updates WHERE sequence=?1 AND state='prepared'",[update.sequence],|r|r.get(0))?;
        let stored: NativeRemoteUpdate = serde_json::from_str(&payload)?;
        ensure!(
            stored.expected == update.expected
                && stored.next == update.next
                && stored.local_modified_filetime == update.local_modified_filetime
                && stored.local_pin == update.local_pin,
            "native remote update receipt changed"
        );
        expected.state = "updating".into();
        ensure!(
            projection::get(&tx, &expected.local_id)?.as_ref() == Some(&expected)
                && expected.volume_serial == Some(physical.volume_serial)
                && expected.native_file_id.as_deref()
                    == Some(format!("{:016x}", physical.file_id).as_str()),
            "native remote completion identity changed"
        );
        let record = self.record(&expected.local_id)?;
        ensure!(
            record.metadata.path == update.next.path
                && record.cloud_remote_id.as_deref() == Some(expected.cloud_id.as_str())
                && record.metadata.etag == expected.etag,
            "native remote completion data changed"
        );
        if has_journal_record(&tx, &expected.local_id)? {
            let state = snapshot(&tx, &expected.local_id)?;
            ensure!(
                state.readers == 0
                    && state.writers == 0
                    && state.upload_generation.is_none()
                    && !state.uncertain_upload
                    && !state.dirty_intent
                    && !state.native_dirty,
                "native remote completion has local edits"
            );
            if update.local_modified_filetime.is_none() {
                let archive = format!("native-remote-before-{}", update.sequence);
                tx.execute(
                    "UPDATE local_cache_journal SET local_id=?2 WHERE local_id=?1",
                    params![expected.local_id, archive],
                )?;
            }
        }
        if expected.path != update.next.path {
            let mut stmt=tx.prepare("SELECT remote_id,path FROM files WHERE path=?1 OR substr(path,1,length(?1)+1)=?1||'/'")?;
            let rows = stmt
                .query_map([&update.next.path], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            for (child, path) in rows {
                let old = format!("{}{}", expected.path, &path[update.next.path.len()..]);
                tx.execute("UPDATE local_native_projections SET path=?2 WHERE local_id=?1 AND root_binding=?3 AND path=?4",params![child,path,expected.root_binding,old])?;
                tx.execute("UPDATE local_cache_journal SET cloud_path=?2 WHERE local_id=?1 AND cloud_path=?3",params![child,path,old])?;
                tx.execute(
                    "UPDATE local_native_creations SET path=?2 WHERE local_id=?1 AND path=?3",
                    params![child, path, old],
                )?;
            }
            tx.execute(
                "DELETE FROM pending_metadata_operations WHERE local_id=?1 AND path=?2",
                params![expected.local_id, update.next.path],
            )?;
        }
        if update.local_modified_filetime.is_some() {
            ensure!(
                has_journal_record(&tx, &expected.local_id)?,
                "source timestamp private data missing"
            );
            tx.execute(
                "UPDATE files SET modified_unix=?2 WHERE remote_id=?1",
                params![expected.local_id, update.next.modified_unix],
            )?;
        } else {
            tx.execute("UPDATE files SET etag=?2,size=?3,modified_unix=?4,state=CASE WHEN is_dir=1 THEN 'cached' ELSE 'online_only' END,cache_path=NULL,cache_accessed_unix=NULL,download_generation=download_generation+1 WHERE remote_id=?1",params![expected.local_id,update.next.etag,i64::try_from(update.next.size)?,update.next.modified_unix])?;
        }

        tx.execute("UPDATE local_native_projections SET etag=?2,size=?3,modified_unix=?4,state='published' WHERE local_id=?1",params![expected.local_id,update.next.etag,i64::try_from(update.next.size)?,update.next.modified_unix])?;
        ensure!(tx.execute("UPDATE local_native_remote_updates SET state='applied' WHERE sequence=?1 AND state='prepared'",[update.sequence])?==1,"native remote update receipt changed");
        tx.commit()?;
        Ok(())
    }
}
