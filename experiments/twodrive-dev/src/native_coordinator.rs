//! Persistent initial native projection. Cloud mutation consumers are separate;
//! publication never infers cloud deletion from a missing local path.
use crate::{local_files::LocalFileBridge, native_source::BoundPublishedSource};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};
use twodrive_core::{CloudIdentity, Database};
use twodrive_core::{MetadataEntry, NativeProjection};
use twodrive_local_files::{
    catalogue::NamespacePlan,
    native::persistent::{
        DatabaseEventSink, DurableDirectoryWatcher, PersistentSession, RootReceipt,
    },
};
#[path = "native_deletions.rs"]
mod deletions;
#[path = "native_remote.rs"]
mod remote;
pub use remote::RemoteProgress;
#[path = "native_pins.rs"]
mod pins;
pub use pins::ResidencyProgress;
#[path = "native_renames.rs"]
mod renames;

#[derive(Clone, Serialize, Deserialize)]
struct Configuration {
    schema_version: u32,
    account: PathBuf,
    identity: CloudIdentity,
    root: PathBuf,
    registration: Vec<u8>,
    receipt: Option<RootReceipt>,
}
pub struct CreateNativeSession {
    pub account: PathBuf,
    pub expected_owner: String,
    pub state: PathBuf,
    pub root: PathBuf,
    pub catalogue: PathBuf,
}
pub struct NativeSession {
    pub coordinator: NativeCoordinator,
    state: PathBuf,
    configuration: Configuration,
}
impl NativeSession {
    /// Stage a fresh native root while the existing client/root remain intact.
    /// This initial projection does not enable the cloud mutation consumers.
    pub fn create(request: CreateNativeSession) -> anyhow::Result<Self> {
        use crate::{
            model::CloudBackend,
            onedrive::{self, OneDrive},
        };
        use sha2::{Digest, Sha256};
        use std::time::{Duration, Instant};
        ensure!(
            request.account.is_absolute()
                && request.state.is_absolute()
                && !request.state.exists()
                && request.root.is_absolute()
                && !request.root.exists()
                && request.catalogue.is_absolute()
                && request.catalogue.is_file(),
            "new absolute native state/root and existing private catalogue required"
        );
        let account = onedrive::paths(&request.account)?.config_dir;
        let provider = OneDrive::open(&account, false)?;
        let identity = provider.graph.readonly_identity(&mut || Ok(()))?;
        ensure!(
            identity
                .account
                .eq_ignore_ascii_case(&request.expected_owner),
            "native personal owner mismatch"
        );
        let cloud_root = provider
            .get_metadata_by_path("/")?
            .context("verified drive root missing")?;
        ensure!(cloud_root.is_dir, "verified root is not a directory");
        let catalogue = Database::new(&request.catalogue);
        let seed = catalogue.cloud_items(&identity)?;
        let cursor = catalogue
            .cloud_delta(&identity)?
            .context("private catalogue has no committed cursor")?;
        ensure!(
            !seed.is_empty(),
            "private catalogue belongs to another account or drive"
        );
        let private = twodrive_local_files::native::GeneratedDirectory::create(&request.state)
            .map_err(anyhow::Error::msg)?;
        let state = private.path().to_path_buf();
        let db = Database::new(state.join("native.sqlite3"));
        db.init()?;
        db.init_cloud()?;
        db.cloud_stage(&identity, &seed)?;
        db.cloud_commit(&identity, &cursor, true)?;
        let deadline = Instant::now() + Duration::from_secs(300);
        let (cursor, replace, _) = provider.graph.stage_delta(&db, &identity, &mut || {
            ensure!(
                Instant::now() < deadline,
                "native metadata refresh deadline reached"
            );
            Ok(())
        })?;
        ensure!(
            provider.graph.readonly_identity(&mut || Ok(()))? == identity,
            "account changed during native preparation"
        );
        db.cloud_commit(&identity, &cursor, replace)?;
        use std::os::windows::ffi::OsStrExt;
        let plan = NamespacePlan::build(
            &db.cloud_items(&identity)?,
            &cloud_root.remote_id,
            request.root.as_os_str().encode_wide().count(),
            twodrive_local_files::names::ordinal_ignorecase_cmp,
        )
        .map_err(anyhow::Error::msg)?;
        let cache = state.join("cache");
        std::fs::create_dir(&cache)?;
        let bridge = onedrive::open_pinned_session(db, &cache, &account, identity.clone(), true)?;
        let nonce = rand::random::<[u8; 32]>();
        let registration = format!(
            "twodrive-dev-native-v1:{:x}",
            Sha256::digest(
                [
                    identity.account.as_bytes(),
                    identity.drive.as_bytes(),
                    &nonce
                ]
                .concat()
            )
        )
        .into_bytes();
        let mut configuration = Configuration {
            schema_version: 1,
            account,
            identity,
            root: request.root,
            registration,
            receipt: None,
        };
        Self::seal(&state, &configuration)?;
        let coordinator = NativeCoordinator::create_new(
            bridge,
            &plan,
            &configuration.root,
            &configuration.registration,
            &state.join("root-receipt.json"),
        )?;
        configuration.root = coordinator.root().to_path_buf();
        configuration.receipt = Some(coordinator.receipt().clone());
        Self::seal(&state, &configuration)?;
        Ok(Self {
            coordinator,
            state,
            configuration,
        })
    }
    fn seal(state: &Path, configuration: &Configuration) -> anyhow::Result<()> {
        twodrive_core::private_file::write_secret(
            &state.join("native-owner.dat"),
            &serde_json::to_vec(configuration)?,
        )
    }
    /// A missing network/credential does not erase known cache authority. Every
    /// subsequent provider operation still verifies the sealed account identity.
    pub fn resume(state: &Path) -> anyhow::Result<Self> {
        ensure!(state.is_absolute(), "absolute native state required");
        let state = state.canonicalize()?;
        let mut configuration: Configuration = serde_json::from_slice(
            &twodrive_core::private_file::read_secret(&state.join("native-owner.dat"))?,
        )?;
        ensure!(
            configuration.schema_version == 1
                && configuration.account.is_absolute()
                && configuration.root.is_absolute()
                && !configuration.identity.account.is_empty()
                && !configuration.identity.drive.is_empty(),
            "invalid native owner configuration"
        );
        ensure!(
            state.join("native.sqlite3").is_file() && state.join("cache").is_dir(),
            "native state is incomplete; recovery required"
        );
        let bridge = crate::onedrive::open_pinned_session(
            Database::new(state.join("native.sqlite3")),
            &state.join("cache"),
            &configuration.account,
            configuration.identity.clone(),
            true,
        )?;
        let mut coordinator = if let Some(receipt) = configuration.receipt.as_ref() {
            ensure!(
                receipt.identity == configuration.registration,
                "native registration receipt changed"
            );
            NativeCoordinator::resume(bridge, &configuration.root, receipt)?
        } else {
            NativeCoordinator::resume_checkpoint(
                bridge,
                &configuration.root,
                &state.join("root-receipt.json"),
                &configuration.registration,
            )?
        };
        if configuration.receipt.is_none() {
            coordinator.materialize_bootstrap()?;
            configuration.receipt = Some(coordinator.receipt().clone());
            Self::seal(&state, &configuration)?;
        }
        Ok(Self {
            coordinator,
            state,
            configuration,
        })
    }
    pub fn state_directory(&self) -> &Path {
        &self.state
    }
    pub fn owner_matches(&self, identity: &CloudIdentity) -> bool {
        self.configuration.identity == *identity
    }
}

