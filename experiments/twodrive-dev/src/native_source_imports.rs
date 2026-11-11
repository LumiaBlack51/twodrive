use super::*;
use crate::local_files::digest_file;
use twodrive_core::{NativeRemoteUpdate, NativeSourceImport};
use twodrive_local_files::native::persistent::{NativeEvent, NativeEventKind};
#[cfg(all(test, feature = "native-test-hooks"))]
thread_local! {pub(super) static FAIL_NEXT_NAMESPACE_LOOKUP_87:std::cell::Cell<bool>=const {std::cell::Cell::new(false)};}

#[derive(Default, Debug, Serialize)]
pub struct SourceImportProgress {
    pub imported: usize,
    pub deferred: usize,
    pub isolated: usize,
    pub error: Option<String>,
}
impl NativeCoordinator {
    pub fn resume_source_imports(&mut self) -> anyhow::Result<SourceImportProgress> {
        let binding = self.binding()?.to_string();
        let mut progress = SourceImportProgress::default();
        for update in self
            .bridge
            .journal()
            .native_remote_updates(&binding)?
            .into_iter()
            .filter(|u| u.local_modified_filetime.is_some())
        {
            self.control.checkpoint("local.import.replay")?;
            let events = self
                .bridge
                .journal()
                .database()
                .pending_native_events(&binding, 1000)?;
            let receipt = serde_json::to_string(self.receipt())?;
            let mut cancelled = false;
            let mut namespace_failure = None;
            for (sequence, payload) in events {
                self.control.checkpoint("local.import.events")?;
                let event: twodrive_local_files::native::persistent::NativeEvent =
                    serde_json::from_str(&payload)?;
                if event.schema_version != 1
                    || event.root_file_id != self.receipt().file_id
                    || event.volume_serial != self.receipt().volume_serial
                    || event.identity != update.expected.tag
                    || update.expected.native_file_id.as_deref()
                        != Some(format!("{:016x}", event.file_id).as_str())
                {
                    continue;
                }
                match self.recover_source_namespace(&update, &receipt, sequence, &payload, &event) {
                    Ok(true) => {
                        cancelled = true;
                        break;
                    }
                    Ok(false) => continue,
                    Err(error) => {
                        // A later exact completion can resolve this object's
                        // earlier rename even when its file reference expired.
                        namespace_failure = Some(error);
                    }
                }
            }
            if cancelled {
                continue;
            }
            if let Some(error) = namespace_failure {
                progress.deferred += 1;
                progress.error =
                    Some(twodrive_backend::diagnostics::safe_error(&error).to_string());
                continue;
            }
            if let Err(error) = self.bridge.apply_native_remote_version(
                &self.owner,
                &update.expected,
                &update.next,
                Some(update.clone()),
            ) {
                progress.deferred += 1;
                progress.error =
                    Some(twodrive_backend::diagnostics::safe_error(&error).to_string());
            }
        }
        for import in self.bridge.journal().native_source_imports(&binding)? {
            self.control.checkpoint("local.import.apply")?;
            match self.apply_source_import(&import) {
                Ok(true) => progress.imported += 1,
                Ok(false) => progress.isolated += 1,
                Err(error) => {
                    #[cfg(test)]
                    eprintln!("generated source import: {error:#}");
                    progress.deferred += 1;
                    progress.error =
                        Some(twodrive_backend::diagnostics::safe_error(&error).to_string());
                }
            }
        }
        Ok(progress)
    }
    fn locate_import_file(
        &self,
        physical: twodrive_core::NativeFileIdentity,
    ) -> Result<Option<PathBuf>, String> {
        #[cfg(all(test, feature = "native-test-hooks"))]
        if FAIL_NEXT_NAMESPACE_LOOKUP_87.with(|f| f.replace(false)) {
            return Err("namespace identity lookup: 87".into());
        }
        self.owner
            .locate_file_id(physical.volume_serial, physical.file_id)
    }
    fn recover_source_namespace(
        &self,
        update: &NativeRemoteUpdate,
        receipt: &str,
        sequence: i64,
        payload: &str,
        event: &NativeEvent,
    ) -> anyhow::Result<bool> {
        if !matches!(
            event.kind,
            NativeEventKind::RenameCompleted | NativeEventKind::DeleteCompleted
        ) {
            return Ok(false);
        }
        let physical = twodrive_core::NativeFileIdentity {
            volume_serial: event.volume_serial,
            file_id: event.file_id,
        };
        let located = self.locate_import_file(physical);
        let path = match located {
            Ok(Some(path)) => Some(path),
            _ if event.kind == NativeEventKind::DeleteCompleted => {
                // A rejected/expired file reference is not absence authority.
                // Re-inventory the owned namespace for a surviving exact object;
                // cancellation itself still requires the retained completion.
                self.owner
                    .inventory()
                    .map_err(anyhow::Error::msg)?
                    .into_iter()
                    .find(|entry| {
                        entry.volume_serial == physical.volume_serial
                            && entry.file_id == physical.file_id
                            && !entry.directory
                            && entry.identity.as_deref() == Some(update.expected.tag.as_slice())
                    })
                    .map(|entry| self.owner.root().join(entry.relative_path))
            }
            Ok(None) => return Ok(false),
            Err(error) => return Err(anyhow::Error::msg(error)),
        };
        if let Some(path) = path {
            let relative = path
                .strip_prefix(self.owner.root())
                .context("renamed pin left owned root")?;
            self.owner
                .with_owned_pin_update(
                    relative,
                    &update.expected.tag,
                    physical,
                    update.expected.size,
                    |os, set_pin| {
                        let pin = if os.release_requested {
                            2
                        } else if os.pinned {
                            1
                        } else {
                            update.local_pin.unwrap_or(0)
                        };
                        let current = if matches!(pin, 1..=2) && update.local_pin != Some(pin) {
                            self.bridge
                                .journal()
                                .refresh_native_migration_pin(update, pin)
                                .map_err(|e| e.to_string())?
                        } else {
                            update.clone()
                        };
                        set_pin(pin)?;
                        self.bridge
                            .journal()
                            .cancel_native_migration_namespace(&current, receipt, sequence, payload)
                            .map_err(|e| e.to_string())
                    },
                )
                .map_err(anyhow::Error::msg)?;
        } else {
            self.bridge
                .journal()
                .cancel_native_migration_namespace(update, receipt, sequence, payload)?;
        }
        Ok(true)
    }
    fn apply_source_import(&mut self, import: &NativeSourceImport) -> anyhow::Result<bool> {
        let current = self
            .bridge
            .journal()
            .native_projection_for_id(&import.expected.local_id)?;
        let Some(current) = current else {
            if self
                .bridge
                .journal()
                .native_delete(&import.expected.local_id)?
                .is_some_and(|(intent, state)| {
                    state == "applied" && intent.publication == import.expected
                })
                || self
                    .bridge
                    .journal()
                    .native_remote_removal_applied(&import.expected)?
            {
                self.bridge
                    .journal()
                    .isolate_native_source_import(import.sequence)?;
                return Ok(false);
            }
            anyhow::bail!("source target publication missing without verified retirement");
        };
        let same = current.state == "published"
            && current.cloud_id == import.expected.cloud_id
            && current.etag == import.expected.etag
            && current.size == import.expected.size
            && current.volume_serial == import.expected.volume_serial
            && current.native_file_id == import.expected.native_file_id;
        let dirty = self
            .bridge
            .journal()
            .snapshot_if_owned(&current.local_id)?
            .is_some_and(|s| {
                s.dirty_intent
                    || s.native_dirty
                    || s.uncertain_upload
                    || s.upload_generation.is_some()
                    || s.recovery_required
            });
        if !same || dirty {
            if current.state == "updating" {
                anyhow::bail!("source metadata update still requires recovery");
            }
            self.bridge
                .journal()
                .isolate_native_source_import(import.sequence)?;
            return Ok(false);
        }
        if self
            .owner
            .with_namespace_binding(
                &Self::relative(&current.path)?,
                &current.tag,
                current.size,
                false,
                |volume, id, os| {
                    Ok(volume != current.volume_serial.unwrap_or(0)
                        || Some(format!("{id:016x}")) != current.native_file_id
                        || !os.os_in_sync
                        || os.modified_bytes != 0)
                },
            )
            .unwrap_or(false)
        {
            self.bridge
                .journal()
                .isolate_native_source_import(import.sequence)?;
            return Ok(false);
        }
        let relative = Self::relative(&current.path)?;
        let physical = twodrive_core::NativeFileIdentity {
            volume_serial: current
                .volume_serial
                .context("source target volume missing")?,
            file_id: u64::from_str_radix(
                current
                    .native_file_id
                    .as_deref()
                    .context("source target physical missing")?,
                16,
            )?,
        };
        let target_intent = self
            .owner
            .with_clean_cache_import(&relative, &current.tag, physical, current.size, |native| {
                let fresh = self
                    .bridge
                    .journal()
                    .database()
                    .get_by_remote_id(&current.local_id)
                    .map_err(|e| e.to_string())?
                    .ok_or("cache import target record missing")?;
                if fresh.cache_path.is_none() {
                    let generation = self
                        .bridge
                        .journal()
                        .begin_bound_download(&fresh)
                        .map_err(|e| e.to_string())?;
                    self.bridge
                        .journal()
                        .publish_download(
                            &current.local_id,
                            generation,
                            &import.cache_path,
                            &import.digest,
                            &current.etag,
                        )
                        .map_err(|e| e.to_string())?;
                }
                self.bridge
                    .journal()
                    .mark_native_source_data_ready(import)
                    .map_err(|e| e.to_string())?;
                Ok(native.clone())
            })
            .map_err(anyhow::Error::msg)?;
        if !target_intent.release_requested {
            self.owner
                .with_owned_exclusive(&relative, &current.tag, physical, current.size, |locked| {
                    let native = locked.inspect()?;
                    if !native.os_in_sync || native.modified_bytes != 0 {
                        return Err("source import preserves newer native bytes".into());
                    }
                    if import.source_pinned && !native.release_requested {
                        locked.retain_pin_intent()?;
                    }
                    if !native.release_requested && current.size != 0 {
                        locked.hydrate()?;
                    }
                    Ok(())
                })
                .map_err(anyhow::Error::msg)?;
        }
        // Source bytes are confirmed before the independently durable local
        // timestamp update. Interruption does not repeat a cloud mutation.
        self.bridge
            .apply_native_source_time(&self.owner, &current, import.source_filetime)?;
        let pin = self
            .owner
            .with_namespace_binding(&relative, &current.tag, current.size, false, |_, _, os| {
                Ok((os.pinned, os.release_requested))
            })
            .map_err(anyhow::Error::msg)?;
        let current_record = self
            .bridge
            .journal()
            .database()
            .get_by_remote_id(&current.local_id)?
            .context("source target disappeared")?;
        if current_record.pin_explicit != pin.0 || current_record.pin_inheritance_blocked != pin.1 {
            self.bridge.journal().mirror_native_pins(&[(
                current.local_id.clone(),
                pin.0,
                pin.1,
            )])?;
        }
        self.bridge
            .journal()
            .finish_native_source_import(import, digest_file)?;
        self.last_successful_watch = None;
        self.last_residency_watch = None;
        Ok(true)
    }
}
