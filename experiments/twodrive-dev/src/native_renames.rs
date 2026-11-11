#[cfg(all(test, feature = "native-test-hooks"))]
thread_local! {pub(crate) static INTERRUPT_BACKUP_AFTER_COMPLETE:std::cell::Cell<bool>=const {std::cell::Cell::new(false)};}
use super::*;
use crate::model::CloudBackend;
use twodrive_core::NativeMoveIntent;
use twodrive_local_files::native::persistent::{NativeEvent, NativeEventKind};

impl NativeCoordinator {
    pub(super) fn synchronize_renames(&mut self) -> anyhow::Result<usize> {
        let binding = self.binding()?.to_string();
        let db = self.bridge.journal().database().clone();
        self.bridge
            .journal()
            .recover_legacy_backup_history(&binding, self.receipt().file_id)?;
        let mut moved = 0;
        for _ in 0..1000 {
            let projections = self.bridge.journal().native_projections(&binding)?;
            let by_tag = projections
                .iter()
                .map(|p| (p.tag.as_slice(), p))
                .collect::<HashMap<_, _>>();
            let mut recovered = false;
            for (intent, state) in self.bridge.journal().native_pending_moves(&binding)? {
                if state == "received" {
                    self.bridge
                        .journal()
                        .apply_native_move(&intent.publication.local_id)?;
                    moved += 1;
                    recovered = true;
                    break;
                }
                ensure!(
                    state != "dispatched",
                    "native rename outcome uncertain; reconciliation required"
                );
            }
            if recovered {
                continue;
            }
            let pending_backups = self.bridge.journal().native_backup_copies(&binding)?;
            if !pending_backups.is_empty() {
                let entries = self.owner.inventory().map_err(anyhow::Error::msg)?;
                for copy in pending_backups {
                    let backup = entries.iter().find(|e| {
                        e.volume_serial == copy.physical.volume_serial
                            && e.file_id == copy.physical.file_id
                    });
                    let Some(backup) = backup else {
                        // This retires a local copy decision, never a cloud
                        // object. Both complete snapshots already have owners.
                        self.bridge.journal().complete_native_backup_copy(
                            copy.sequence,
                            crate::local_files::digest_file,
                        )?;
                        continue;
                    };
                    let target = entries
                        .iter()
                        .find(|e| {
                            e.volume_serial == copy.target.volume_serial
                                && e.file_id == copy.target.file_id
                        })
                        .map(|e| e.relative_path.clone())
                        .unwrap_or(Self::relative(&copy.source.path)?);
                    self.bridge.split_native_backup(
                        &self.owner,
                        &backup.relative_path,
                        &target,
                        &copy.source,
                        copy.physical,
                    )?;
                }
                continue;
            }
            let mut entries = self.observed_entries()?;
            entries.sort_by_key(|e| e.relative_path.components().count());
            let mut candidate = None;
            let mut split = false;
            for entry in &entries {
                let Some(p) = entry
                    .identity
                    .as_deref()
                    .and_then(|tag| by_tag.get(tag))
                    .copied()
                else {
                    continue;
                };
                if entry.directory != p.directory || entry.relative_path == Self::relative(&p.path)?
                {
                    continue;
                }
                let captured = self.bridge.journal().native_capture_binding(&p.local_id)?;
                let physical_matches = p.volume_serial == Some(entry.volume_serial)
                    && (p.native_file_id.as_deref()
                        == Some(format!("{:016x}", entry.file_id).as_str())
                        || captured.is_some_and(|c| {
                            c.physical.volume_serial == entry.volume_serial
                                && c.physical.file_id == entry.file_id
                        }));
                if !physical_matches
                    || entry.directory != p.directory
                    || entry.relative_path == Self::relative(&p.path)?
                {
                    continue;
                }
                if !p.directory
                    && let Some(target) = entries.iter().find(|target| {
                        target.relative_path == Self::relative(&p.path).unwrap_or_default()
                            && !target.directory
                            && (target.volume_serial != entry.volume_serial
                                || target.file_id != entry.file_id)
                            && target.identity.as_deref().is_none_or(|tag| tag == p.tag)
                    })
                {
                    self.bridge.split_native_backup(
                        &self.owner,
                        &entry.relative_path,
                        &target.relative_path,
                        p,
                        twodrive_core::NativeFileIdentity {
                            volume_serial: entry.volume_serial,
                            file_id: entry.file_id,
                        },
                    )?;
                    #[cfg(all(test, feature = "native-test-hooks"))]
                    if INTERRUPT_BACKUP_AFTER_COMPLETE.with(|f| f.replace(false)) {
                        anyhow::bail!("generated interruption after backup completion transaction");
                    }
                    split = true;
                    break;
                }
                ensure!(
                    entries
                        .iter()
                        .filter(|other| other.volume_serial == entry.volume_serial
                            && other.file_id == entry.file_id)
                        .count()
                        == 1,
                    "ambiguous native rename link identity"
                );
                candidate = Some((p.clone(), entry.clone()));
                break;
            }
            if split {
                continue;
            }
            let Some((publication, entry)) = candidate else {
                break;
            };
            let destination = Self::cloud_path(&entry.relative_path)?;
            let parent_path = entry
                .relative_path
                .parent()
                .context("native rename parent missing")?;
            let mut prefix = PathBuf::new();
            for component in parent_path.components() {
                prefix.push(component);
                let cloud = Self::cloud_path(&prefix)?;
                if db.get_by_path(&cloud)?.is_none() {
                    self.synchronize_new_directory(&prefix)?;
                }
            }
            let parent = if parent_path.as_os_str().is_empty() {
                MetadataEntry::new_dir(
                    db.get_state_value("native_cloud_root_id")?
                        .context("native cloud root missing")?,
                    "/",
                    0,
                    "",
                )
            } else {
                let record = db
                    .get_by_path(&Self::cloud_path(parent_path)?)?
                    .context("native rename parent not synchronized")?;
                ensure!(
                    record.metadata.is_dir
                        && !db.has_pending_metadata_at_or_above(&record.metadata.path)?,
                    "native rename parent has pending work"
                );
                let mut parent = record.metadata;
                parent.remote_id = record
                    .cloud_remote_id
                    .context("native rename parent has no cloud ID")?;
                parent
            };
            let existing = self.bridge.journal().native_move(&publication.local_id)?;
            let intent =
                if let Some((intent, state)) = existing.filter(|(_, state)| state != "applied") {
                    ensure!(
                        state == "prepared"
                            && intent.destination == destination
                            && intent.parent_id == parent.remote_id,
                        "prepared native rename destination changed"
                    );
                    intent
                } else {
                    let version = self
                        .bridge
                        .backend()
                        .get_metadata(&publication.cloud_id)?
                        .context("native rename cloud object missing")?;
                    NativeMoveIntent {
                        publication: publication.clone(),
                        version,
                        destination,
                        parent_id: parent.remote_id.clone(),
                    }
                };
            self.owner
                .with_namespace_binding(
                    &entry.relative_path,
                    &publication.tag,
                    publication.size,
                    publication.directory,
                    |volume, id, _| {
                        if volume != entry.volume_serial || id != entry.file_id {
                            return Err("native rename physical identity changed".into());
                        }
                        self.bridge
                            .journal()
                            .prepare_native_move(&intent)
                            .map_err(|e| e.to_string())
                    },
                )
                .map_err(anyhow::Error::msg)?;
            if self
                .bridge
                .journal()
                .dispatch_native_move(&publication.local_id)?
            {
                match self.bridge.backend().move_bound(
                    &intent.version,
                    &parent,
                    &intent.destination,
                ) {
                    Ok(receipt) => self
                        .bridge
                        .journal()
                        .receive_native_move(&publication.local_id, &receipt)?,
                    Err(error) => {
                        if error
                            .downcast_ref::<crate::provider::TransferNotStarted>()
                            .is_some()
                        {
                            self.bridge
                                .journal()
                                .native_move_not_dispatched(&publication.local_id)?;
                        }
                        return Err(error);
                    }
                }
            }
            self.bridge
                .journal()
                .apply_native_move(&publication.local_id)?;
            moved += 1;
        }
        // A positive in-root physical observation can retire only that object's
        // completed/no-op rename hints. Interleaved deletes remain durable.
        let projections = self.bridge.journal().native_projections(&binding)?;
        let by_tag = projections
            .iter()
            .map(|p| (p.tag.as_slice(), p))
            .collect::<HashMap<_, _>>();
        let entries = self.observed_entries()?;
        let mut decisions = Vec::new();
        for (sequence, payload) in db.pending_native_events(&binding, 1000)? {
            let event: NativeEvent = serde_json::from_str(&payload)?;
            ensure!(
                event.schema_version == 1
                    && event.volume_serial == self.receipt().volume_serial
                    && event.root_file_id == self.receipt().file_id,
                "native rename event root changed"
            );
            if !matches!(
                event.kind,
                NativeEventKind::RenameRequested | NativeEventKind::RenameCompleted
            ) {
                continue;
            }
            let Some(p) = by_tag.get(event.identity.as_slice()).copied() else {
                continue;
            };
            if entries.iter().any(|e| {
                e.file_id == event.file_id
                    && e.volume_serial == event.volume_serial
                    && e.identity.as_deref() == Some(p.tag.as_slice())
                    && Self::cloud_path(&e.relative_path).is_ok_and(|path| path == p.path)
            }) {
                decisions.push((sequence, payload));
            }
        }
        db.acknowledge_native_event_decisions(&binding, &decisions)?;
        Ok(moved)
    }
}