pub struct NativeCoordinator {
    // Cancel directory I/O before releasing the connection and source.
    watcher: Option<DurableDirectoryWatcher>,
    owner: PersistentSession,
    _source: Arc<BoundPublishedSource>,
    bridge: LocalFileBridge,
    last_successful_watch: Option<(i64, i64)>,
    last_residency_watch: Option<(i64, i64)>,
    residency_pending: bool,
}
#[derive(Default, Serialize, Debug)]
pub struct SyncProgress {
    pub uploaded: usize,
    pub deferred: usize,
    pub conflicts: usize,
    pub needs_namespace: bool,
    pub error: Option<String>,
    pub replacements: usize,
    pub created: usize,
    pub renamed: usize,
    pub idle: bool,
    pub deleted: usize,
}
#[derive(Serialize)]
pub struct NativeSummary {
    pub catalogue_files: usize,
    pub catalogue_directories: usize,
    pub logical_bytes: u64,
    pub published: usize,
    pub durable_work_items: usize,
    pub native_event_batch: usize,
    pub rescan_required: bool,
}
impl NativeCoordinator {
    fn is_untracked_artifact(path: &Path, directory: bool) -> bool {
        if directory {
            return false;
        }
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                let lower = name.to_ascii_lowercase();
                lower.starts_with("~$")
                    || lower.ends_with(".tmp")
                    || matches!(
                        lower.as_str(),
                        "desktop.ini" | "thumbs.db" | ".ds_store" | ".lock"
                    )
            })
    }
    fn observed_entries(
        &self,
    ) -> anyhow::Result<Vec<twodrive_local_files::native::persistent::NativeEntry>> {
        let mut entries = self.owner.inventory().map_err(anyhow::Error::msg)?;
        let receipt = serde_json::to_string(self.receipt())?;
        let mut kept = Vec::with_capacity(entries.len());
        for entry in entries.drain(..) {
            if entry.identity.is_none()
                && Self::is_untracked_artifact(&entry.relative_path, entry.directory)
                && self
                    .bridge
                    .journal()
                    .database()
                    .get_by_path(&Self::cloud_path(&entry.relative_path)?)?
                    .is_none()
                && self
                    .bridge
                    .journal()
                    .native_creation_for_path(&receipt, &Self::cloud_path(&entry.relative_path)?)?
                    .is_none()
            {
                continue;
            }
            kept.push(entry);
        }
        Ok(kept)
    }
    /// Aggregate diagnostics only; no user names, tokens or content are returned.
    pub fn summary(&self) -> anyhow::Result<NativeSummary> {
        let records = self.bridge.journal().database().all_records()?;
        let state = self
            .watcher
            .as_ref()
            .context("native watcher stopped")?
            .state()
            .map_err(anyhow::Error::msg)?;
        Ok(NativeSummary {
            catalogue_files: records.iter().filter(|r| !r.metadata.is_dir).count(),
            catalogue_directories: records.iter().filter(|r| r.metadata.is_dir).count(),
            logical_bytes: records.iter().filter(|r| !r.metadata.is_dir).try_fold(
                0u64,
                |sum, r| {
                    sum.checked_add(r.metadata.size)
                        .context("native total size overflow")
                },
            )?,
            published: self
                .bridge
                .journal()
                .native_projections(self.binding()?)?
                .iter()
                .filter(|p| p.state == "published")
                .count(),
            durable_work_items: self.bridge.journal().native_work_ids()?.len(),
            native_event_batch: self
                .bridge
                .journal()
                .database()
                .pending_native_events(self.binding()?, 1000)?
                .len(),
            rescan_required: state.rescan_required,
        })
    }
    fn cloud_path(relative: &Path) -> anyhow::Result<String> {
        let names = relative
            .components()
            .map(|part| match part {
                std::path::Component::Normal(name) => {
                    name.to_str().context("invalid native filename")
                }
                _ => anyhow::bail!("relative native path required"),
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        ensure!(
            !names.is_empty(),
            "native root cannot be created as a child"
        );
        for name in &names {
            twodrive_local_files::names::validate_native_name(name).map_err(anyhow::Error::msg)?;
        }
        let path = format!("/{}", names.join("/"));
        crate::provider::valid_path(&path)?;
        Ok(path)
    }
    pub fn synchronize_new_directory(&mut self, relative: &Path) -> anyhow::Result<()> {
        let path = Self::cloud_path(relative)?;
        let db = self.bridge.journal().database();
        let parent_path = path.rsplit_once('/').unwrap().0;
        let parent = if parent_path.is_empty() {
            MetadataEntry::new_dir(
                db.get_state_value("native_cloud_root_id")?
                    .context("native cloud root missing")?,
                "/",
                0,
                "",
            )
        } else {
            let record = db
                .get_by_path(parent_path)?
                .context("native parent has not synchronized")?;
            ensure!(
                record.metadata.is_dir && !db.has_pending_metadata_at_or_above(parent_path)?,
                "native parent has pending work"
            );
            let mut entry = record.metadata;
            entry.remote_id = record
                .cloud_remote_id
                .context("native parent has no cloud receipt")?;
            entry
        };
        let root_receipt = serde_json::to_string(self.receipt())?;
        let claims = self.bridge.journal().native_creations()?;
        let claim = claims
            .iter()
            .find(|c| c.path == path && c.root_binding == root_receipt);
        let existing = db.get_by_path(&path)?;
        let id = if let Some(record) = &existing {
            ensure!(
                record.metadata.is_dir
                    && claim.is_some_and(|c| c.local_id == record.metadata.remote_id),
                "native directory is already owned"
            );
            record.metadata.remote_id.clone()
        } else {
            claim
                .map(|c| c.local_id.clone())
                .unwrap_or_else(|| format!("local-upload-dir-{:032x}", rand::random::<u128>()))
        };
        self.owner
            .with_local_directory(
                relative,
                id.as_bytes(),
                |volume, file_id| {
                    let physical = format!("{volume:08x}:{file_id:016x}");
                    if let Some(claim) = claim {
                        if claim.native_binding != physical {
                            return Err("native directory physical identity changed".into());
                        }
                        Ok(())
                    } else {
                        self.bridge
                            .journal()
                            .begin_native_creation(&id, &path, &root_receipt, &physical)
                            .map_err(|e| e.to_string())
                    }
                },
                |_, _, _| {
                    if existing.is_none() {
                        db.create_local_directory(&id, &path)
                            .map_err(|e| e.to_string())?;
                    }
                    self.bridge
                        .journal()
                        .stage_native_directory(&id, &parent.remote_id)
                        .map_err(|e| e.to_string())
                },
            )
            .map_err(anyhow::Error::msg)?;
        if self.bridge.journal().dispatch_native_directory(&id)? {
            match self.bridge.backend().create_directory_bound(&parent, &path) {
                Ok(receipt) => self
                    .bridge
                    .journal()
                    .receive_native_directory(&id, &receipt)?,
                Err(error) => {
                    if error
                        .downcast_ref::<crate::provider::TransferNotStarted>()
                        .is_some()
                    {
                        self.bridge.journal().native_directory_not_dispatched(&id)?;
                    }
                    return Err(error);
                }
            }
        }
        let receipt = self.bridge.journal().native_directory_receipt(&id)?;
        ensure!(
            db.complete_metadata_operation(&id, &path, &receipt)?,
            "native directory path changed during cloud creation"
        );
        let root = self.binding()?.to_string();
        self.owner
            .with_local_directory(
                relative,
                id.as_bytes(),
                |volume, file_id| {
                    let expected = self
                        .bridge
                        .journal()
                        .native_creations()
                        .map_err(|e| e.to_string())?;
                    if !expected.iter().any(|c| {
                        c.local_id == id
                            && c.root_binding == root_receipt
                            && c.native_binding == format!("{volume:08x}:{file_id:016x}")
                    }) {
                        return Err("native directory receipt identity changed".into());
                    }
                    Ok(())
                },
                |volume_serial, file_id, acknowledge| {
                    self.bridge
                        .journal()
                        .with_native_directory_ack(
                            &id,
                            &root,
                            &root_receipt,
                            twodrive_core::NativeFileIdentity {
                                volume_serial,
                                file_id,
                            },
                            || acknowledge().map_err(anyhow::Error::msg),
                        )
                        .map_err(|e| e.to_string())
                },
            )
            .map_err(anyhow::Error::msg)
    }
    /// Create-only transfer for a named ordinary file beneath an already owned
    /// directory. The physical creation claim and immutable bytes precede I/O.
    pub fn synchronize_new_file(&mut self, relative: &Path) -> anyhow::Result<bool> {
        let cloud_path = format!(
            "/{}",
            relative
                .components()
                .map(|part| match part {
                    std::path::Component::Normal(name) =>
                        name.to_str().context("invalid new filename"),
                    _ => anyhow::bail!("relative new file required"),
                })
                .collect::<anyhow::Result<Vec<_>>>()?
                .join("/")
        );
        crate::provider::valid_path(&cloud_path)?;
        let root_receipt = serde_json::to_string(self.receipt())?;
        let existing = self.bridge.journal().database().get_by_path(&cloud_path)?;
        let claim = self
            .bridge
            .journal()
            .native_creation_for_path(&root_receipt, &cloud_path)?;
        ensure!(
            !Self::is_untracked_artifact(relative, false) || existing.is_some() || claim.is_some(),
            "unowned temporary/system file is outside new-file synchronization"
        );
        let id = if let Some(record) = existing {
            ensure!(
                !record.metadata.is_dir
                    && record.metadata.remote_id.starts_with("local-upload-")
                    && self
                        .bridge
                        .journal()
                        .native_projections(self.binding()?)?
                        .iter()
                        .all(|p| p.local_id != record.metadata.remote_id),
                "new file already has a published owner"
            );
            ensure!(
                claim
                    .as_ref()
                    .is_some_and(|claim| claim.local_id == record.metadata.remote_id),
                "native creation claim changed"
            );
            record.metadata.remote_id
        } else {
            let id = if let Some(claim) = claim {
                ensure!(
                    claim.state == "preparing",
                    "native creation has a staged owner without metadata"
                );
                claim.local_id
            } else {
                format!("local-upload-{:032x}", rand::random::<u128>())
            };
            self.bridge.create_native_file(&self.owner, relative, &id)?;
            id
        };
        let state = self.bridge.status(&id)?;
        if state.uncertain_upload {
            self.bridge.reconcile(&id)?;
        }
        let state = self.bridge.status(&id)?;
        if state.state == twodrive_core::FileState::Conflict {
            return Ok(false);
        }
        if !state.sync_confirmed() {
            self.bridge
                .capture_native_replacement(&self.owner, relative, &id)?;
            if !self.bridge.upload(&id, crate::local_files::no_progress())? {
                return Ok(false);
            }
        }
        self.bridge
            .acknowledge_native_creation(&self.owner, relative, &id)
    }
    /// Coalesce complete same-path replacements before consuming old physical
    /// delete notifications. The new immutable snapshot is durable first.
    pub fn synchronize_once(&mut self) -> anyhow::Result<SyncProgress> {
        self.drain_observations()?;
        let state = self
            .watcher
            .as_ref()
            .context("native watcher stopped")?
            .state()
            .map_err(anyhow::Error::msg)?;
        if !state.rescan_required
            && self.last_successful_watch == Some((state.epoch, state.generation))
            && self
                .bridge
                .journal()
                .database()
                .pending_native_events(self.binding()?, 1)?
                .is_empty()
            && self
                .bridge
                .journal()
                .native_publications_clean(self.binding()?)?
        {
            return Ok(SyncProgress {
                idle: true,
                ..Default::default()
            });
        }
        // A reconnect or metadata-only notification often has no work. One
        // complete identity inventory can reconcile that gap without walking
        // the same tree again through every mutation consumer.
        if self
            .bridge
            .journal()
            .native_publications_clean(self.binding()?)?
        {
            let expected = self.bridge.journal().native_projections(self.binding()?)?;
            let by_tag = expected
                .iter()
                .map(|p| (p.tag.as_slice(), p))
                .collect::<HashMap<_, _>>();
            let entries = self.observed_entries()?;
            let clean = entries.len() == expected.len()
                && entries.iter().all(|entry| {
                    let Some(p) = entry
                        .identity
                        .as_deref()
                        .and_then(|tag| by_tag.get(tag))
                        .copied()
                    else {
                        return false;
                    };
                    p.volume_serial == Some(entry.volume_serial)
                        && p.native_file_id.as_deref()
                            == Some(format!("{:016x}", entry.file_id).as_str())
                        && Self::relative(&p.path).is_ok_and(|path| path == entry.relative_path)
                        && p.directory == entry.directory
                        && p.size == entry.size
                        && entry
                            .cloud
                            .as_ref()
                            .is_some_and(|os| os.os_in_sync && os.modified_bytes == 0)
                });
            if clean
                && self
                    .bridge
                    .journal()
                    .native_publications_clean(self.binding()?)?
            {
                let db = self.bridge.journal().database();
                let binding = serde_json::to_string(self.receipt())?;
                if state.rescan_required {
                    db.complete_native_watch_scan(&binding, state)?;
                }
                if state.through_sequence > 0 {
                    db.acknowledge_native_observations(&binding, state.through_sequence)?;
                }
                self.last_successful_watch = Some((state.epoch, state.generation));
                return Ok(SyncProgress {
                    idle: true,
                    ..Default::default()
                });
            }
        }
        let deletion = self.synchronize_deletions()?;
        if deletion.waiting != 0 {
            return Ok(SyncProgress {
                deleted: deletion.deleted,
                deferred: deletion.waiting,
                needs_namespace: true,
                error: deletion.error,
                ..Default::default()
            });
        }
        let deleted = deletion.deleted;
        let renamed = self.synchronize_renames()?;
        let binding = self.binding()?.to_string();
        let db = self.bridge.journal().database().clone();
        let projections = self.bridge.journal().native_projections(&binding)?;
        let entries = self.observed_entries()?;
        let by_path = entries
            .iter()
            .map(|entry| (entry.relative_path.as_path(), entry))
            .collect::<HashMap<_, _>>();
        let physical = entries
            .iter()
            .map(|entry| {
                (
                    entry.volume_serial,
                    entry.file_id,
                    entry.identity.as_deref(),
                )
            })
            .collect::<HashSet<_>>();
        let mut candidates = HashMap::new();
        for projection in projections
            .iter()
            .filter(|p| !p.directory && p.state == "published")
        {
            let relative = Self::relative(&projection.path)?;
            let Some(entry) = by_path
                .get(relative.as_path())
                .copied()
                .filter(|entry| !entry.directory)
            else {
                continue;
            };
            let old_physical = projection
                .native_file_id
                .as_deref()
                .map(|id| u64::from_str_radix(id, 16))
                .transpose()?;
            let old_is_present = old_physical.is_some_and(|id| {
                physical.contains(&(
                    projection.volume_serial.unwrap_or(0),
                    id,
                    Some(projection.tag.as_slice()),
                ))
            });
            if !old_is_present
                && entry.volume_serial == self.owner.receipt().volume_serial
                && entry
                    .identity
                    .as_deref()
                    .is_none_or(|tag| tag == projection.tag)
            {
                candidates.insert(
                    projection.tag.clone(),
                    (projection, relative, entry.file_id),
                );
            }
        }
        let events = db.pending_native_events(&binding, 1000)?;
        for (_, payload) in &events {
            let event: twodrive_local_files::native::persistent::NativeEvent =
                serde_json::from_str(payload)?;
            ensure!(
                event.schema_version == 1
                    && event.volume_serial == self.owner.receipt().volume_serial
                    && event.root_file_id == self.owner.receipt().file_id,
                "native event root changed"
            );
            let Some((projection, _, current_id)) = candidates.get(&event.identity) else {
                return Ok(SyncProgress {
                    needs_namespace: true,
                    renamed,
                    deleted,
                    ..Default::default()
                });
            };
            if projection.native_file_id.as_deref()
                != Some(format!("{:016x}", event.file_id).as_str())
                && event.file_id != *current_id
            {
                return Ok(SyncProgress {
                    needs_namespace: true,
                    renamed,
                    deleted,
                    ..Default::default()
                });
            }
        }
        let mut progress = SyncProgress {
            renamed,
            deleted,
            ..Default::default()
        };
        for (projection, relative, current_id) in candidates.values() {
            let id = &projection.local_id;
            if self
                .bridge
                .journal()
                .snapshot_if_owned(id)?
                .as_ref()
                .is_some_and(|state| state.uncertain_upload)
                && let Err(error) = self.bridge.reconcile(id)
            {
                progress.deferred += 1;
                progress.error =
                    Some(twodrive_backend::diagnostics::safe_error(&error).to_string());
                continue;
            }
            let state = self.bridge.journal().snapshot_if_owned(id)?;
            let captured = self.bridge.journal().native_capture_binding(id)?;
            // This exact replacement already has a durable immutable snapshot.
            // Consume only its covered namespace hints, then verify/acknowledge
            // the retained receipt below before considering a newer capture.
            if captured.is_some_and(|capture| {
                capture.physical.volume_serial == self.receipt().volume_serial
                    && capture.physical.file_id == *current_id
                    && state
                        .as_ref()
                        .is_some_and(|state| state.generation == capture.generation)
            }) {
                continue;
            }
            if state
                .as_ref()
                .is_some_and(|state| state.state == twodrive_core::FileState::Conflict)
            {
                progress.deferred += 1;
                progress.conflicts += 1;
                continue;
            }
            if let Err(error) =
                self.bridge
                    .capture_native_replacement(&self.owner, relative, &projection.local_id)
            {
                progress.deferred += 1;
                progress.error =
                    Some(twodrive_backend::diagnostics::safe_error(&error).to_string());
            } else {
                progress.replacements += 1;
            }
        }
        if progress.deferred != 0 {
            return Ok(progress);
        }
        if let Some((through, _)) = events.last() {
            db.acknowledge_native_events(&binding, *through)?;
        }
        // Discover ordinary children only after the existing native namespace
        // decisions above have been durably handled. Parents always precede files.
        let mut entries = self.observed_entries()?;
        entries.sort_by_key(|entry| (entry.relative_path.components().count(), !entry.directory));
        let records = db.all_records()?;
        let records_by_path = records
            .iter()
            .map(|record| (record.metadata.path.as_str(), record))
            .collect::<HashMap<_, _>>();
        let published_ids = projections
            .iter()
            .filter(|p| p.state == "published")
            .map(|p| p.local_id.as_str())
            .collect::<HashSet<_>>();
        for entry in entries {
            let path = Self::cloud_path(&entry.relative_path)?;
            let existing = records_by_path.get(path.as_str()).copied();
            if existing
                .is_some_and(|record| published_ids.contains(record.metadata.remote_id.as_str()))
            {
                continue;
            }
            let owned_pending = existing
                .as_ref()
                .is_some_and(|record| record.metadata.remote_id.starts_with("local-upload-"));
            if entry.identity.is_some() && !owned_pending {
                progress.needs_namespace = true;
                continue;
            }
            let result = if entry.directory {
                self.synchronize_new_directory(&entry.relative_path)
                    .map(|_| true)
            } else {
                self.synchronize_new_file(&entry.relative_path)
            };
            match result {
                Ok(true) => progress.created += 1,
                Ok(false) => progress.deferred += 1,
                Err(error) => {
                    progress.deferred += 1;
                    progress.error =
                        Some(twodrive_backend::diagnostics::safe_error(&error).to_string());
                }
            }
        }
        if progress.needs_namespace || progress.deferred != 0 {
            return Ok(progress);
        }
        let mut data = self.synchronize_known_files_once()?;
        data.replacements = progress.replacements;
        data.created = progress.created;
        data.renamed = progress.renamed;
        data.deleted = progress.deleted;
        Ok(data)
    }
    /// The caller supplies a verified account-owned bridge and an all-or-nothing
    /// plan. Only a new nonexistent root is accepted by the platform layer.
    pub fn create_new(
        bridge: LocalFileBridge,
        plan: &NamespacePlan,
        root: &Path,
        identity: &[u8],
        checkpoint: &Path,
    ) -> anyhow::Result<Self> {
        ensure!(
            root.is_absolute()
                && checkpoint.is_absolute()
                && !root.exists()
                && !checkpoint.exists(),
            "new native root/checkpoint required"
        );
        ensure!(
            bridge.journal().database().all_records()?.is_empty(),
            "new projection requires an empty engine catalogue"
        );
        let metadata = plan
            .entries
            .iter()
            .map(|entry| {
                let modified = time::OffsetDateTime::parse(
                    entry
                        .item
                        .modified
                        .as_deref()
                        .context("missing cloud timestamp")?,
                    &time::format_description::well_known::Rfc3339,
                )?
                .unix_timestamp();
                let path = entry.cloud_path();
                Ok(if entry.item.kind == "folder" {
                    MetadataEntry::new_dir(&entry.item.id, path, modified, &entry.item.etag)
                } else {
                    MetadataEntry::new_file(
                        &entry.item.id,
                        path,
                        entry.item.size,
                        modified,
                        &entry.item.etag,
                    )
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        bridge
            .journal()
            .database()
            .upsert_metadata_batch(&metadata)?;
        bridge
            .journal()
            .database()
            .set_state_value("native_cloud_root_id", &plan.root_id)?;
        let binding = std::str::from_utf8(identity)?.to_string();
        let source = Arc::new(BoundPublishedSource::unbound(bridge.clone()));
        let sink = Arc::new(
            DatabaseEventSink::new(bridge.journal().database().clone(), binding)
                .map_err(anyhow::Error::msg)?,
        );
        let owner = PersistentSession::create_new_recording(
            root,
            identity,
            checkpoint,
            source.clone(),
            sink,
        )
        .map_err(anyhow::Error::msg)?;
        source.bind_root(&owner)?;
        let mut coordinator = Self {
            last_successful_watch: None,
            last_residency_watch: None,
            residency_pending: false,
            watcher: None,
            owner,
            _source: source,
            bridge,
        };
        coordinator.materialize_bootstrap()?;
        Ok(coordinator)
    }
    pub fn resume(
        bridge: LocalFileBridge,
        root: &Path,
        receipt: &RootReceipt,
    ) -> anyhow::Result<Self> {
        let source = Arc::new(BoundPublishedSource::from_receipt(
            bridge.clone(),
            receipt.clone(),
        )?);
        let binding = std::str::from_utf8(&receipt.identity)?.to_string();
        let sink = Arc::new(
            DatabaseEventSink::new(bridge.journal().database().clone(), binding)
                .map_err(anyhow::Error::msg)?,
        );
        let owner = PersistentSession::resume_recording(root, receipt, source.clone(), sink)
            .map_err(anyhow::Error::msg)?;
        let watcher = owner
            .watch_directory_durable(bridge.journal().database().clone())
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            watcher: Some(watcher),
            last_successful_watch: None,
            last_residency_watch: None,
            residency_pending: false,
            owner,
            _source: source,
            bridge,
        })
    }
    pub fn resume_checkpoint(
        bridge: LocalFileBridge,
        root: &Path,
        checkpoint: &Path,
        registration: &[u8],
    ) -> anyhow::Result<Self> {
        let source = Arc::new(BoundPublishedSource::unbound(bridge.clone()));
        let binding = std::str::from_utf8(registration)?.to_string();
        let sink = Arc::new(
            DatabaseEventSink::new(bridge.journal().database().clone(), binding)
                .map_err(anyhow::Error::msg)?,
        );
        let owner = PersistentSession::resume_expected_checkpoint_recording(
            root,
            checkpoint,
            registration,
            source.clone(),
            sink,
        )
        .map_err(anyhow::Error::msg)?;
        source.bind_root(&owner)?;
        let watcher = owner
            .watch_directory_durable(bridge.journal().database().clone())
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            watcher: Some(watcher),
            last_successful_watch: None,
            last_residency_watch: None,
            residency_pending: false,
            owner,
            _source: source,
            bridge,
        })
    }
    pub fn root(&self) -> &Path {
        self.owner.root()
    }
    pub fn receipt(&self) -> &RootReceipt {
        self.owner.receipt()
    }
    fn binding(&self) -> anyhow::Result<&str> {
        Ok(std::str::from_utf8(&self.owner.receipt().identity)?)
    }
    fn relative(path: &str) -> anyhow::Result<PathBuf> {
        crate::provider::valid_path(path)?;
        ensure!(path != "/", "root has no projected leaf");
        Ok(path[1..].split('/').collect())
    }
    /// Finish an interrupted initial projection. Existing entries are checked,
    /// never overwritten. Call before exposing a newly prepared root to users.
    pub fn materialize_bootstrap(&mut self) -> anyhow::Result<()> {
        // Do not fill the bounded observation queue with tens of thousands of
        // provider-owned initial publications. Re-arm before the mandatory full
        // inventory; CFAPI mutation callbacks remain durable throughout.
        self.watcher.take();
        let mut records = self.bridge.journal().database().all_records()?;
        records.sort_by(|a, b| {
            a.metadata
                .path
                .matches('/')
                .count()
                .cmp(&b.metadata.path.matches('/').count())
                .then_with(|| b.metadata.is_dir.cmp(&a.metadata.is_dir))
                .then_with(|| a.metadata.path.cmp(&b.metadata.path))
        });
        let binding = self.binding()?.to_string();
        for record in &records {
            let id = &record.metadata.remote_id;
            let existing = self.bridge.journal().native_projection_for_id(id)?;
            if let Some(p) = existing.as_ref().filter(|p| p.state == "published") {
                ensure!(
                    p.root_binding == binding,
                    "projection belongs to another root"
                );
                continue;
            }
            let tag = existing
                .as_ref()
                .map(|p| p.tag.clone())
                .unwrap_or_else(|| rand::random::<[u8; 32]>().to_vec());
            let projection = self
                .bridge
                .journal()
                .prepare_native_projection(id, &binding, &tag)?;
            let relative = Self::relative(&projection.path)?;
            if !self
                .owner
                .entry_exists(&relative)
                .map_err(anyhow::Error::msg)?
            {
                self.owner
                    .publish_placeholder(
                        &relative,
                        &tag,
                        projection.size,
                        projection.directory,
                        projection.modified_unix,
                    )
                    .map_err(anyhow::Error::msg)?;
            }
            self.owner
                .with_publication_binding(
                    &relative,
                    &tag,
                    projection.size,
                    projection.directory,
                    |volume, file_id| {
                        self.bridge
                            .journal()
                            .complete_native_projection(id, &binding, &tag, volume, file_id)
                            .map_err(|e| e.to_string())
                    },
                )
                .map_err(anyhow::Error::msg)?;
        }
        self.watcher = Some(
            self.owner
                .watch_directory_durable(self.bridge.journal().database().clone())
                .map_err(anyhow::Error::msg)?,
        );
        self.verify_bootstrap_inventory()?;
        Ok(())
    }
    pub fn poll_observations(&mut self, timeout_ms: u32) -> anyhow::Result<bool> {
        Ok(self
            .watcher
            .as_mut()
            .context("native watcher stopped")?
            .poll(timeout_ms)
            .map_err(anyhow::Error::msg)?
            .is_some())
    }
    /// Consume data changes only while the namespace is stable. Creation,
    /// replacement, rename and deletion are left durable for their consumer.
    pub fn synchronize_known_files_once(&mut self) -> anyhow::Result<SyncProgress> {
        self.drain_observations()?;
        let binding = self.binding()?.to_string();
        let db = self.bridge.journal().database().clone();
        let watcher = self.watcher.as_ref().context("native watcher stopped")?;
        let watermark = watcher.state().map_err(anyhow::Error::msg)?;
        let observation_binding = watcher.binding().to_string();
        let mut progress = SyncProgress::default();
        if !db.pending_native_events(&binding, 1)?.is_empty() {
            progress.needs_namespace = true;
            return Ok(progress);
        }
        let expected = self.bridge.journal().native_projections(&binding)?;
        let durable_work = self
            .bridge
            .journal()
            .native_work_ids()?
            .into_iter()
            .collect::<HashSet<_>>();
        let entries = self.observed_entries()?;
        let by_tag = expected
            .iter()
            .map(|p| (p.tag.as_slice(), p))
            .collect::<HashMap<_, _>>();
        if entries.len() != expected.len() {
            progress.needs_namespace = true;
            return Ok(progress);
        }
        let mut work = Vec::new();
        for entry in entries {
            let Some(projection) = entry
                .identity
                .as_deref()
                .and_then(|tag| by_tag.get(tag))
                .copied()
            else {
                progress.needs_namespace = true;
                return Ok(progress);
            };
            let published_matches = projection.volume_serial == Some(entry.volume_serial)
                && projection.native_file_id.as_deref()
                    == Some(format!("{:016x}", entry.file_id).as_str());
            let captured_matches = if published_matches {
                false
            } else {
                self.bridge
                    .journal()
                    .native_capture_binding(&projection.local_id)?
                    .is_some_and(|binding| {
                        binding.physical.volume_serial == entry.volume_serial
                            && binding.physical.file_id == entry.file_id
                    })
            };
            if entry.relative_path != Self::relative(&projection.path)?
                || projection.volume_serial != Some(entry.volume_serial)
                || (!captured_matches
                    && projection.native_file_id.as_deref()
                        != Some(format!("{:016x}", entry.file_id).as_str()))
                || entry.directory != projection.directory
            {
                progress.needs_namespace = true;
                return Ok(progress);
            }
            if !entry.directory {
                if !durable_work.contains(&projection.local_id)
                    && entry.size == projection.size
                    && entry
                        .cloud
                        .as_ref()
                        .is_some_and(|cloud| cloud.os_in_sync && cloud.modified_bytes == 0)
                {
                    continue;
                }
                work.push((entry, projection.clone()));
            }
        }
        for (entry, projection) in work {
            if !db.pending_native_events(&binding, 1)?.is_empty() {
                progress.needs_namespace = true;
                break;
            }
            let id = &projection.local_id;
            let state = self.bridge.journal().snapshot_if_owned(id)?;
            if state.as_ref().is_some_and(|state| state.uncertain_upload)
                && let Err(error) = self.bridge.reconcile(id)
            {
                progress.deferred += 1;
                progress.error =
                    Some(twodrive_backend::diagnostics::safe_error(&error).to_string());
                continue;
            }
            if self
                .bridge
                .journal()
                .snapshot_if_owned(id)?
                .as_ref()
                .is_some_and(|state| state.state == twodrive_core::FileState::Conflict)
            {
                progress.conflicts += 1;
                continue;
            }
            let native_dirty = entry
                .cloud
                .as_ref()
                .is_none_or(|native| !native.os_in_sync || native.modified_bytes != 0)
                || entry.size != projection.size
                || self.bridge.journal().native_dirty(id)?
                || self
                    .bridge
                    .journal()
                    .snapshot_if_owned(id)?
                    .as_ref()
                    .is_some_and(|state| !state.sync_confirmed());
            if !native_dirty {
                continue;
            }
            let path = self.root().join(&entry.relative_path);
            let result = (|| -> anyhow::Result<bool> {
                let retained = self.bridge.journal().native_capture_binding(id)?;
                if self
                    .bridge
                    .journal()
                    .snapshot_if_owned(id)?
                    .as_ref()
                    .is_some_and(|state| {
                        state.sync_confirmed()
                            && !state.native_dirty
                            && retained
                                .is_some_and(|capture| capture.generation == state.generation)
                    })
                    && self.bridge.acknowledge_native(&path, id)?
                {
                    return Ok(true);
                }
                self.bridge.capture_native_edit(&path, id)?;
                ensure!(
                    self.bridge.upload(id, crate::local_files::no_progress())?,
                    "native upload receipt needs reconciliation"
                );
                self.bridge.acknowledge_native(&path, id)
            })();
            match result {
                Ok(true) => progress.uploaded += 1,
                Ok(false) => progress.deferred += 1,
                Err(error) => {
                    progress.deferred += 1;
                    progress.error =
                        Some(twodrive_backend::diagnostics::safe_error(&error).to_string());
                }
            }
        }
        if !db.pending_native_events(&binding, 1)?.is_empty() {
            progress.needs_namespace = true;
        }
        if progress.deferred == 0 && progress.conflicts == 0 && !progress.needs_namespace {
            if watermark.rescan_required {
                db.complete_native_watch_scan(&observation_binding, watermark)?;
            }
            if watermark.through_sequence > 0 {
                db.acknowledge_native_observations(
                    &observation_binding,
                    watermark.through_sequence,
                )?;
            }
            self.last_successful_watch = Some((watermark.epoch, watermark.generation));
        }
        Ok(progress)
    }
    fn drain_observations(&mut self) -> anyhow::Result<()> {
        for _ in 0..1000 {
            if !self.poll_observations(0)? {
                return Ok(());
            }
        }
        anyhow::bail!("native namespace remains busy")
    }
    fn inventory_matches(&self, expected: &[NativeProjection]) -> anyhow::Result<()> {
        let entries = self.observed_entries()?;
        ensure!(
            entries.len() == expected.len(),
            "native bootstrap has unexpected or missing entries"
        );
        let by_tag = expected
            .iter()
            .map(|p| (p.tag.as_slice(), p))
            .collect::<HashMap<_, _>>();
        for entry in entries {
            let tag = entry
                .identity
                .as_deref()
                .context("ordinary entry needs native reconciliation")?;
            let p = by_tag.get(tag).context("unknown native bootstrap tag")?;
            ensure!(
                p.state == "published"
                    && entry.relative_path == Self::relative(&p.path)?
                    && entry.volume_serial == self.owner.receipt().volume_serial
                    && p.volume_serial == Some(entry.volume_serial)
                    && p.native_file_id.as_deref()
                        == Some(format!("{:016x}", entry.file_id).as_str())
                    && entry.directory == p.directory
                    && entry.size == p.size
                    && entry
                        .cloud
                        .is_some_and(|c| c.os_in_sync && c.modified_bytes == 0),
                "native bootstrap changed before verification"
            );
        }
        Ok(())
    }
    pub fn verify_bootstrap_inventory(&mut self) -> anyhow::Result<()> {
        let binding = self.binding()?.to_string();
        let db = self.bridge.journal().database().clone();
        for _ in 0..3 {
            self.drain_observations()?;
            let watcher = self.watcher.as_ref().context("native watcher stopped")?;
            let before = watcher.state().map_err(anyhow::Error::msg)?;
            let observation_binding = watcher.binding().to_string();
            let expected = self.bridge.journal().native_projections(&binding)?;
            ensure!(
                db.pending_native_events(&binding, 1)?.is_empty(),
                "native mutation needs reconciliation before bootstrap"
            );
            self.inventory_matches(&expected)?;
            self.drain_observations()?;
            if db.complete_native_watch_scan(&observation_binding, before)? {
                // A later epoch/batch cannot be erased by this older inventory.
                if before.through_sequence > 0 {
                    db.acknowledge_native_observations(
                        &observation_binding,
                        before.through_sequence,
                    )?;
                }
                return Ok(());
            }
        }
        anyhow::bail!("native inventory changed during bootstrap")
    }
    pub fn disconnect(&mut self) -> anyhow::Result<()> {
        self.watcher.take();
        self.owner.disconnect().map_err(anyhow::Error::msg)
    }
    #[cfg(feature = "native-test-hooks")]
    pub fn retire_empty_generated_fixture(&mut self) -> anyhow::Result<()> {
        self.watcher.take();
        self.owner
            .retire_empty_office_fixture()
            .map_err(anyhow::Error::msg)
    }
}

#[cfg(all(test, feature = "native-test-hooks"))]
mod tests {
    use super::*;
    use crate::{model::CloudBackend, native_local_fixture::GeneratedProvider, provider::Backend};
    use twodrive_core::{Database, IndexedItem};
    use twodrive_local_files::fixtures::{Dataset, DownloadCounters};
    #[test]
    #[ignore = "explicit NEW generated root; remote version update, native-before-SQL interruption and local edit preservation"]
    fn remote_file_version_restarts_without_overwriting_local_edits() {
        let root = PathBuf::from(
            std::env::var_os("TWODRIVE_APPROVED_REMOTE_UPDATE_ROOT")
                .expect("explicit generated root"),
        );
        assert!(
            root.is_absolute()
                && !root.exists()
                && root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("twodrive-cfapi-fixture-office-")
        );
        let state = tempfile::tempdir().unwrap();
        let private = state.path().canonicalize().unwrap();
        let cache = private.join("cache");
        std::fs::create_dir(&cache).unwrap();
        let provider = crate::local_files::tests::MockProvider::new();
        let initial = provider.list_all().unwrap().remove(0);
        let plan = NamespacePlan::build(
            &[
                IndexedItem {
                    id: "root".into(),
                    parent: None,
                    name: "root".into(),
                    kind: "folder".into(),
                    size: 0,
                    etag: "v1".into(),
                    modified: None,
                    deleted: false,
                },
                IndexedItem {
                    id: initial.remote_id.clone(),
                    parent: Some("root".into()),
                    name: initial.name.clone(),
                    kind: "file".into(),
                    size: initial.size,
                    etag: initial.etag.clone(),
                    modified: Some("1970-01-01T00:00:01Z".into()),
                    deleted: false,
                },
            ],
            "root",
            100,
            twodrive_local_files::names::ordinal_ignorecase_cmp,
        )
        .unwrap();
        let db = Database::new(private.join("native.sqlite3"));
        let open = || {
            LocalFileBridge::open_bound(
                db.clone(),
                &cache,
                Backend::new(provider.clone()),
                "generated-remote-update",
            )
            .unwrap()
        };
        let checkpoint = root.with_extension("receipt.json");
        let mut owner = NativeCoordinator::create_new(
            open(),
            &plan,
            &root,
            b"twodrive-dev-native-v1:generated-office-only",
            &checkpoint,
        )
        .unwrap();
        let file = owner.root().join(&initial.name);
        let before = std::fs::read(&file).unwrap();
        let old_cache = db
            .get_by_remote_id(&initial.remote_id)
            .unwrap()
            .unwrap()
            .cache_path
            .unwrap();
        twodrive_local_files::native::pin(&file, true).unwrap();
        provider.change(b"generated newer remote complete content");
        let remote = provider.list_all().unwrap();
        crate::local_files::INTERRUPT_REMOTE_AFTER_NATIVE_UPDATE.with(|flag| flag.set(true));
        let interrupted = owner.apply_remote_metadata(&remote).unwrap();
        let pending = owner
            .bridge
            .journal()
            .native_remote_updates(owner.binding().unwrap())
            .unwrap()
            .len();
        let receipt = owner.receipt().clone();
        owner.disconnect().unwrap();
        drop(owner);
        owner = NativeCoordinator::resume(open(), &root, &receipt).unwrap();
        let resumed = owner.apply_remote_metadata(&remote).unwrap();
        let after = std::fs::read(&file).unwrap();
        use crate::provider::Provider;
        let current = provider.get_metadata(&initial.remote_id).unwrap().unwrap();
        provider
            .move_bound(
                &current,
                &MetadataEntry::new_dir("root", "/", 0, ""),
                "/remote-renamed.txt",
            )
            .unwrap();
        crate::local_files::INTERRUPT_REMOTE_AFTER_RENAME.with(|flag| flag.set(true));
        let rename_interrupted = owner
            .apply_remote_metadata(&provider.list_all().unwrap())
            .unwrap();
        let receipt = owner.receipt().clone();
        owner.disconnect().unwrap();
        drop(owner);
        owner = NativeCoordinator::resume(open(), &root, &receipt).unwrap();
        let moved = owner
            .apply_remote_metadata(&provider.list_all().unwrap())
            .unwrap();
        let directory = provider
            .create_directory_bound(
                &MetadataEntry::new_dir("root", "/", 0, ""),
                "/remote-folder",
            )
            .unwrap();
        let directory_created = owner
            .apply_remote_metadata(&[
                directory.clone(),
                provider.get_metadata(&initial.remote_id).unwrap().unwrap(),
            ])
            .unwrap();
        let current = provider.get_metadata(&initial.remote_id).unwrap().unwrap();
        let child = provider
            .move_bound(&current, &directory, "/remote-folder/remote-renamed.txt")
            .unwrap();
        let child_moved = owner
            .apply_remote_metadata(&[directory.clone(), child])
            .unwrap();
        let directory_new = provider
            .move_bound(
                &directory,
                &MetadataEntry::new_dir("root", "/", 0, ""),
                "/remote-folder-renamed",
            )
            .unwrap();
        let directory_moved = owner
            .apply_remote_metadata(&[
                directory_new,
                provider.get_metadata(&initial.remote_id).unwrap().unwrap(),
            ])
            .unwrap();
        let renamed_file = owner
            .root()
            .join("remote-folder-renamed/remote-renamed.txt");
        let renamed_bytes = std::fs::read(&renamed_file);
        let pin_retained = owner
            .owner
            .inventory()
            .unwrap()
            .iter()
            .find(|e| !e.directory)
            .unwrap()
            .cloud
            .as_ref()
            .unwrap()
            .pinned;
        let old_preserved = std::fs::read(&old_cache).unwrap() == before
            && !db.remove_unreferenced_cache(&old_cache).unwrap();
        let native_write = std::fs::write(&renamed_file, b"generated local user edit must survive");
        provider.change(b"generated concurrent remote change");
        let concurrent = owner
            .apply_remote_metadata(&provider.list_all().unwrap())
            .unwrap();
        let local = std::fs::read(&renamed_file);
        let cloud = provider.download(&initial.remote_id).unwrap();
        let entries = owner.owner.inventory().unwrap();
        for entry in entries.iter().filter(|e| !e.directory) {
            assert!(entry.size < 1024);
            std::fs::remove_file(owner.root().join(&entry.relative_path)).unwrap();
        }
        let mut dirs = entries.iter().filter(|e| e.directory).collect::<Vec<_>>();
        dirs.sort_by_key(|e| std::cmp::Reverse(e.relative_path.components().count()));
        for entry in dirs {
            std::fs::remove_dir(owner.root().join(&entry.relative_path)).unwrap();
        }
        owner.retire_empty_generated_fixture().unwrap();
        drop(owner);
        std::fs::remove_dir(root).unwrap();
        std::fs::remove_file(checkpoint).unwrap();
        state.close().unwrap();
        assert_eq!(interrupted.deferred, 1, "{interrupted:?}");
        assert_eq!(pending, 1);
        assert_eq!(resumed.deferred, 0, "{resumed:?}");
        assert_eq!(after, b"generated newer remote complete content");
        assert!(old_preserved && pin_retained);
        assert_eq!(moved.deferred, 0, "{moved:?}");
        assert_eq!(directory_created.deferred, 0, "{directory_created:?}");
        assert_eq!(child_moved.deferred, 0, "{child_moved:?}");
        assert_eq!(directory_moved.deferred, 0, "{directory_moved:?}");
        assert_eq!(rename_interrupted.deferred, 1, "{rename_interrupted:?}");
        assert!(moved.updated + moved.moved >= 1, "{moved:?}");
        assert_eq!(
            renamed_bytes.unwrap(),
            b"generated newer remote complete content"
        );
        assert!(concurrent.deferred > 0);
        assert!(
            native_write.is_ok(),
            "{native_write:?}; child={child_moved:?} dir={directory_moved:?}"
        );
        assert_eq!(local.unwrap(), b"generated local user edit must survive");
        assert_eq!(cloud, b"generated concurrent remote change");
    }
    #[test]
    #[ignore = "explicit NEW generated root; confirmed remote deletion retains cached revision"]
    fn remote_deleted_file_is_retired_with_recoverable_private_bytes() {
        let root = PathBuf::from(
            std::env::var_os("TWODRIVE_APPROVED_REMOTE_UPDATE_ROOT")
                .expect("explicit generated root"),
        );
        assert!(
            root.is_absolute()
                && !root.exists()
                && root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("twodrive-cfapi-fixture-office-")
        );
        let state = tempfile::tempdir().unwrap();
        let private = state.path().canonicalize().unwrap();
        let cache = private.join("cache");
        std::fs::create_dir(&cache).unwrap();
        let provider = crate::local_files::tests::MockProvider::new();
        let item = provider.list_all().unwrap().remove(0);
        let plan = NamespacePlan::build(
            &[
                IndexedItem {
                    id: "root".into(),
                    parent: None,
                    name: "root".into(),
                    kind: "folder".into(),
                    size: 0,
                    etag: "v1".into(),
                    modified: None,
                    deleted: false,
                },
                IndexedItem {
                    id: item.remote_id.clone(),
                    parent: Some("root".into()),
                    name: item.name.clone(),
                    kind: "file".into(),
                    size: item.size,
                    etag: item.etag.clone(),
                    modified: Some("1970-01-01T00:00:01Z".into()),
                    deleted: false,
                },
            ],
            "root",
            100,
            twodrive_local_files::names::ordinal_ignorecase_cmp,
        )
        .unwrap();
        let db = Database::new(private.join("native.sqlite3"));
        let open = || {
            LocalFileBridge::open_bound(
                db.clone(),
                &cache,
                Backend::new(provider.clone()),
                "generated-remote-removal",
            )
            .unwrap()
        };
        let checkpoint = root.with_extension("receipt.json");
        let mut owner = NativeCoordinator::create_new(
            open(),
            &plan,
            &root,
            b"twodrive-dev-native-v1:generated-office-only",
            &checkpoint,
        )
        .unwrap();
        let path = owner.root().join(&item.name);
        let bytes = std::fs::read(&path).unwrap();
        let archived = db
            .get_by_remote_id(&item.remote_id)
            .unwrap()
            .unwrap()
            .cache_path
            .unwrap();
        let premature = owner.apply_remote_removals(&HashSet::new()).unwrap();
        use crate::provider::Provider;
        provider.delete_bound(&item).unwrap();
        crate::local_files::INTERRUPT_REMOTE_AFTER_REMOVAL.with(|f| f.set(true));
        let interrupted = owner.apply_remote_removals(&HashSet::new()).unwrap();
        let receipt = owner.receipt().clone();
        owner.disconnect().unwrap();
        drop(owner);
        owner = NativeCoordinator::resume(open(), &root, &receipt).unwrap();
        let progress = owner.apply_remote_removals(&HashSet::new()).unwrap();
        let missing = !path.exists();
        let retained = std::fs::read(&archived).unwrap();
        let guarded = !db.remove_unreferenced_cache(&archived).unwrap();
        if path.exists() {
            std::fs::remove_file(path).unwrap();
        }
        owner.retire_empty_generated_fixture().unwrap();
        drop(owner);
        std::fs::remove_dir(root).unwrap();
        std::fs::remove_file(checkpoint).unwrap();
        state.close().unwrap();
        assert_eq!(premature.removed, 0);
        assert!(premature.deferred > 0);
        assert_eq!(interrupted.deferred, 1, "{interrupted:?}");
        assert_eq!(progress.removed, 1, "{progress:?}");
        assert_eq!(progress.deferred, 0, "{progress:?}");
        assert!(missing && guarded);
        assert_eq!(retained, bytes);
    }
    #[test]
    #[ignore = "explicit NEW generated root; Shell pin/free-space, private cache removal and dirty veto"]
    fn native_pin_release_rehydrates_and_preserves_dirty_data() {
        let root = PathBuf::from(
            std::env::var_os("TWODRIVE_APPROVED_REMOTE_UPDATE_ROOT")
                .expect("explicit generated root"),
        );
        assert!(
            root.is_absolute()
                && !root.exists()
                && root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("twodrive-cfapi-fixture-office-")
        );
        let state = tempfile::tempdir().unwrap();
        let private = state.path().canonicalize().unwrap();
        let cache = private.join("cache");
        std::fs::create_dir(&cache).unwrap();
        let provider = crate::local_files::tests::MockProvider::new();
        let item = provider.list_all().unwrap().remove(0);
        let plan = NamespacePlan::build(
            &[
                IndexedItem {
                    id: "root".into(),
                    parent: None,
                    name: "root".into(),
                    kind: "folder".into(),
                    size: 0,
                    etag: "v1".into(),
                    modified: None,
                    deleted: false,
                },
                IndexedItem {
                    id: item.remote_id.clone(),
                    parent: Some("root".into()),
                    name: item.name.clone(),
                    kind: "file".into(),
                    size: item.size,
                    etag: item.etag.clone(),
                    modified: Some("1970-01-01T00:00:01Z".into()),
                    deleted: false,
                },
            ],
            "root",
            100,
            twodrive_local_files::names::ordinal_ignorecase_cmp,
        )
        .unwrap();
        let db = Database::new(private.join("native.sqlite3"));
        let open = || {
            LocalFileBridge::open_bound(
                db.clone(),
                &cache,
                Backend::new(provider.clone()),
                "generated-residency",
            )
            .unwrap()
        };
        let checkpoint = root.with_extension("receipt.json");
        let mut owner = NativeCoordinator::create_new(
            open(),
            &plan,
            &root,
            b"twodrive-dev-native-v1:generated-office-only",
            &checkpoint,
        )
        .unwrap();
        let path = owner.root().join(&item.name);
        twodrive_local_files::native::pin(&path, true).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let pinned = loop {
            let result = owner.synchronize_residency().unwrap();
            let current = owner.owner.inventory().unwrap().remove(0);
            if current.cloud.as_ref().unwrap().resident {
                break result;
            }
            assert!(std::time::Instant::now() < deadline, "{result:?}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let record = db.get_by_remote_id(&item.remote_id).unwrap().unwrap();
        let cached = record.cache_path.clone().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        twodrive_local_files::native::test_release_intent(&path).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let released = loop {
            let result = owner.synchronize_residency().unwrap();
            if result.released == 1 || std::time::Instant::now() >= deadline {
                break result;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let online = owner.owner.inventory().unwrap().remove(0).cloud.unwrap();
        let private_removed = !cached.exists();
        let rehydrated = std::fs::read(&path).unwrap();
        std::fs::write(&path, b"generated unsynchronized native bytes must remain").unwrap();
        let dirty = owner.synchronize_residency().unwrap();
        let local = std::fs::read(&path).unwrap();
        let current_cache = db
            .get_by_remote_id(&item.remote_id)
            .unwrap()
            .unwrap()
            .cache_path
            .unwrap();
        let private_preserved = current_cache.exists();
        std::fs::remove_file(&path).unwrap();
        owner.retire_empty_generated_fixture().unwrap();
        drop(owner);
        std::fs::remove_dir(root).unwrap();
        std::fs::remove_file(checkpoint).unwrap();
        state.close().unwrap();
        assert_eq!(pinned.deferred, 0, "{pinned:?}");
        assert!(record.effective_pinned());
        assert_eq!(released.released, 1, "{released:?}");
        assert!(!online.resident && private_removed);
        assert_eq!(bytes, rehydrated);
        assert_eq!(dirty.released, 0, "{dirty:?}");
        assert!(private_preserved);
        assert_eq!(local, b"generated unsynchronized native bytes must remain");
    }
    #[test]
    #[ignore = "explicit NEW generated root; resume a legacy-owned TMP creation after a definite paused upload"]
    fn owned_artifact_creation_resumes_after_restart() {
        artifact_creation_recovery(true);
    }
    #[test]
    #[ignore = "explicit NEW generated root; prepared artifact claim survives before native conversion/metadata"]
    fn prepared_artifact_claim_resumes_before_conversion() {
        artifact_creation_recovery(false);
    }
    fn artifact_creation_recovery(captured: bool) {
        let root = PathBuf::from(
            std::env::var_os("TWODRIVE_APPROVED_OWNED_ARTIFACT_ROOT")
                .expect("explicit generated root"),
        );
        assert!(
            root.is_absolute()
                && !root.exists()
                && root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("twodrive-cfapi-fixture-office-")
        );
        let checkpoint = root.with_extension("receipt.json");
        let state = tempfile::tempdir().unwrap();
        let private = state.path().canonicalize().unwrap();
        let cache = private.join("cache");
        std::fs::create_dir(&cache).unwrap();
        let pause = private.join("pause");
        std::fs::write(&pause, b"generated pause").unwrap();
        let provider = crate::local_files::tests::MockProvider::empty_at("/legacy.tmp");
        let plan = NamespacePlan::build(
            &[IndexedItem {
                id: "root".into(),
                parent: None,
                name: "root".into(),
                kind: "folder".into(),
                size: 0,
                etag: "root-v1".into(),
                modified: None,
                deleted: false,
            }],
            "root",
            100,
            twodrive_local_files::names::ordinal_ignorecase_cmp,
        )
        .unwrap();
        let db = Database::new(private.join("native.sqlite3"));
        let open = || {
            LocalFileBridge::open_bound(
                db.clone(),
                &cache,
                Backend::new(provider.clone()).with_pause_file(pause.clone()),
                "generated-owned-artifact",
            )
            .unwrap()
        };
        let mut coordinator = NativeCoordinator::create_new(
            open(),
            &plan,
            &root,
            b"twodrive-dev-native-v1:generated-office-only",
            &checkpoint,
        )
        .unwrap();
        let relative = Path::new("legacy.tmp");
        let file = coordinator.root().join(relative);
        let bytes = b"generated legacy creation is owned and must remain recoverable";
        std::fs::write(&file, bytes).unwrap();
        let id = "local-upload-legacy-artifact";
        // Model a previous version which captured the file before this policy.
        if captured {
            coordinator
                .bridge
                .create_native_file(&coordinator.owner, relative, id)
                .unwrap();
            assert!(
                coordinator
                    .bridge
                    .upload(id, crate::local_files::no_progress())
                    .unwrap_err()
                    .is::<crate::provider::TransferNotStarted>()
            );
            assert!(!coordinator.bridge.status(id).unwrap().uncertain_upload);
        } else {
            let receipt = serde_json::to_string(coordinator.receipt()).unwrap();
            let result = coordinator.owner.with_replacement_capture_staged(
                relative,
                id.as_bytes(),
                |volume, file_id| {
                    coordinator
                        .bridge
                        .journal()
                        .begin_native_creation(
                            id,
                            "/legacy.tmp",
                            &receipt,
                            &format!("{volume:08x}:{file_id:016x}"),
                        )
                        .map_err(|e| e.to_string())?;
                    Err("generated interruption before conversion".into())
                },
                |_| Ok(()),
            );
            assert!(result.is_err());
            assert!(
                coordinator
                    .bridge
                    .journal()
                    .database()
                    .get_by_path("/legacy.tmp")
                    .unwrap()
                    .is_none()
            );
        }
        let receipt = coordinator.receipt().clone();
        coordinator.disconnect().unwrap();
        drop(coordinator);
        std::fs::remove_file(&pause).unwrap();
        let mut coordinator = NativeCoordinator::resume(open(), &root, &receipt).unwrap();
        let progress = coordinator.synchronize_once().unwrap();
        let recovered = coordinator.bridge.status(id).unwrap().sync_confirmed();
        let published = coordinator
            .bridge
            .journal()
            .native_projection_for_id(id)
            .unwrap()
            .is_some();
        let retained = std::fs::read(&file).unwrap();
        std::fs::remove_file(file).unwrap();
        coordinator.retire_empty_generated_fixture().unwrap();
        drop(coordinator);
        std::fs::remove_dir(root).unwrap();
        std::fs::remove_file(checkpoint).unwrap();
        state.close().unwrap();
        assert_eq!(retained, bytes);
        assert_eq!(
            progress.created, 1,
            "legacy owned claim was stranded: {progress:?}"
        );
        assert_eq!(progress.deferred, 0, "{progress:?}");
        assert!(recovered && published);
    }
    #[test]
    #[ignore = "explicit NEW generated CFAPI root; new ordinary file through create-only receipt and published binding"]
    fn new_native_file_becomes_a_published_cloud_object_without_an_old_download() {
        new_file_namespace_case(false);
    }
    #[test]
    #[ignore = "explicit NEW generated root; rename/delete batch, prepare interruption, restart and unrelated progress"]
    fn deletion_prepare_restart_retires_prior_rename_history() {
        new_file_namespace_case(true);
    }
    fn new_file_namespace_case(interrupt_delete: bool) {
        let root = PathBuf::from(
            std::env::var_os("TWODRIVE_APPROVED_COORDINATOR_NEW_FILE_ROOT")
                .expect("explicit generated root"),
        );
        assert!(
            root.is_absolute()
                && !root.exists()
                && root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("twodrive-cfapi-fixture-office-")
        );
        let checkpoint = root.with_extension("receipt.json");
        let state = tempfile::tempdir().unwrap();
        let private = state.path().canonicalize().unwrap();
        let cache = private.join("cache");
        std::fs::create_dir(&cache).unwrap();
        let plan = NamespacePlan::build(
            &[IndexedItem {
                id: "root".into(),
                parent: None,
                name: "root".into(),
                kind: "folder".into(),
                size: 0,
                etag: "v1".into(),
                modified: None,
                deleted: false,
            }],
            "root",
            100,
            twodrive_local_files::names::ordinal_ignorecase_cmp,
        )
        .unwrap();
        let provider = crate::local_files::tests::MockProvider::empty_at(
            "/generated-dir/sub/generated-中文.txt",
        );
        let bridge = LocalFileBridge::open_bound(
            Database::new(private.join("native.sqlite3")),
            &cache,
            Backend::new(provider.clone()),
            "generated-new-native-file",
        )
        .unwrap();
        let mut coordinator = NativeCoordinator::create_new(
            bridge,
            &plan,
            &root,
            b"twodrive-dev-native-v1:generated-office-only",
            &checkpoint,
        )
        .unwrap();
        let relative = Path::new("generated-dir/sub/generated-中文.txt");
        let mut path = coordinator.root().join(relative);
        let bytes = b"generated new file only, no original cloud bytes";
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let artifacts = [
            "~$generated.xlsx",
            "generated-save.tmp",
            "desktop.ini",
            "Thumbs.db",
        ];
        for name in artifacts {
            std::fs::write(
                coordinator.root().join(name),
                b"generated local artifact only",
            )
            .unwrap();
        }
        let progress = coordinator.synchronize_once().unwrap();
        assert_eq!(progress.created, 3, "{progress:?}");
        assert_eq!(progress.deferred, 0, "{progress:?}");
        assert!(!progress.needs_namespace, "{progress:?}");
        for name in artifacts {
            let inventory = coordinator.owner.inventory().unwrap();
            assert!(inventory.iter().any(|entry|entry.relative_path==Path::new(name) && entry.identity.is_none()));
        }
        let idle = coordinator.synchronize_once().unwrap();
        assert!(
            idle.idle,
            "stable acknowledged watcher must become idle: {idle:?}"
        );
        use std::os::windows::process::CommandExt;
        for action in ["directory", "file"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "native_coordinator::tests::namespace_rename_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("TWODRIVE_NAMESPACE_CHILD_ROOT", coordinator.root())
                .env("TWODRIVE_NAMESPACE_CHILD_ACTION", action)
                .creation_flags(0x08000000)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let progress = coordinator.synchronize_once().unwrap();
            assert_eq!(progress.renamed, 1, "{progress:?}");
            assert_eq!(progress.deferred, 0, "{progress:?}");
            assert!(!progress.needs_namespace, "{progress:?}");
        }
        path = coordinator.root().join("renamed-dir/sub/renamed.txt");
        let projections = coordinator
            .bridge
            .journal()
            .native_projections(coordinator.binding().unwrap())
            .unwrap();
        assert_eq!(projections.len(), 3);
        let projection = projections.iter().find(|p| !p.directory).unwrap();
        assert!(projection.local_id.starts_with("local-upload-"));
        assert_eq!(projection.tag, projection.local_id.as_bytes());
        assert_eq!(provider.download(&projection.cloud_id).unwrap(), bytes);
        let generation = coordinator
            .bridge
            .status(&projection.local_id)
            .unwrap()
            .generation;
        coordinator
            .bridge
            .journal()
            .with_clean_cache(&projection.local_id, generation, |cache, expected| {
                ensure!(
                    crate::local_files::digest_file(cache)? == expected,
                    "generated bytes changed"
                );
                twodrive_local_files::native::with_exclusive_placeholder(
                    &path,
                    &projection.tag,
                    projection.size as usize,
                    |locked| locked.dehydrate(),
                )
                .map_err(anyhow::Error::msg)
            })
            .unwrap()
            .expect("clean new file snapshot");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        // A request without completion may be a failed delete. Its no-op
        // decision requires a real namespace lease, not a scan alone.
        use std::os::windows::fs::OpenOptionsExt;
        let pending = std::fs::OpenOptions::new()
            .read(true)
            .access_mode(0x10080)
            .share_mode(7)
            .custom_flags(0x00300000)
            .open(&path)
            .unwrap();
        let request = twodrive_local_files::native::persistent::NativeEvent {
            schema_version: 1,
            kind: twodrive_local_files::native::persistent::NativeEventKind::DeleteRequested,
            volume_serial: coordinator.receipt().volume_serial,
            root_file_id: coordinator.receipt().file_id,
            file_id: u64::from_str_radix(projection.native_file_id.as_deref().unwrap(), 16)
                .unwrap(),
            identity: projection.tag.clone(),
            path: "generated request history".into(),
            related_path: None,
            flags: 0,
            connection_key: 0,
            request_key: 2,
        };
        coordinator
            .bridge
            .journal()
            .database()
            .append_native_event(
                coordinator.binding().unwrap(),
                &serde_json::to_string(&request).unwrap(),
            )
            .unwrap();
        assert!(coordinator.synchronize_once().unwrap().needs_namespace);
        assert_eq!(
            coordinator
                .bridge
                .journal()
                .database()
                .pending_native_events(coordinator.binding().unwrap(), 10)
                .unwrap()
                .len(),
            1
        );
        drop(pending);
        let no_op = coordinator.synchronize_once().unwrap();
        assert!(!no_op.needs_namespace, "{no_op:?}");
        assert!(
            coordinator
                .bridge
                .journal()
                .database()
                .pending_native_events(coordinator.binding().unwrap(), 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            provider
                .get_metadata(&projection.cloud_id)
                .unwrap()
                .unwrap()
                .etag,
            projection.etag
        );
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_coordinator::tests::namespace_rename_child",
                "--ignored",
                "--nocapture",
            ])
            .env("TWODRIVE_NAMESPACE_CHILD_ROOT", coordinator.root())
            .env(
                "TWODRIVE_NAMESPACE_CHILD_ACTION",
                if interrupt_delete {
                    "rename_then_delete"
                } else {
                    "delete"
                },
            )
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(output.status.success(), "generated deletion child failed");
        let mut raw_after_prepare = 0;
        if interrupt_delete {
            deletions::INTERRUPT_AFTER_DELETE_PREPARE.with(|flag| flag.set(true));
            assert!(coordinator.synchronize_once().is_err());
            raw_after_prepare = coordinator
                .bridge
                .journal()
                .database()
                .pending_native_events(coordinator.binding().unwrap(), 1000)
                .unwrap()
                .len();
            let receipt = coordinator.receipt().clone();
            coordinator.disconnect().unwrap();
            drop(coordinator);
            let bridge = LocalFileBridge::open_bound(
                Database::new(private.join("native.sqlite3")),
                &cache,
                Backend::new(provider.clone()),
                "generated-new-native-file",
            )
            .unwrap();
            coordinator = NativeCoordinator::resume(bridge, &root, &receipt).unwrap();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        let deleted = loop {
            let progress = coordinator.synchronize_once().unwrap();
            if progress.deleted == 1 || std::time::Instant::now() >= deadline {
                break progress;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let subsequent = coordinator.synchronize_once().unwrap();
        let raw_remaining = coordinator
            .bridge
            .journal()
            .database()
            .pending_native_events(coordinator.binding().unwrap(), 1000)
            .unwrap()
            .len();
        let archive_count = coordinator
            .bridge
            .journal()
            .native_deleted_archive_count()
            .unwrap();
        let remote_missing = provider
            .get_metadata(&projection.cloud_id)
            .unwrap()
            .is_none();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_coordinator::tests::namespace_rename_child",
                "--ignored",
                "--nocapture",
            ])
            .env("TWODRIVE_NAMESPACE_CHILD_ROOT", coordinator.root())
            .env("TWODRIVE_NAMESPACE_CHILD_ACTION", "delete_dirs")
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "generated directory deletion child failed"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
        let mut directory_deletes = 0;
        loop {
            let progress = coordinator.synchronize_once().unwrap();
            directory_deletes += progress.deleted;
            if directory_deletes == 2 || std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        for name in artifacts {
            let path = coordinator.root().join(name);
            assert_eq!(
                std::fs::read(&path).unwrap(),
                b"generated local artifact only"
            );
            std::fs::remove_file(path).unwrap();
        }
        coordinator.retire_empty_generated_fixture().unwrap();
        drop(coordinator);
        std::fs::remove_dir(root).unwrap();
        std::fs::remove_file(checkpoint).unwrap();
        state.close().unwrap();
        assert_eq!(deleted.deleted, 1, "{deleted:?}");
        assert_eq!(deleted.deferred, 0, "{deleted:?}");
        assert!(remote_missing);
        assert_eq!(archive_count, 1);
        assert_eq!(directory_deletes, 2, "directory consumer did not finish");
        assert_eq!(raw_after_prepare, 0, "prepare transaction left raw events");
        assert_eq!(raw_remaining, 0, "deletion left covered history");
        assert!(!subsequent.needs_namespace, "{subsequent:?}");
    }
    #[test]
    #[ignore = "child helper for explicit generated namespace tests"]
    fn namespace_rename_child() {
        let root = PathBuf::from(
            std::env::var_os("TWODRIVE_NAMESPACE_CHILD_ROOT").expect("generated child root"),
        );
        assert!(
            root.is_absolute()
                && root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("twodrive-cfapi-fixture-office-")
        );
        match std::env::var("TWODRIVE_NAMESPACE_CHILD_ACTION")
            .unwrap()
            .as_str()
        {
            "directory" => {
                std::fs::rename(root.join("generated-dir"), root.join("renamed-dir")).unwrap()
            }
            "delete" => std::fs::remove_file(root.join("renamed-dir/sub/renamed.txt")).unwrap(),
            "delete_dirs" => {
                std::fs::remove_dir(root.join("renamed-dir/sub")).unwrap();
                std::fs::remove_dir(root.join("renamed-dir")).unwrap();
            }
            "rename_then_delete" => {
                let from = root.join("renamed-dir/sub/renamed.txt");
                let to = root.join("renamed-dir/sub/deleted-after-rename.txt");
                std::fs::rename(from, &to).unwrap();
                std::fs::remove_file(to).unwrap();
            }
            "file" => std::fs::rename(
                root.join("renamed-dir/sub/generated-中文.txt"),
                root.join("renamed-dir/sub/renamed.txt"),
            )
            .unwrap(),
            _ => panic!("unknown generated action"),
        }
    }
    #[test]
    #[ignore = "child helper for the explicit generated replacement test"]
    fn atomic_replacement_child() {
        let root = PathBuf::from(
            std::env::var_os("TWODRIVE_ATOMIC_CHILD_ROOT").expect("generated child root"),
        );
        assert!(
            root.is_absolute()
                && root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("twodrive-cfapi-fixture-office-")
        );
        let leaf = std::env::var_os("TWODRIVE_ATOMIC_CHILD_LEAF").expect("generated leaf");
        assert_eq!(Path::new(&leaf).components().count(), 1);
        let temporary = root.join("generated-save.tmp");
        std::fs::write(
            &temporary,
            b"generated atomic replacement with a new physical identity",
        )
        .unwrap();
        if std::env::var("TWODRIVE_ATOMIC_CHILD_RENAME_BACKUP").as_deref() == Ok("1") {
            std::fs::rename(root.join(&leaf), root.join("generated-backup.wbk")).unwrap();
            std::fs::rename(temporary, root.join(&leaf)).unwrap();
        } else if std::env::var_os("TWODRIVE_ATOMIC_CHILD_REPLACEFILE").is_some() {
            use std::os::windows::ffi::OsStrExt;
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn ReplaceFileW(
                    target: *const u16,
                    replacement: *const u16,
                    backup: *const u16,
                    flags: u32,
                    exclude: *mut core::ffi::c_void,
                    reserved: *mut core::ffi::c_void,
                ) -> i32;
            }
            let wide = |path: PathBuf| {
                path.as_os_str()
                    .encode_wide()
                    .chain(Some(0))
                    .collect::<Vec<_>>()
            };
            let target = wide(root.join(leaf));
            let replacement = wide(temporary);
            let backup = wide(root.join("generated-backup.wbk"));
            assert_ne!(
                unsafe {
                    ReplaceFileW(
                        target.as_ptr(),
                        replacement.as_ptr(),
                        backup.as_ptr(),
                        0,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                },
                0,
                "{}",
                std::io::Error::last_os_error()
            );
        } else {
            std::fs::rename(temporary, root.join(leaf)).unwrap();
        }
    }
    #[test]
    #[ignore = "explicit NEW generated root; inspect actual ReplaceFileW target/backup identities"]
    fn replacefile_backup_identity_probe() {
        replacefile_backup_case(0);
    }
    #[test]
    #[ignore = "explicit NEW generated root; restart after backup revert before durable completion"]
    fn replacefile_backup_restarts_after_native_revert() {
        replacefile_backup_case(1);
    }
    #[test]
    #[ignore = "explicit NEW generated root; external rename backup, completion/ACK transaction and restart"]
    fn backup_completion_restart_consumes_only_retained_history() {
        replacefile_backup_case(2);
    }
    #[test]
    #[ignore = "explicit NEW generated root; reverted backup removed before restart"]
    fn reverted_backup_removed_before_restart_keeps_target_progress() {
        replacefile_backup_case(3);
    }
    #[test]
    #[ignore = "explicit NEW generated root; reverted backup moved outside before restart"]
    fn reverted_backup_moved_out_before_restart_keeps_target_progress() {
        replacefile_backup_case(4);
    }
    fn replacefile_backup_case(interrupt: u8) {
        let root = PathBuf::from(
            std::env::var_os("TWODRIVE_APPROVED_REPLACEFILE_ROOT")
                .expect("explicit generated root"),
        );
        assert!(
            root.is_absolute()
                && !root.exists()
                && root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("twodrive-cfapi-fixture-office-")
        );
        let state = tempfile::tempdir().unwrap();
        let private = state.path().canonicalize().unwrap();
        let cache = private.join("cache");
        std::fs::create_dir(&cache).unwrap();
        let provider = crate::local_files::tests::MockProvider::new().with_auxiliary_files();
        let metadata = provider.list_all().unwrap().remove(0);
        let plan = NamespacePlan::build(
            &[
                IndexedItem {
                    id: "root".into(),
                    parent: None,
                    name: "root".into(),
                    kind: "folder".into(),
                    size: 0,
                    etag: "v1".into(),
                    modified: None,
                    deleted: false,
                },
                IndexedItem {
                    id: metadata.remote_id.clone(),
                    parent: Some("root".into()),
                    name: metadata.name.clone(),
                    kind: "file".into(),
                    size: metadata.size,
                    etag: metadata.etag.clone(),
                    modified: Some("1970-01-01T00:00:01Z".into()),
                    deleted: false,
                },
            ],
            "root",
            100,
            twodrive_local_files::names::ordinal_ignorecase_cmp,
        )
        .unwrap();
        let bridge = LocalFileBridge::open_bound(
            Database::new(private.join("native.sqlite3")),
            &cache,
            Backend::new(provider.clone()),
            "generated-replacefile",
        )
        .unwrap();
        let checkpoint = root.with_extension("receipt.json");
        let mut coordinator = NativeCoordinator::create_new(
            bridge,
            &plan,
            &root,
            b"twodrive-dev-native-v1:generated-office-only",
            &checkpoint,
        )
        .unwrap();
        let path = coordinator.root().join(&metadata.name);
        let original = std::fs::read(&path).unwrap();
        let published = coordinator
            .bridge
            .journal()
            .native_projection_for_id(&metadata.remote_id)
            .unwrap()
            .unwrap();
        use std::os::windows::process::CommandExt;
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_coordinator::tests::atomic_replacement_child",
                "--ignored",
                "--nocapture",
            ])
            .env("TWODRIVE_ATOMIC_CHILD_ROOT", coordinator.root())
            .env("TWODRIVE_ATOMIC_CHILD_LEAF", &metadata.name)
            .env("TWODRIVE_ATOMIC_CHILD_REPLACEFILE", "1")
            .env(
                "TWODRIVE_ATOMIC_CHILD_RENAME_BACKUP",
                if interrupt == 2 { "1" } else { "" },
            )
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{} {}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        let entries = coordinator.owner.inventory().unwrap();
        for entry in &entries {
            println!(
                "generated ReplaceFileW: target={} backup={} old_physical={} original_tag={} ordinary={}",
                entry.relative_path == Path::new(&metadata.name),
                entry.relative_path == Path::new("generated-backup.wbk"),
                published.native_file_id.as_deref()
                    == Some(format!("{:016x}", entry.file_id).as_str()),
                entry.identity.as_deref() == Some(published.tag.as_slice()),
                entry.identity.is_none()
            );
        }
        if interrupt == 2 {
            renames::INTERRUPT_BACKUP_AFTER_COMPLETE.with(|flag| flag.set(true));
        } else if interrupt > 0 {
            crate::local_files::INTERRUPT_BACKUP_AFTER_REVERT.with(|flag| flag.set(true));
        }
        let mut coordination = coordinator.synchronize_once();
        let archived_backup = coordinator
            .bridge
            .journal()
            .native_backup_copies(coordinator.binding().unwrap())
            .unwrap()
            .first()
            .map(|c| c.cache_path.clone());
        if interrupt > 0 {
            assert!(coordination.is_err());
            let backup_path = coordinator.root().join("generated-backup.wbk");
            if interrupt == 3 {
                std::fs::remove_file(&backup_path).unwrap();
            }
            if interrupt == 4 {
                std::fs::rename(&backup_path, private.join("moved-backup.wbk")).unwrap();
            }
            let receipt = coordinator.receipt().clone();
            coordinator.disconnect().unwrap();
            drop(coordinator);
            let bridge = LocalFileBridge::open_bound(
                Database::new(private.join("native.sqlite3")),
                &cache,
                Backend::new(provider.clone()),
                "generated-replacefile",
            )
            .unwrap();
            coordinator = NativeCoordinator::resume(bridge, &root, &receipt).unwrap();
            if interrupt >= 3 {
                std::fs::write(
                    coordinator.root().join("unrelated.txt"),
                    b"generated unrelated progress",
                )
                .unwrap();
            }
            coordination = coordinator.synchronize_once();
        }
        let current_bytes = provider.download(&metadata.remote_id).unwrap();
        let backup_remote = provider
            .get_metadata_by_path("/generated-backup.wbk")
            .unwrap();
        let backup_cloud_bytes = backup_remote
            .as_ref()
            .map(|remote| provider.download(&remote.remote_id).unwrap());
        let cloud = provider.get_metadata(&metadata.remote_id).unwrap().unwrap();
        let target = std::fs::read(&path);
        let backup = std::fs::read(coordinator.root().join("generated-backup.wbk"));
        let retained_backup = coordinator
            .bridge
            .journal()
            .database()
            .get_by_remote_id(&metadata.remote_id)
            .unwrap()
            .unwrap()
            .cache_path
            .clone()
            .map(|p| std::fs::read(p).unwrap());
        let old_backup_retained = archived_backup.map(|p| std::fs::read(p).unwrap());
        let unrelated = provider
            .get_metadata_by_path("/unrelated.txt")
            .unwrap()
            .map(|m| provider.download(&m.remote_id).unwrap());
        let copies = coordinator
            .bridge
            .journal()
            .native_backup_copies(coordinator.binding().unwrap())
            .unwrap();
        let namespace = coordinator
            .bridge
            .journal()
            .database()
            .pending_native_events(coordinator.binding().unwrap(), 1000)
            .unwrap();
        for entry in coordinator.owner.inventory().unwrap() {
            std::fs::remove_file(coordinator.root().join(entry.relative_path)).unwrap();
        }
        coordinator.retire_empty_generated_fixture().unwrap();
        drop(coordinator);
        std::fs::remove_dir(root).unwrap();
        std::fs::remove_file(checkpoint).unwrap();
        state.close().unwrap();
        assert_eq!(
            target.unwrap(),
            b"generated atomic replacement with a new physical identity"
        );
        if interrupt < 3 {
            assert_eq!(backup.unwrap(), original);
            assert_ne!(backup_remote.unwrap().remote_id, metadata.remote_id);
            assert_eq!(backup_cloud_bytes.unwrap(), original);
        } else {
            assert!(backup.is_err() && backup_remote.is_none());
            assert_eq!(old_backup_retained.unwrap(), original);
            assert_eq!(unrelated.unwrap(), b"generated unrelated progress");
            assert_eq!(
                retained_backup.unwrap(),
                b"generated atomic replacement with a new physical identity"
            );
        }
        assert!(
            copies.is_empty() && namespace.is_empty(),
            "{coordination:?}"
        );
        assert_eq!(
            current_bytes, b"generated atomic replacement with a new physical identity",
            "{coordination:?}"
        );
        assert_eq!(
            cloud.path, metadata.path,
            "retained backup changed the logical cloud name; coordination={coordination:?}"
        );
    }
    #[test]
    #[ignore = "explicit NEW generated CFAPI root; projected in-place edit, upload and updated native acknowledgement"]
    fn projected_edit_advances_its_version_under_the_retained_native_lease() {
        projected_replacement_recovery(false);
    }
    #[test]
    #[ignore = "explicit NEW generated CFAPI root; replacement lost reply, restart and conflicting remote bytes"]
    fn projected_replacement_conflict_survives_restart_without_more_snapshots() {
        projected_replacement_recovery(true);
    }
    fn projected_replacement_recovery(conflict: bool) {
        use std::io::Write;
        let root = PathBuf::from(
            std::env::var_os("TWODRIVE_APPROVED_PROJECTED_EDIT_ROOT")
                .expect("explicit new generated root"),
        );
        assert!(
            root.is_absolute()
                && !root.exists()
                && root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("twodrive-cfapi-fixture-office-")
        );
        let checkpoint = root.with_extension("receipt.json");
        let state = tempfile::tempdir().unwrap();
        let private = state.path().canonicalize().unwrap();
        let cache = private.join("cache");
        std::fs::create_dir(&cache).unwrap();
        let provider = crate::local_files::tests::MockProvider::new();
        let metadata = provider.list_all().unwrap().remove(0);
        let entries = vec![
            IndexedItem {
                id: "root".into(),
                parent: None,
                name: "root".into(),
                kind: "folder".into(),
                size: 0,
                etag: "v1".into(),
                modified: None,
                deleted: false,
            },
            IndexedItem {
                id: metadata.remote_id.clone(),
                parent: Some("root".into()),
                name: metadata.name.clone(),
                kind: "file".into(),
                size: metadata.size,
                etag: metadata.etag.clone(),
                modified: Some("1970-01-01T00:00:01Z".into()),
                deleted: false,
            },
        ];
        let plan = NamespacePlan::build(
            &entries,
            "root",
            100,
            twodrive_local_files::names::ordinal_ignorecase_cmp,
        )
        .unwrap();
        let db = Database::new(private.join("native.sqlite3"));
        let bridge = LocalFileBridge::open_bound(
            db,
            &cache,
            Backend::new(provider.clone()),
            "generated-projected-edit",
        )
        .unwrap();
        let mut coordinator = NativeCoordinator::create_new(
            bridge,
            &plan,
            &root,
            b"twodrive-dev-native-v1:generated-office-only",
            &checkpoint,
        )
        .unwrap();
        let file = coordinator.root().join(&metadata.name);
        std::fs::read(&file).unwrap();
        let edited = b"generated projected content with a different length and preserved timestamp";
        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&file)
            .unwrap();
        writer.write_all(edited).unwrap();
        writer.sync_all().unwrap();
        drop(writer);
        let progress = coordinator.synchronize_known_files_once().unwrap();
        if progress.uploaded != 1 || progress.deferred != 0 || progress.needs_namespace {
            std::fs::remove_file(&file).unwrap();
            coordinator.retire_empty_generated_fixture().unwrap();
            drop(coordinator);
            std::fs::remove_dir(root).unwrap();
            std::fs::remove_file(checkpoint).unwrap();
            state.close().unwrap();
            panic!("generated initial in-place worker failed: {progress:?}");
        }
        assert_eq!(progress.uploaded, 1, "{progress:?}");
        assert_eq!(progress.deferred, 0);
        assert!(!progress.needs_namespace);
        let projection = coordinator
            .bridge
            .journal()
            .native_projection_for_id(&metadata.remote_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            projection.etag,
            provider
                .get_metadata(&metadata.remote_id)
                .unwrap()
                .unwrap()
                .etag
        );
        assert_eq!(projection.size, edited.len() as u64);
        assert_eq!(provider.download(&metadata.remote_id).unwrap(), edited);
        let generation = coordinator
            .bridge
            .status(&metadata.remote_id)
            .unwrap()
            .generation;
        coordinator
            .bridge
            .journal()
            .with_clean_cache(&metadata.remote_id, generation, |cache, expected| {
                ensure!(
                    crate::local_files::digest_file(cache)? == expected,
                    "generated private bytes changed"
                );
                twodrive_local_files::native::with_exclusive_placeholder(
                    &file,
                    &projection.tag,
                    projection.size as usize,
                    |locked| locked.dehydrate(),
                )
                .map_err(anyhow::Error::msg)
            })
            .unwrap()
            .expect("clean generated source");
        assert_eq!(std::fs::read(&file).unwrap(), edited);
        let atomic = b"generated atomic replacement with a new physical identity";
        use std::os::windows::process::CommandExt;
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_coordinator::tests::atomic_replacement_child",
                "--ignored",
                "--nocapture",
            ])
            .env("TWODRIVE_ATOMIC_CHILD_ROOT", coordinator.root())
            .env("TWODRIVE_ATOMIC_CHILD_LEAF", &metadata.name)
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(child.status.success(), "generated child replacement failed");
        provider.lost_reply(true);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let replacement = loop {
            let progress = coordinator.synchronize_once().unwrap();
            if progress.deferred == 1 || std::time::Instant::now() >= deadline {
                break progress;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(
            replacement.deferred, 1,
            "lost reply was not injected: {replacement:?}"
        );
        assert!(
            coordinator
                .bridge
                .status(&metadata.remote_id)
                .unwrap()
                .uncertain_upload
        );
        let retained_generation = coordinator
            .bridge
            .status(&metadata.remote_id)
            .unwrap()
            .generation;
        let snapshots_before = std::fs::read_dir(&cache).unwrap().count();
        let receipt = coordinator.receipt().clone();
        coordinator.disconnect().unwrap();
        drop(coordinator);
        provider.lost_reply(false);
        if conflict {
            provider.change(b"generated concurrent remote version");
        }
        let bridge = LocalFileBridge::open_bound(
            Database::new(private.join("native.sqlite3")),
            &cache,
            Backend::new(provider.clone()),
            "generated-projected-edit",
        )
        .unwrap();
        let mut coordinator = NativeCoordinator::resume(bridge, &root, &receipt).unwrap();
        let recovery = coordinator.synchronize_once().unwrap();
        let mut retry_states = Vec::new();
        for _ in 0..2 {
            retry_states.push(coordinator.synchronize_once().unwrap());
        }
        let replaced = coordinator
            .bridge
            .journal()
            .native_projection_for_id(&metadata.remote_id)
            .unwrap()
            .unwrap();
        let recovered_state = coordinator.bridge.status(&metadata.remote_id).unwrap();
        let snapshots_after = std::fs::read_dir(&cache).unwrap().count();
        let local_bytes = std::fs::read(&file).unwrap();
        let remote_bytes = provider.download(&metadata.remote_id).unwrap();
        let captured = coordinator
            .bridge
            .journal()
            .native_capture_binding(&metadata.remote_id)
            .unwrap();
        std::fs::remove_file(file).unwrap();
        let source_probe = Arc::downgrade(&coordinator._source);
        coordinator.retire_empty_generated_fixture().unwrap();
        drop(coordinator);
        assert_eq!(
            source_probe.strong_count(),
            0,
            "native callback source retained after disconnect"
        );
        std::fs::remove_dir(root).unwrap();
        std::fs::remove_file(checkpoint).unwrap();
        let ownership = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(private.join("native.local-cache.lock"))
            .unwrap();
        ownership
            .try_lock()
            .expect("native cache owner remained alive after coordinator drop");
        drop(ownership);
        state.close().unwrap();
        assert_eq!(local_bytes, atomic);
        assert_eq!(
            snapshots_after, snapshots_before,
            "retries must not keep extra full captures"
        );
        assert_eq!(
            recovered_state.generation, retained_generation,
            "receipt recovery must use the retained snapshot"
        );
        assert!(!recovered_state.uncertain_upload, "recovery={recovery:?}");
        assert_eq!(recovery.deferred, 0, "{recovery:?}");
        if conflict {
            assert_eq!(recovery.conflicts, 1, "{recovery:?}");
            assert!(
                retry_states
                    .iter()
                    .all(|p| p.conflicts == 1 && p.deferred == 0)
            );
            assert_eq!(recovered_state.state, twodrive_core::FileState::Conflict);
            assert_eq!(remote_bytes, b"generated concurrent remote version");
            assert_eq!(replaced.native_file_id, projection.native_file_id);
            assert!(captured.is_some());
        } else {
            assert_eq!(recovery.uploaded, 1, "{recovery:?}");
            assert!(recovered_state.sync_confirmed());
            assert_eq!(remote_bytes, atomic);
            assert_ne!(replaced.native_file_id, projection.native_file_id);
            assert!(captured.is_none());
        }
    }
    #[test]
    #[ignore = "explicit NEW generated CFAPI root; whole nested projection, directory watcher and resumed source"]
    fn coordinator_publishes_a_parent_first_tree_and_resumes_its_physical_bindings() {
        let root = PathBuf::from(
            std::env::var_os("TWODRIVE_APPROVED_COORDINATOR_ROOT")
                .expect("explicit new generated root"),
        );
        assert!(
            root.is_absolute()
                && !root.exists()
                && root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("twodrive-cfapi-fixture-office-")
        );
        let checkpoint = root.with_extension("receipt.json");
        let state = tempfile::tempdir().unwrap();
        let private = state.path().canonicalize().unwrap();
        let cache = private.join("cache");
        std::fs::create_dir(&cache).unwrap();
        let dataset = Dataset::VisualV1;
        let backend = Backend::new(GeneratedProvider {
            dataset,
            counters: Arc::new(DownloadCounters::default()),
        });
        let metadata = backend.list_all().unwrap();
        let root_id = "generated-drive-root";
        let mut entries = vec![IndexedItem {
            id: root_id.into(),
            parent: None,
            name: "root".into(),
            kind: "folder".into(),
            size: 0,
            etag: "root-v1".into(),
            modified: None,
            deleted: false,
        }];
        for item in &metadata {
            let parent = if item.parent_path == "/" {
                root_id
            } else {
                &metadata
                    .iter()
                    .find(|parent| parent.path == item.parent_path)
                    .unwrap()
                    .remote_id
            };
            entries.push(IndexedItem {
                id: item.remote_id.clone(),
                parent: Some(parent.into()),
                name: item.name.clone(),
                kind: if item.is_dir { "folder" } else { "file" }.into(),
                size: item.size,
                etag: item.etag.clone(),
                modified: Some("1970-01-01T00:00:01Z".into()),
                deleted: false,
            });
        }
        let plan = NamespacePlan::build(
            &entries,
            root_id,
            100,
            twodrive_local_files::names::ordinal_ignorecase_cmp,
        )
        .unwrap();
        let db = Database::new(private.join("native.sqlite3"));
        let bridge = LocalFileBridge::open_bound(
            db.clone(),
            &cache,
            backend,
            "onedrive:generated-owner:generated-drive",
        )
        .unwrap();
        let mut coordinator = NativeCoordinator::create_new(
            bridge,
            &plan,
            &root,
            b"twodrive-dev-native-v1:generated-office-only",
            &checkpoint,
        )
        .unwrap();
        let receipt = coordinator.receipt().clone();
        let files = dataset.files();
        assert_eq!(
            std::fs::read(coordinator.root().join(&files[0].relative_path)).unwrap(),
            dataset.bytes(0)
        );
        coordinator.disconnect().unwrap();
        drop(coordinator);
        let bridge = LocalFileBridge::open_bound(
            db,
            &cache,
            Backend::new(GeneratedProvider {
                dataset,
                counters: Arc::new(DownloadCounters::default()),
            }),
            "onedrive:generated-owner:generated-drive",
        )
        .unwrap();
        let mut coordinator = NativeCoordinator::resume(bridge, &root, &receipt).unwrap();
        assert_eq!(
            std::fs::read(coordinator.root().join(&files[1].relative_path)).unwrap(),
            dataset.bytes(1)
        );
        coordinator.verify_bootstrap_inventory().unwrap();
        let projection = coordinator
            .bridge
            .journal()
            .native_projection_for_id(&files[0].id)
            .unwrap()
            .unwrap();
        let path = coordinator.root().join(&files[0].relative_path);
        let generation = coordinator.bridge.status(&files[0].id).unwrap().generation;
        coordinator
            .bridge
            .journal()
            .with_clean_cache(&files[0].id, generation, |cache, expected| {
                ensure!(
                    crate::local_files::digest_file(cache)? == expected,
                    "generated cache changed"
                );
                twodrive_local_files::native::with_exclusive_placeholder(
                    &path,
                    &projection.tag,
                    projection.size as usize,
                    |locked| locked.dehydrate(),
                )
                .map_err(anyhow::Error::msg)
            })
            .unwrap()
            .expect("clean generated source lease");
        let missing_account = private.join("missing-account");
        let configuration = Configuration {
            schema_version: 1,
            account: missing_account.clone(),
            identity: CloudIdentity {
                account: "generated-owner".into(),
                drive: "generated-drive".into(),
            },
            root: coordinator.root().to_path_buf(),
            registration: receipt.identity.clone(),
            receipt: None,
        };
        std::fs::copy(&checkpoint, private.join("root-receipt.json")).unwrap();
        NativeSession::seal(&private, &configuration).unwrap();
        coordinator.disconnect().unwrap();
        drop(coordinator);
        let mut session = NativeSession::resume(&private).unwrap();
        assert_eq!(
            std::fs::read(session.coordinator.root().join(&files[0].relative_path)).unwrap(),
            dataset.bytes(0)
        );
        assert!(
            !missing_account.exists(),
            "cached source consulted missing credentials"
        );
        assert!(
            std::fs::read(session.coordinator.root().join(&files[2].relative_path)).is_err(),
            "uncached source bypassed account verification"
        );
        let coordinator = &mut session.coordinator;
        for file in files {
            std::fs::remove_file(coordinator.root().join(file.relative_path)).unwrap();
        }
        for directory in dataset.directories().into_iter().rev() {
            std::fs::remove_dir(coordinator.root().join(directory.relative_path)).unwrap();
        }
        coordinator.retire_empty_generated_fixture().unwrap();
        drop(session);
        std::fs::remove_dir(root).unwrap();
        std::fs::remove_file(checkpoint).unwrap();
        state.close().unwrap();
    }
}
