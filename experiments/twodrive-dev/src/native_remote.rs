use super::*;
use crate::model::CloudBackend;

#[derive(Default, Debug, Serialize)]
pub struct RemoteProgress {
    pub created: usize,
    pub updated: usize,
    pub moved: usize,
    pub removed: usize,
    pub deferred: usize,
    pub error: Option<String>,
}
impl NativeSession {
    /// Refresh account metadata independently of native/data mutation phases.
    pub fn refresh_remote(&mut self) -> anyhow::Result<RemoteProgress> {
        let provider = crate::native_account::NativeAccount::new(
            self.configuration.account.clone(),
            self.configuration.identity.clone(),
            true,
        )?;
        let provider = provider.verified()?;
        let db = self.coordinator.bridge.journal().database();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        let mut check = || {
            ensure!(
                std::time::Instant::now() < deadline,
                "native delta refresh deadline reached"
            );
            Ok(())
        };
        let (cursor, replace, _) =
            provider
                .graph
                .stage_delta(db, &self.configuration.identity, &mut check)?;
        ensure!(
            provider.graph.readonly_identity(&mut check)? == self.configuration.identity,
            "native account changed during refresh"
        );
        db.cloud_commit(&self.configuration.identity, &cursor, replace)?;
        use std::os::windows::ffi::OsStrExt;
        let root = db
            .get_state_value("native_cloud_root_id")?
            .context("native cloud root missing")?;
        let plan = NamespacePlan::build(
            &db.cloud_items(&self.configuration.identity)?,
            &root,
            self.coordinator.root().as_os_str().encode_wide().count(),
            twodrive_local_files::names::ordinal_ignorecase_cmp,
        )
        .map_err(anyhow::Error::msg)?;
        let metadata = plan
            .entries
            .iter()
            .map(|entry| {
                let modified = time::OffsetDateTime::parse(
                    entry
                        .item
                        .modified
                        .as_deref()
                        .context("remote timestamp missing")?,
                    &time::format_description::well_known::Rfc3339,
                )?
                .unix_timestamp();
                Ok(if entry.item.kind == "folder" {
                    MetadataEntry::new_dir(
                        &entry.item.id,
                        entry.cloud_path(),
                        modified,
                        &entry.item.etag,
                    )
                } else {
                    MetadataEntry::new_file(
                        &entry.item.id,
                        entry.cloud_path(),
                        entry.item.size,
                        modified,
                        &entry.item.etag,
                    )
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut progress = self.coordinator.apply_remote_metadata(&metadata)?;
        let ids = metadata
            .iter()
            .map(|entry| entry.remote_id.clone())
            .collect::<HashSet<_>>();
        let removed = self.coordinator.apply_remote_removals(&ids)?;
        progress.removed += removed.removed;
        progress.deferred += removed.deferred;
        if removed.error.is_some() {
            progress.error = removed.error;
        }
        Ok(progress)
    }
}
impl NativeCoordinator {
    /// Reconcile omissions from a complete account snapshot. Every new removal
    /// also requires a fresh bound-ID lookup proving the service object absent.
    pub fn apply_remote_removals(
        &mut self,
        remaining: &HashSet<String>,
    ) -> anyhow::Result<RemoteProgress> {
        let binding = self.binding()?.to_string();
        let mut progress = RemoteProgress::default();
        self.last_successful_watch = None;
        for removal in self.bridge.journal().native_remote_removals(&binding)? {
            match self.bridge.apply_native_remote_removal(
                &self.owner,
                &removal.expected,
                Some(removal.clone()),
            ) {
                Ok(()) => progress.removed += 1,
                Err(error) => {
                    progress.deferred += 1;
                    progress.error =
                        Some(twodrive_backend::diagnostics::safe_error(&error).to_string());
                }
            }
        }
        let mut projections = self.bridge.journal().native_projections(&binding)?;
        projections.sort_by_key(|p| std::cmp::Reverse(p.path.matches('/').count()));
        for expected in projections
            .into_iter()
            .filter(|p| p.state == "published" && !remaining.contains(&p.cloud_id))
        {
            let result = (|| -> anyhow::Result<()> {
                ensure!(
                    self.bridge
                        .backend()
                        .get_metadata(&expected.cloud_id)?
                        .is_none(),
                    "catalogue deletion is not confirmed by bound object lookup"
                );
                self.bridge
                    .apply_native_remote_removal(&self.owner, &expected, None)
            })();
            match result {
                Ok(()) => progress.removed += 1,
                Err(error) => {
                    progress.deferred += 1;
                    progress.error =
                        Some(twodrive_backend::diagnostics::safe_error(&error).to_string());
                }
            }
        }
        Ok(progress)
    }
    fn materialize_remote_claim(&mut self, id: &str) -> anyhow::Result<()> {
        let binding = self.binding()?.to_string();
        let existing = self.bridge.journal().native_projection_for_id(id)?;
        let projection = if let Some(existing) = existing {
            ensure!(
                existing.root_binding == binding && existing.state == "preparing",
                "remote create has another publication phase"
            );
            existing
        } else {
            let record = self
                .bridge
                .journal()
                .database()
                .get_by_remote_id(id)?
                .context("remote create record missing")?;
            let relative = Self::relative(&record.metadata.path)?;
            ensure!(
                !self
                    .owner
                    .entry_exists(&relative)
                    .map_err(anyhow::Error::msg)?,
                "remote create preserves unowned data"
            );
            self.bridge.journal().prepare_native_projection(
                id,
                &binding,
                &rand::random::<[u8; 32]>(),
            )?
        };
        let relative = Self::relative(&projection.path)?;
        if !self
            .owner
            .entry_exists(&relative)
            .map_err(anyhow::Error::msg)?
        {
            self.owner
                .publish_placeholder(
                    &relative,
                    &projection.tag,
                    projection.size,
                    projection.directory,
                    projection.modified_unix,
                )
                .map_err(anyhow::Error::msg)?;
        }
        self.owner
            .with_publication_binding(
                &relative,
                &projection.tag,
                projection.size,
                projection.directory,
                |volume, id| {
                    self.bridge
                        .journal()
                        .complete_native_projection(
                            &projection.local_id,
                            &binding,
                            &projection.tag,
                            volume,
                            id,
                        )
                        .map_err(|e| e.to_string())
                },
            )
            .map_err(anyhow::Error::msg)
    }
    fn publish_remote_create(&mut self, entry: &MetadataEntry) -> anyhow::Result<()> {
        let relative = Self::relative(&entry.path)?;
        ensure!(
            !self
                .owner
                .entry_exists(&relative)
                .map_err(anyhow::Error::msg)?,
            "remote create preserves an existing native path"
        );
        let db = self.bridge.journal().database();
        ensure!(
            db.get_by_path(&entry.path)?.is_none(),
            "remote create has a local owner"
        );
        if entry.parent_path != "/" {
            let parent = db
                .get_by_path(&entry.parent_path)?
                .context("remote parent not published")?;
            ensure!(
                parent.metadata.is_dir
                    && self
                        .bridge
                        .journal()
                        .native_projection_for_id(&parent.metadata.remote_id)?
                        .is_some_and(|p| p.state == "published"),
                "remote parent is not ready"
            );
        }
        db.upsert_metadata(entry)?;
        self.materialize_remote_claim(&entry.remote_id)
    }
    pub fn apply_remote_metadata(
        &mut self,
        metadata: &[MetadataEntry],
    ) -> anyhow::Result<RemoteProgress> {
        let mut progress = RemoteProgress::default();
        let binding = self.binding()?.to_string();
        self.last_successful_watch = None;
        for update in self.bridge.journal().native_remote_updates(&binding)? {
            match self.bridge.apply_native_remote_version(
                &self.owner,
                &update.expected,
                &update.next,
                Some(update.clone()),
            ) {
                Ok(()) => progress.updated += 1,
                Err(error) => {
                    progress.deferred += 1;
                    progress.error =
                        Some(twodrive_backend::diagnostics::safe_error(&error).to_string());
                }
            }
        }
        let records = self.bridge.journal().database().all_records()?;
        let by_cloud = records
            .iter()
            .filter_map(|r| r.cloud_remote_id.as_ref().map(|id| (id.as_str(), r)))
            .collect::<HashMap<_, _>>();
        let unchanged = self
            .bridge
            .journal()
            .native_projections(&binding)?
            .into_iter()
            .filter(|p| p.state == "published")
            .map(|p| (p.cloud_id.clone(), p))
            .collect::<HashMap<_, _>>();
        for entry in metadata {
            if unchanged.get(&entry.remote_id).is_some_and(|p| {
                p.directory == entry.is_dir
                    && p.path == entry.path
                    && (entry.is_dir || (p.etag == entry.etag && p.size == entry.size))
            }) {
                continue;
            }
            let result = (|| -> anyhow::Result<()> {
                let Some(record) = by_cloud.get(entry.remote_id.as_str()).copied() else {
                    self.publish_remote_create(entry)?;
                    progress.created += 1;
                    return Ok(());
                };
                let existing = self
                    .bridge
                    .journal()
                    .native_projection_for_id(&record.metadata.remote_id)?;
                if existing.as_ref().is_none_or(|p| p.state == "preparing") {
                    self.materialize_remote_claim(&record.metadata.remote_id)?;
                    progress.created += 1;
                }
                let projection = self
                    .bridge
                    .journal()
                    .native_projection_for_id(&record.metadata.remote_id)?
                    .context("remote publication missing after materialization")?;
                ensure!(
                    projection.directory == entry.is_dir,
                    "remote item changed type"
                );
                if projection.etag == entry.etag
                    && projection.size == entry.size
                    && projection.path == entry.path
                {
                    return Ok(());
                }
                if entry.is_dir && projection.path == entry.path {
                    return Ok(());
                }
                self.bridge
                    .apply_native_remote_version(&self.owner, &projection, entry, None)?;
                if projection.path != entry.path {
                    progress.moved += 1;
                } else {
                    progress.updated += 1;
                }
                Ok(())
            })();
            if let Err(error) = result {
                progress.deferred += 1;
                progress.error =
                    Some(twodrive_backend::diagnostics::safe_error(&error).to_string());
            }
        }
        Ok(progress)
    }
}
