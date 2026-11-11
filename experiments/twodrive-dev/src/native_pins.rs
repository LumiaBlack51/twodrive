//! Shell pin/free-space intents never bypass content and identity leases.
use super::*;

#[derive(Default, Debug, Serialize)]
pub struct ResidencyProgress {
    pub private_cache_files_reclaimed: usize,
    pub hydrated: usize,
    pub released: usize,
    pub deferred: usize,
    pub error: Option<String>,
}
impl NativeCoordinator {
    pub fn synchronize_residency(&mut self) -> anyhow::Result<ResidencyProgress> {
        let reclaimed = self
            .bridge
            .journal()
            .database()
            .prune_retired_cache(self.bridge.journal().cache_root())?;
        self.drain_observations()?;
        let watch = self
            .watcher
            .as_ref()
            .context("native watcher stopped")?
            .state()
            .map_err(anyhow::Error::msg)?;
        if !self.residency_pending
            && !watch.rescan_required
            && self.last_residency_watch == Some((watch.epoch, watch.generation))
        {
            return Ok(ResidencyProgress {
                private_cache_files_reclaimed: reclaimed,
                ..Default::default()
            });
        }
        let binding = self.binding()?.to_string();
        let records = self
            .bridge
            .journal()
            .database()
            .all_records()?
            .into_iter()
            .map(|r| (r.metadata.remote_id.clone(), r))
            .collect::<HashMap<_, _>>();
        let projections = self
            .bridge
            .journal()
            .native_projections(&binding)?
            .into_iter()
            .filter(|p| p.state == "published")
            .map(|p| (p.tag.clone(), p))
            .collect::<HashMap<_, _>>();
        let entries = self.observed_entries()?;
        let mut changes = Vec::new();
        for entry in &entries {
            self.control.checkpoint("residency.scan")?;
            let Some(p) = entry.identity.as_ref().and_then(|tag| projections.get(tag)) else {
                continue;
            };
            let Some(os) = entry.cloud.as_ref() else {
                continue;
            };
            let physical = p.volume_serial == Some(entry.volume_serial)
                && p.native_file_id.as_deref() == Some(format!("{:016x}", entry.file_id).as_str())
                && Self::cloud_path(&entry.relative_path)? == p.path;
            if !physical || (!os.pinned && !os.pin_known_unpinned) {
                continue;
            }
            let record = records
                .get(&p.local_id)
                .context("native pin record missing")?;
            if record.pin_explicit != os.pinned
                || record.pin_inheritance_blocked != os.release_requested
            {
                changes.push((p.local_id.clone(), os.pinned, os.release_requested));
            }
        }
        self.bridge.journal().mirror_native_pins(&changes)?;
        let mut progress = ResidencyProgress {
            private_cache_files_reclaimed: reclaimed,
            ..Default::default()
        };
        for entry in entries.into_iter().filter(|e| !e.directory) {
            self.control.checkpoint("residency.apply")?;
            let Some(p) = entry.identity.as_ref().and_then(|tag| projections.get(tag)) else {
                continue;
            };
            let Some(os) = entry.cloud.as_ref() else {
                continue;
            };
            if !(os.pinned && !os.resident || os.release_requested) {
                continue;
            }
            let result = (|| -> anyhow::Result<()> {
                let physical = twodrive_core::NativeFileIdentity {
                    volume_serial: p.volume_serial.context("pin volume missing")?,
                    file_id: u64::from_str_radix(
                        p.native_file_id
                            .as_deref()
                            .context("pin physical identity missing")?,
                        16,
                    )?,
                };
                ensure!(
                    physical.volume_serial == entry.volume_serial
                        && physical.file_id == entry.file_id
                        && Self::cloud_path(&entry.relative_path)? == p.path,
                    "native pin observation changed"
                );
                if os.pinned && !os.resident {
                    ensure!(
                        self.owner
                            .has_hydration_space(self.bridge.journal().cache_root(), p.size)
                            .map_err(anyhow::Error::msg)?,
                        "pin waits for free disk space"
                    );
                    self.owner
                        .with_owned_exclusive(
                            &entry.relative_path,
                            &p.tag,
                            physical,
                            p.size,
                            |locked| {
                                ensure_native_pin(locked.inspect()?.pinned)?;
                                locked.hydrate()
                            },
                        )
                        .map_err(anyhow::Error::msg)?;
                    progress.hydrated += 1;
                } else if os.release_requested {
                    let Some(state) = self.bridge.journal().snapshot_if_owned(&p.local_id)? else {
                        return Ok(());
                    };
                    let released = self
                        .owner
                        .with_owned_exclusive(
                            &entry.relative_path,
                            &p.tag,
                            physical,
                            p.size,
                            |locked| {
                                let native = locked.inspect()?;
                                if !native.release_requested
                                    || native.pinned
                                    || !native.os_in_sync
                                    || native.modified_bytes != 0
                                {
                                    return Err(
                                        "native free-space intent changed or has local edits"
                                            .into(),
                                    );
                                }
                                self.bridge
                                    .journal()
                                    .release_native_cache(
                                        &p.local_id,
                                        state.generation,
                                        crate::local_files::digest_file,
                                        || {
                                            if native.resident {
                                                locked.dehydrate().map_err(anyhow::Error::msg)?;
                                            }
                                            Ok(())
                                        },
                                    )
                                    .map_err(|e| e.to_string())
                            },
                        )
                        .map_err(anyhow::Error::msg)?;
                    ensure!(released, "free-space waits for clean private generation");
                    progress.released += 1;
                }
                Ok(())
            })();
            if let Err(error) = result {
                progress.deferred += 1;
                progress.error =
                    Some(twodrive_backend::diagnostics::safe_error(&error).to_string());
            }
        }
        self.residency_pending = progress.deferred != 0;
        self.last_residency_watch = Some((watch.epoch, watch.generation));
        Ok(progress)
    }
}
fn ensure_native_pin(pinned: bool) -> Result<(), String> {
    if pinned {
        Ok(())
    } else {
        Err("native pin intent was cancelled".into())
    }
}
