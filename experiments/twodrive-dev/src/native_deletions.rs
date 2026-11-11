use super::*;
use crate::model::CloudBackend;
use twodrive_core::{NativeDeleteIntent, NativeFileIdentity};
use twodrive_local_files::native::persistent::{NativeEntry, NativeEvent, NativeEventKind};

#[derive(Default)]
pub(super) struct DeletionProgress {
    pub deleted: usize,
    pub waiting: usize,
    pub error: Option<String>,
}
#[cfg(all(test, feature = "native-test-hooks"))]
thread_local! {pub(super) static INTERRUPT_AFTER_DELETE_PREPARE:std::cell::Cell<bool>=const{std::cell::Cell::new(false)};}
impl NativeCoordinator {
    fn supersedes_delete(
        entries: &[NativeEntry],
        intent: &NativeDeleteIntent,
    ) -> anyhow::Result<bool> {
        let relative = Self::relative(&intent.publication.path)?;
        Ok(entries.iter().any(|e| {
            (e.volume_serial == intent.physical.volume_serial
                && e.file_id == intent.physical.file_id)
                || (!intent.publication.directory
                    && e.relative_path == relative
                    && !e.directory
                    && e.identity
                        .as_deref()
                        .is_none_or(|tag| tag == intent.publication.tag))
        }))
    }
    pub(super) fn synchronize_deletions(&mut self) -> anyhow::Result<DeletionProgress> {
        let binding = self.binding()?.to_string();
        let db = self.bridge.journal().database().clone();
        let mut replayed = HashSet::new();
        for (_, payload) in db.pending_native_events(&binding, 1000)? {
            self.control.checkpoint("local.delete")?;
            let event: NativeEvent = serde_json::from_str(&payload)?;
            ensure!(
                event.schema_version == 1
                    && event.root_file_id == self.receipt().file_id
                    && event.volume_serial == self.receipt().volume_serial,
                "retained delete event root changed"
            );
            if replayed.insert(event.identity.clone())
                && let Some((intent, _)) = self
                    .bridge
                    .journal()
                    .native_delete_for_tag(&binding, &event.identity)?
            {
                self.bridge
                    .journal()
                    .acknowledge_native_delete_history(&intent)?;
            }
        }
        let mut events = db
            .pending_native_events(&binding, 1000)?
            .into_iter()
            .map(|(sequence, payload)| {
                let event: NativeEvent = serde_json::from_str(&payload)?;
                ensure!(
                    event.schema_version == 1
                        && event.volume_serial == self.receipt().volume_serial
                        && event.root_file_id == self.receipt().file_id,
                    "native delete event root changed"
                );
                Ok((sequence, payload, event))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        // Shell recycling can emit request-only callbacks after the matching
        // conditional cloud deletion has already committed. They are not work
        // for a live publication, but retain them in its private archive.
        let mut retired = Vec::new();
        let mut seen = HashSet::new();
        for (_, _, event) in &events {
            self.control.checkpoint("local.delete")?;
            if seen.insert(event.identity.clone())
                && let Some((intent, state)) = self
                    .bridge
                    .journal()
                    .native_delete_for_tag(&binding, &event.identity)?
                && state == "applied"
            {
                retired.push(intent);
            }
        }
        if !retired.is_empty() {
            let entries = self.observed_entries()?;
            let mut archived = HashSet::new();
            for intent in retired {
                self.control.checkpoint("local.delete")?;
                // A different new file at the old path is independent work.
                // Only a surviving old physical/tagged entity vetoes archival.
                if entries.iter().any(|e| {
                    (e.volume_serial == intent.physical.volume_serial
                        && e.file_id == intent.physical.file_id)
                        || e.identity.as_deref() == Some(intent.publication.tag.as_slice())
                }) {
                    continue;
                }
                let decisions = events
                    .iter()
                    .filter(|(seq, _, e)| {
                        *seq > intent.completion_sequence
                            && e.identity == intent.publication.tag
                            && e.file_id == intent.physical.file_id
                            && e.volume_serial == intent.physical.volume_serial
                    })
                    .map(|(seq, payload, _)| (*seq, payload.clone()))
                    .collect::<Vec<_>>();
                self.bridge
                    .journal()
                    .archive_retired_native_events(&intent, &decisions)?;
                archived.extend(decisions.iter().map(|(seq, _)| *seq));
            }
            events.retain(|(seq, _, _)| !archived.contains(seq));
        }
        let existing = self.bridge.journal().native_pending_deletes()?;
        if !existing
            .iter()
            .any(|(_, s)| !matches!(s.as_str(), "applied" | "cancelled"))
            && !events.iter().any(|(_, _, e)| {
                matches!(
                    e.kind,
                    NativeEventKind::DeleteRequested | NativeEventKind::DeleteCompleted
                )
            })
        {
            return Ok(DeletionProgress::default());
        }
        let entries = self.observed_entries()?;
        let projections = self.bridge.journal().native_projections(&binding)?;
        let by_tag = projections
            .iter()
            .map(|p| (p.tag.as_slice(), p))
            .collect::<HashMap<_, _>>();
        let mut no_op = HashSet::new();
        for (_, _, event) in &events {
            self.control.checkpoint("local.delete")?;
            if !matches!(
                event.kind,
                NativeEventKind::DeleteRequested | NativeEventKind::DeleteCompleted
            ) {
                continue;
            }
            let Some(p) = by_tag.get(event.identity.as_slice()).copied() else {
                continue;
            };
            let Some(entry) = entries.iter().find(|entry| {
                entry.file_id == event.file_id
                    && entry.volume_serial == event.volume_serial
                    && entry.identity.as_deref() == Some(p.tag.as_slice())
            }) else {
                continue;
            };
            let decisions = events
                .iter()
                .filter(|(_, _, e)| {
                    e.file_id == event.file_id
                        && e.identity == event.identity
                        && matches!(
                            e.kind,
                            NativeEventKind::DeleteRequested | NativeEventKind::DeleteCompleted
                        )
                })
                .map(|(sequence, payload, _)| (*sequence, payload.clone()))
                .collect::<Vec<_>>();
            // The no-delete-sharing namespace lease cannot coexist with an
            // outstanding delete-capable leaf handle. A failed delete or an
            // already restored in-root object is a positive no-op decision.
            if self
                .owner
                .with_namespace_binding(
                    &entry.relative_path,
                    &p.tag,
                    p.size,
                    p.directory,
                    |volume, id, _| {
                        if volume != entry.volume_serial || id != entry.file_id {
                            return Err("native delete no-op identity changed".into());
                        }
                        db.acknowledge_native_event_decisions(&binding, &decisions)
                            .map_err(|e| e.to_string())
                    },
                )
                .is_ok()
            {
                no_op.extend(decisions.iter().map(|(sequence, _)| *sequence));
            }
        }
        let mut progress = DeletionProgress::default();
        let completed_tags = events
            .iter()
            .filter(|(sequence, _, e)| {
                e.kind == NativeEventKind::DeleteCompleted && !no_op.contains(sequence)
            })
            .map(|(_, _, e)| e.identity.as_slice())
            .collect::<HashSet<_>>();
        let mut projections = projections.iter().collect::<Vec<_>>();
        projections.sort_by_key(|p| std::cmp::Reverse(p.path.matches('/').count()));
        for p in projections
            .into_iter()
            .filter(|p| completed_tags.contains(p.tag.as_slice()))
        {
            self.control.checkpoint("local.delete")?;
            if existing.iter().any(|(i, s)| {
                i.publication.local_id == p.local_id
                    && !matches!(s.as_str(), "applied" | "cancelled")
            }) {
                continue;
            }
            let relative = Self::relative(&p.path)?;
            // A complete same-path replacement belongs to the data consumer.
            if !p.directory
                && entries
                    .iter()
                    .any(|e| e.relative_path == relative && !e.directory)
            {
                continue;
            }
            let captured = self.bridge.journal().native_capture_binding(&p.local_id)?;
            for (sequence, payload, complete) in events.iter().filter(|(sequence, _, e)| {
                e.kind == NativeEventKind::DeleteCompleted
                    && e.identity == p.tag
                    && !no_op.contains(sequence)
            }) {
                self.control.checkpoint("local.delete")?;
                let physical = NativeFileIdentity {
                    volume_serial: complete.volume_serial,
                    file_id: complete.file_id,
                };
                if entries.iter().any(|e| {
                    e.volume_serial == physical.volume_serial && e.file_id == physical.file_id
                }) {
                    continue;
                }
                if !(p.volume_serial == Some(physical.volume_serial)
                    && p.native_file_id.as_deref()
                        == Some(format!("{:016x}", physical.file_id).as_str()))
                    && !captured.is_some_and(|c| c.physical == physical)
                {
                    continue;
                }
                let Some((request_sequence, request_payload, _)) =
                    events.iter().rev().find(|(n, _, e)| {
                        n < sequence
                            && e.kind == NativeEventKind::DeleteRequested
                            && e.identity == p.tag
                            && e.file_id == physical.file_id
                            && e.connection_key == complete.connection_key
                            && (e.request_key == complete.request_key || complete.request_key == 0)
                    })
                else {
                    continue;
                };
                if self
                    .bridge
                    .journal()
                    .snapshot_if_owned(&p.local_id)?
                    .as_ref()
                    .is_some_and(|s| s.uncertain_upload)
                {
                    self.bridge.reconcile(&p.local_id)?;
                }
                let record = db
                    .get_by_remote_id(&p.local_id)?
                    .context("native deleted record missing")?;
                if p.directory && !db.list_descendants(&p.path)?.is_empty() {
                    continue;
                }
                let directory_parent_id = if p.directory {
                    Some(if record.metadata.parent_path == "/" {
                        db.get_state_value("native_cloud_root_id")?
                            .context("directory parent root missing")?
                    } else {
                        db.get_by_path(&record.metadata.parent_path)?
                            .and_then(|r| r.cloud_remote_id)
                            .context("directory parent cloud identity missing")?
                    })
                } else {
                    None
                };
                let version = if p.directory {
                    let mut entry = record.metadata.clone();
                    entry.remote_id = p.cloud_id.clone();
                    let Some(stamp) = self.bridge.backend().empty_directory_version_bound(
                        &entry,
                        directory_parent_id.as_deref().unwrap(),
                    )?
                    else {
                        progress.waiting += 1;
                        continue;
                    };
                    stamp
                } else {
                    self.bridge
                        .backend()
                        .get_metadata(&p.cloud_id)?
                        .unwrap_or_else(|| {
                            let mut version = record.metadata.clone();
                            version.remote_id = p.cloud_id.clone();
                            version
                        })
                };
                let intent = NativeDeleteIntent {
                    directory_parent_id,
                    publication: p.clone(),
                    version,
                    root_receipt: serde_json::to_string(self.receipt())?,
                    physical,
                    request_sequence: *request_sequence,
                    request_payload: request_payload.clone(),
                    completion_sequence: *sequence,
                    completion_payload: payload.clone(),
                    not_before_unix: twodrive_core::now_unix() + 2,
                };
                self.bridge.journal().prepare_native_delete(&intent)?;
                #[cfg(all(test, feature = "native-test-hooks"))]
                if INTERRUPT_AFTER_DELETE_PREPARE.with(|flag| flag.replace(false)) {
                    anyhow::bail!("generated interruption after delete prepare");
                }

                db.acknowledge_native_event_decisions(
                    &binding,
                    &[
                        (*request_sequence, request_payload.clone()),
                        (*sequence, payload.clone()),
                    ],
                )?;
                break;
            }
        }
        for (intent, state) in self.bridge.journal().native_pending_deletes()? {
            self.control.checkpoint("local.delete")?;
            let id = &intent.publication.local_id;
            if matches!(state.as_str(), "applied" | "cancelled") {
                continue;
            }
            if state == "prepared" && Self::supersedes_delete(&entries, &intent)? {
                self.bridge.journal().cancel_prepared_native_delete(id)?;
                continue;
            }
            let result = (|| -> anyhow::Result<bool> {
                if state == "dispatched" {
                    match self
                        .bridge
                        .backend()
                        .get_metadata(&intent.version.remote_id)?
                    {
                        None => self.bridge.journal().receive_native_delete(id)?,
                        Some(current) => {
                            let current = if intent.publication.directory {
                                ensure!(
                                    current.path == intent.version.path,
                                    "deleted directory moved remotely"
                                );
                                self.bridge
                                    .backend()
                                    .empty_directory_version_bound(
                                        &current,
                                        intent
                                            .directory_parent_id
                                            .as_deref()
                                            .context("directory parent receipt missing")?,
                                    )?
                                    .context("directory gained remote children")?
                            } else {
                                current
                            };
                            if intent.publication.directory {
                                self.bridge
                                    .journal()
                                    .retry_native_empty_directory(id, &current)?;
                            } else {
                                self.bridge
                                    .journal()
                                    .retry_native_delete_unchanged(id, &current)?;
                            }
                            if Self::supersedes_delete(&entries, &intent)? {
                                self.bridge.journal().cancel_prepared_native_delete(id)?;
                                return Ok(false);
                            }
                        }
                    }
                }
                let (active, current) = self
                    .bridge
                    .journal()
                    .native_delete(id)?
                    .context("native delete recovery missing")?;
                if current == "prepared" {
                    if twodrive_core::now_unix() < intent.not_before_unix {
                        return Ok(true);
                    }
                    if self.bridge.journal().dispatch_native_delete(id)? {
                        match self.bridge.backend().delete_bound(&active.version) {
                            Ok(()) => self.bridge.journal().receive_native_delete(id)?,
                            Err(error) => {
                                if error
                                    .downcast_ref::<crate::provider::TransferNotStarted>()
                                    .is_some()
                                {
                                    self.bridge.journal().native_delete_not_dispatched(id)?;
                                }
                                return Err(error);
                            }
                        }
                    }
                }
                self.bridge.journal().apply_native_delete(id)?;
                progress.deleted += 1;
                Ok(false)
            })();
            match result {
                Ok(true) => progress.waiting += 1,
                Ok(false) => {}
                Err(error) => {
                    progress.waiting += 1;
                    progress.error =
                        Some(twodrive_backend::diagnostics::safe_error(&error).to_string());
                }
            }
        }
        Ok(progress)
    }
}
