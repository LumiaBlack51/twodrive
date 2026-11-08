use super::*;
use crate::local_files::digest_file;
use twodrive_core::NativeSourceImport;

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
    fn apply_source_import(&mut self, import: &NativeSourceImport) -> anyhow::Result<bool> {
        let current = self
            .bridge
            .journal()
            .native_projection_for_id(&import.expected.local_id)?
            .context("source target publication missing")?;
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
