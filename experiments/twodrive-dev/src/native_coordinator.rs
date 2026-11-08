//! Persistent initial native projection. Cloud mutation consumers are separate;
//! publication never infers cloud deletion from a missing local path.
use crate::{local_files::LocalFileBridge, native_source::BoundPublishedSource};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
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
}
#[derive(Default, Serialize, Debug)]
pub struct SyncProgress {
    pub uploaded: usize,
    pub deferred: usize,
    pub conflicts: usize,
    pub needs_namespace: bool,
    pub error: Option<String>,
    pub replacements: usize,
}
impl NativeCoordinator {
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
        let id = if let Some(record) = existing {
            ensure!(
                record.metadata.remote_id.starts_with("local-upload-")
                    && self
                        .bridge
                        .journal()
                        .native_projections(self.binding()?)?
                        .iter()
                        .all(|p| p.local_id != record.metadata.remote_id),
                "new file already has a published owner"
            );
            let claims = self.bridge.journal().native_creations()?;
            ensure!(
                claims
                    .iter()
                    .any(|claim| claim.local_id == record.metadata.remote_id
                        && claim.root_binding == root_receipt
                        && claim.path == cloud_path),
                "native creation claim changed"
            );
            record.metadata.remote_id
        } else {
            let claims = self.bridge.journal().native_creations()?;
            let id = claims
                .iter()
                .find(|claim| {
                    claim.path == cloud_path
                        && claim.root_binding == root_receipt
                        && claim.state == "preparing"
                })
                .map(|claim| claim.local_id.clone())
                .unwrap_or_else(|| format!("local-upload-{:032x}", rand::random::<u128>()));
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
        let binding = self.binding()?.to_string();
        let db = self.bridge.journal().database().clone();
        let projections = self.bridge.journal().native_projections(&binding)?;
        let entries = self.owner.inventory().map_err(anyhow::Error::msg)?;
        let mut candidates = HashMap::new();
        for projection in projections
            .iter()
            .filter(|p| !p.directory && p.state == "published")
        {
            let relative = Self::relative(&projection.path)?;
            let Some(entry) = entries
                .iter()
                .find(|entry| entry.relative_path == relative && !entry.directory)
            else {
                continue;
            };
            let old_is_present = entries.iter().any(|entry| {
                projection.volume_serial == Some(entry.volume_serial)
                    && projection.native_file_id.as_deref()
                        == Some(format!("{:016x}", entry.file_id).as_str())
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
                    ..Default::default()
                });
            };
            if projection.native_file_id.as_deref()
                != Some(format!("{:016x}", event.file_id).as_str())
                && event.file_id != *current_id
            {
                return Ok(SyncProgress {
                    needs_namespace: true,
                    ..Default::default()
                });
            }
        }
        let mut progress = SyncProgress::default();
        for (projection, relative, _) in candidates.values() {
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
        let mut data = self.synchronize_known_files_once()?;
        data.replacements = progress.replacements;
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
        let entries = self.owner.inventory().map_err(anyhow::Error::msg)?;
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
            let captured = self
                .bridge
                .journal()
                .native_capture_binding(&projection.local_id)?;
            let captured_matches = captured.is_some_and(|binding| {
                binding.physical.volume_serial == entry.volume_serial
                    && binding.physical.file_id == entry.file_id
            });
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
        let entries = self.owner.inventory().map_err(anyhow::Error::msg)?;
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
    #[ignore = "explicit NEW generated CFAPI root; new ordinary file through create-only receipt and published binding"]
    fn new_native_file_becomes_a_published_cloud_object_without_an_old_download() {
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
        let provider = crate::local_files::tests::MockProvider::empty();
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
        let relative = Path::new("generated-中文.txt");
        let path = coordinator.root().join(relative);
        let bytes = b"generated new file only, no original cloud bytes";
        std::fs::write(&path, bytes).unwrap();
        assert!(coordinator.synchronize_new_file(relative).unwrap());
        let projections = coordinator
            .bridge
            .journal()
            .native_projections(coordinator.binding().unwrap())
            .unwrap();
        assert_eq!(projections.len(), 1);
        let projection = &projections[0];
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
        std::fs::remove_file(path).unwrap();
        coordinator.retire_empty_generated_fixture().unwrap();
        drop(coordinator);
        std::fs::remove_dir(root).unwrap();
        std::fs::remove_file(checkpoint).unwrap();
        state.close().unwrap();
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
        std::fs::rename(temporary, root.join(leaf)).unwrap();
    }
    #[test]
    #[ignore = "explicit NEW generated CFAPI root; projected in-place edit, upload and updated native acknowledgement"]
    fn projected_edit_advances_its_version_under_the_retained_native_lease() {
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
        assert_eq!(progress.uploaded, 1);
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let replacement = loop {
            let progress = coordinator.synchronize_once().unwrap();
            if progress.uploaded == 1 || std::time::Instant::now() >= deadline {
                break progress;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(replacement.uploaded, 1);
        assert_eq!(replacement.deferred, 0);
        assert!(!replacement.needs_namespace);
        let replaced = coordinator
            .bridge
            .journal()
            .native_projection_for_id(&metadata.remote_id)
            .unwrap()
            .unwrap();
        assert_ne!(replaced.native_file_id, projection.native_file_id);
        assert_eq!(provider.download(&metadata.remote_id).unwrap(), atomic);
        assert!(
            coordinator
                .bridge
                .journal()
                .native_capture_binding(&metadata.remote_id)
                .unwrap()
                .is_none()
        );
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
