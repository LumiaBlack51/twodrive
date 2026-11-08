//! Read-only source preflight precedes any original-folder migration.
use super::*;
use twodrive_backend::onedrive::{IntegrityRequest, QuickXorHash};
use twodrive_local_files::native::persistent::{ReadOnlyRegisteredRoot, personal_source_location};

#[derive(Default, Debug, Serialize)]
pub struct SourceChecksumSummary {
    pub sampled: usize,
    pub verified: usize,
    pub metadata_deferred: usize,
    pub source_deferred: usize,
    pub source_failure_counts: std::collections::BTreeMap<String, usize>,
    pub checksum_mismatches: usize,
    pub bytes_read: u64,
}
struct Candidate {
    entry: twodrive_local_files::native::persistent::NativeEntry,
    request: IntegrityRequest,
}
struct ChecksumSink(QuickXorHash);
impl std::io::Write for ChecksumSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes).map_err(std::io::Error::other)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Default, Debug, Serialize)]
pub struct SourceCopySummary {
    pub candidates: usize,
    pub staged: usize,
    pub already_owned: usize,
    pub bytes_copied: u64,
    pub deferred: usize,
    pub checksum_mismatches: usize,
    pub completed: usize,
    pub isolated: usize,
}
struct SnapshotWriter<'a> {
    file: &'a mut std::fs::File,
    sha: sha2::Sha256,
    quick: QuickXorHash,
}
impl std::io::Write for SnapshotWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        use sha2::Digest;
        let count = self.file.write(bytes)?;
        self.sha.update(&bytes[..count]);
        self.quick
            .update(&bytes[..count])
            .map_err(std::io::Error::other)?;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

fn sorted_records(
    records: Vec<twodrive_core::FileRecord>,
) -> anyhow::Result<Vec<twodrive_core::FileRecord>> {
    let mut records = records;
    let mut error = None;
    records.sort_by(|a, b| {
        match twodrive_local_files::names::ordinal_ignorecase_cmp(
            &a.metadata.path,
            &b.metadata.path,
        ) {
            Ok(order) => order,
            Err(e) => {
                error = Some(e);
                std::cmp::Ordering::Equal
            }
        }
    });
    ensure!(error.is_none(), "source path comparison unavailable");
    for pair in records.windows(2) {
        ensure!(
            twodrive_local_files::names::ordinal_ignorecase_cmp(
                &pair[0].metadata.path,
                &pair[1].metadata.path
            )
            .map_err(anyhow::Error::msg)?
                != std::cmp::Ordering::Equal,
            "source catalogue has a Windows path collision"
        );
    }
    Ok(records)
}
fn find_record<'a>(
    records: &'a [twodrive_core::FileRecord],
    path: &str,
) -> anyhow::Result<Option<&'a twodrive_core::FileRecord>> {
    let (mut start, mut end) = (0, records.len());
    while start < end {
        let mid = start + (end - start) / 2;
        match twodrive_local_files::names::ordinal_ignorecase_cmp(&records[mid].metadata.path, path)
            .map_err(anyhow::Error::msg)?
        {
            std::cmp::Ordering::Less => start = mid + 1,
            std::cmp::Ordering::Greater => end = mid,
            std::cmp::Ordering::Equal => return Ok(Some(&records[mid])),
        }
    }
    Ok(None)
}

#[derive(Debug, Serialize)]
pub struct SourceAuditFailure {
    pub stage: &'static str,
    pub detail: String,
}
impl std::fmt::Display for SourceAuditFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "personal source audit failed at {}", self.stage)
    }
}
impl std::error::Error for SourceAuditFailure {}
fn failure(stage: &'static str, detail: String) -> anyhow::Error {
    anyhow::Error::new(SourceAuditFailure {
        stage,
        detail: detail.chars().take(512).collect(),
    })
}
#[derive(Default, Debug, Serialize)]
pub struct SourceSummary {
    pub personal_owner_verified: bool,
    pub files: usize,
    pub directories: usize,
    pub complete_clean_files: usize,
    pub complete_clean_bytes: u64,
    pub incomplete_files: usize,
    pub unsynchronized_files: usize,
    pub ordinary_files: usize,
    pub pinned_files: usize,
    pub maximum_file_bytes: u64,
}
impl NativeSession {
    /// Import only full, clean replicas whose bytes match the bound service
    /// version. The personal source is never registered, modified or stopped.
    pub fn copy_personal_source(&mut self, maximum: usize) -> anyhow::Result<SourceCopySummary> {
        use sha2::Digest;
        let recovered = self.coordinator.resume_source_imports()?;
        let mut summary = SourceCopySummary {
            completed: recovered.imported,
            deferred: recovered.deferred,
            isolated: recovered.isolated,
            ..Default::default()
        };
        let (owner, path) = personal_source_location().map_err(|e| failure("registry", e))?;
        ensure!(
            owner.eq_ignore_ascii_case(&self.configuration.identity.account),
            "source account mismatch"
        );
        let provider = crate::native_account::NativeAccount::new(
            self.configuration.account.clone(),
            self.configuration.identity.clone(),
            false,
        )?
        .verified()?;
        let source =
            ReadOnlyRegisteredRoot::open(&path).map_err(|e| failure("registered_root", e))?;
        ensure!(
            source.receipt().volume_serial != self.coordinator.receipt().volume_serial
                || source.receipt().file_id != self.coordinator.receipt().file_id,
            "source and target must be distinct"
        );
        let db = self.coordinator.bridge.journal().database().clone();
        let records = sorted_records(db.all_records()?)?;
        let root = db
            .get_state_value("native_cloud_root_id")?
            .context("source cloud root missing")?;
        let receipt = serde_json::to_string(source.receipt())?;
        let mut batch = Vec::new();
        let mut attempted = 0usize;
        for entry in source.inventory().map_err(|e| failure("inventory", e))? {
            if entry.directory
                || entry.attributes & 0x4000 != 0
                || entry.cloud.as_ref().is_none_or(|os| {
                    !os.resident || os.partial || !os.os_in_sync || os.modified_bytes != 0
                })
            {
                continue;
            }
            let path = NativeCoordinator::cloud_path(&entry.relative_path)?;
            let Some(record) = find_record(&records, &path)? else {
                continue;
            };
            if record.metadata.is_dir || record.metadata.size != entry.size {
                summary.deferred += 1;
                continue;
            }
            let Some(id) = record.cloud_remote_id.as_ref() else {
                summary.deferred += 1;
                continue;
            };
            summary.candidates += 1;
            let projection = self
                .coordinator
                .bridge
                .journal()
                .native_projection_for_id(&record.metadata.remote_id)?
                .context("source target publication missing")?;
            if self
                .coordinator
                .source_import_is_complete(&entry, record, &projection)?
            {
                summary.already_owned += 1;
                continue;
            }
            let parent = if record.metadata.parent_path == "/" {
                root.clone()
            } else {
                find_record(&records, &record.metadata.parent_path)?
                    .and_then(|p| p.cloud_remote_id.clone())
                    .context("source parent unbound")?
            };
            let mut expected = record.metadata.clone();
            expected.remote_id = id.clone();
            if projection.state != "published" {
                summary.deferred += 1;
                continue;
            }
            let mut temporary =
                tempfile::NamedTempFile::new_in(self.coordinator.bridge.journal().cache_root())?;
            let mut writer = SnapshotWriter {
                file: temporary.as_file_mut(),
                sha: sha2::Sha256::new(),
                quick: QuickXorHash::default(),
            };
            attempted += 1;
            let copied = source.copy_complete_clean(&entry, &mut writer);
            let sha = format!("{:x}", writer.sha.finalize());
            let quick = writer.quick;
            let Ok(bytes) = copied else {
                summary.deferred += 1;
                if maximum != 0 && attempted >= maximum {
                    break;
                }
                continue;
            };
            temporary.as_file().sync_all()?;
            summary.bytes_copied = summary
                .bytes_copied
                .checked_add(bytes)
                .context("source copy byte total overflow")?;
            batch.push((
                Candidate {
                    entry,
                    request: IntegrityRequest {
                        parent_id: parent,
                        expected,
                    },
                },
                projection,
                temporary,
                sha,
                quick,
            ));
            if batch.len() == 20 || (maximum != 0 && attempted >= maximum) {
                self.finish_source_batch(&provider, &receipt, &mut batch, &mut summary)?;
            }
            if maximum != 0 && attempted >= maximum {
                break;
            }
        }
        if !batch.is_empty() {
            self.finish_source_batch(&provider, &receipt, &mut batch, &mut summary)?;
        }
        Ok(summary)
    }

    fn finish_source_batch(
        &mut self,
        provider: &crate::onedrive::OneDrive,
        receipt: &str,
        batch: &mut Vec<(
            Candidate,
            NativeProjection,
            tempfile::NamedTempFile,
            String,
            QuickXorHash,
        )>,
        summary: &mut SourceCopySummary,
    ) -> anyhow::Result<()> {
        let requests = batch
            .iter()
            .map(|(c, _, _, _, _)| c.request.clone())
            .collect::<Vec<_>>();
        let fingerprints = provider.graph.file_integrities_bound(
            &self.configuration.identity,
            &requests,
            &mut || Ok(()),
        )?;
        for ((candidate, projection, temporary, digest, quick), fingerprint) in
            batch.drain(..).zip(fingerprints)
        {
            let Ok(fingerprint) = fingerprint else {
                let _ = temporary.keep()?;
                summary.deferred += 1;
                continue;
            };
            if !fingerprint.matches(&quick) {
                let _ = temporary.keep()?;
                summary.checksum_mismatches += 1;
                continue;
            }
            let (file, cache_path) = temporary.keep()?;
            drop(file);
            self.coordinator
                .bridge
                .journal()
                .prepare_native_source_import(
                    twodrive_core::NativeSourceImport {
                        sequence: 0,
                        expected: projection,
                        source_root_receipt: receipt.into(),
                        source_file: twodrive_core::NativeFileIdentity {
                            volume_serial: candidate.entry.volume_serial,
                            file_id: candidate.entry.file_id,
                        },
                        cache_path,
                        digest,
                        source_filetime: candidate.entry.last_write_time,
                        source_pinned: candidate.entry.cloud.as_ref().is_some_and(|s| s.pinned),
                        state: "verified".into(),
                    },
                    crate::local_files::digest_file,
                )?;
            summary.staged += 1;
        }
        let progress = self.coordinator.resume_source_imports()?;
        summary.completed += progress.imported;
        summary.deferred += progress.deferred;
        summary.isolated += progress.isolated;
        Ok(())
    }
    pub fn verify_personal_source_sample(
        &self,
        limit: usize,
    ) -> anyhow::Result<SourceChecksumSummary> {
        ensure!((1..=20).contains(&limit), "bounded source sample required");
        let (owner, path) = personal_source_location().map_err(|e| failure("registry", e))?;
        ensure!(
            owner.eq_ignore_ascii_case(&self.configuration.identity.account),
            "personal source owner differs from sealed account"
        );
        let provider = crate::native_account::NativeAccount::new(
            self.configuration.account.clone(),
            self.configuration.identity.clone(),
            false,
        )?
        .verified()?;
        let source =
            ReadOnlyRegisteredRoot::open(&path).map_err(|e| failure("registered_root", e))?;
        ensure!(
            source.receipt().volume_serial != self.coordinator.receipt().volume_serial
                || source.receipt().file_id != self.coordinator.receipt().file_id,
            "source and target roots must be distinct"
        );
        let db = self.coordinator.bridge.journal().database();
        let records = sorted_records(db.all_records()?)?;
        let root = db
            .get_state_value("native_cloud_root_id")?
            .context("cloud root missing")?;
        let mut candidates = Vec::new();
        for entry in source.inventory().map_err(|e| failure("inventory", e))? {
            if entry.directory
                || entry.size == 0
                || entry.size > 65536
                || entry.attributes & 0x4000 != 0
                || entry.cloud.as_ref().is_none_or(|os| {
                    !os.resident || os.partial || !os.os_in_sync || os.modified_bytes != 0
                })
            {
                continue;
            }
            let path = NativeCoordinator::cloud_path(&entry.relative_path)?;
            let Some(record) = find_record(&records, &path)? else {
                continue;
            };
            if record.metadata.is_dir || record.metadata.size != entry.size {
                continue;
            }
            let Some(id) = record.cloud_remote_id.as_ref() else {
                continue;
            };
            let parent = if record.metadata.parent_path == "/" {
                root.clone()
            } else {
                find_record(&records, &record.metadata.parent_path)?
                    .and_then(|p| p.cloud_remote_id.clone())
                    .context("source sample parent unbound")?
            };
            let mut expected = record.metadata.clone();
            expected.remote_id = id.clone();
            candidates.push(Candidate {
                entry,
                request: IntegrityRequest {
                    parent_id: parent,
                    expected,
                },
            });
            if candidates.len() == limit {
                break;
            }
        }
        ensure!(
            !candidates.is_empty(),
            "no complete clean source sample available"
        );
        let requested = candidates
            .iter()
            .map(|c| c.request.clone())
            .collect::<Vec<_>>();
        let fingerprints = provider.graph.file_integrities_bound(
            &self.configuration.identity,
            &requested,
            &mut || Ok(()),
        )?;
        let mut summary = SourceChecksumSummary {
            sampled: candidates.len(),
            ..Default::default()
        };
        for (candidate, fingerprint) in candidates.into_iter().zip(fingerprints) {
            let Ok(fingerprint) = fingerprint else {
                summary.metadata_deferred += 1;
                continue;
            };
            let mut sink = ChecksumSink(QuickXorHash::default());
            match source.copy_complete_clean(&candidate.entry, &mut sink) {
                Ok(bytes) => {
                    summary.bytes_read = summary
                        .bytes_read
                        .checked_add(bytes)
                        .context("sample byte total overflow")?;
                    if fingerprint.matches(&sink.0) {
                        summary.verified += 1;
                    } else {
                        summary.checksum_mismatches += 1;
                    }
                }
                Err(error) => {
                    summary.source_deferred += 1;
                    *summary
                        .source_failure_counts
                        .entry(error.chars().take(128).collect())
                        .or_default() += 1;
                }
            }
        }
        Ok(summary)
    }
    pub fn audit_personal_source(&self) -> anyhow::Result<SourceSummary> {
        let (owner, path) = personal_source_location().map_err(|e| failure("registry", e))?;
        ensure!(
            owner.eq_ignore_ascii_case(&self.configuration.identity.account),
            "personal source owner differs from the sealed account"
        );
        crate::native_account::NativeAccount::new(
            self.configuration.account.clone(),
            self.configuration.identity.clone(),
            false,
        )?
        .verified()
        .map_err(|e| {
            failure(
                "account",
                twodrive_backend::diagnostics::safe_error(&e).to_string(),
            )
        })?;
        let source =
            ReadOnlyRegisteredRoot::open(&path).map_err(|e| failure("registered_root", e))?;
        ensure!(
            source.receipt().volume_serial != self.coordinator.receipt().volume_serial
                || source.receipt().file_id != self.coordinator.receipt().file_id,
            "source and target roots must be distinct"
        );
        let mut summary = SourceSummary {
            personal_owner_verified: true,
            ..Default::default()
        };
        for entry in source.inventory().map_err(|e| failure("inventory", e))? {
            if entry.directory {
                summary.directories += 1;
                continue;
            }
            summary.files += 1;
            summary.maximum_file_bytes = summary.maximum_file_bytes.max(entry.size);
            if let Some(os) = entry.cloud {
                if os.pinned {
                    summary.pinned_files += 1;
                }
                if !os.os_in_sync || os.modified_bytes != 0 {
                    summary.unsynchronized_files += 1;
                }
                if !os.resident || os.partial {
                    summary.incomplete_files += 1;
                }
                if os.resident
                    && !os.partial
                    && os.os_in_sync
                    && os.modified_bytes == 0
                    && entry.attributes & 0x4000 == 0
                {
                    summary.complete_clean_files += 1;
                    summary.complete_clean_bytes = summary
                        .complete_clean_bytes
                        .checked_add(entry.size)
                        .context("source byte total overflow")?;
                }
            } else {
                summary.ordinary_files += 1;
            }
        }
        Ok(summary)
    }
}

impl NativeCoordinator {
    pub(crate) fn source_import_is_complete(
        &self,
        entry: &twodrive_local_files::native::persistent::NativeEntry,
        record: &twodrive_core::FileRecord,
        projection: &NativeProjection,
    ) -> anyhow::Result<bool> {
        if record.cache_path.is_none()
            || projection.state != "published"
            || !self.bridge.journal().native_source_import_completed(
                &record.metadata.remote_id,
                &record.metadata.etag,
                entry.last_write_time,
            )?
            || !self
                .bridge
                .journal()
                .snapshot_if_owned(&record.metadata.remote_id)?
                .is_some_and(|s| s.sync_confirmed())
        {
            return Ok(false);
        }
        let source_pinned = entry.cloud.as_ref().is_some_and(|os| os.pinned);
        self.owner
            .with_namespace_binding(
                &NativeCoordinator::relative(&projection.path)?,
                &projection.tag,
                projection.size,
                false,
                |volume, id, os| {
                    use std::os::windows::fs::MetadataExt;
                    let stamp = std::fs::symlink_metadata(self.root().join(
                        NativeCoordinator::relative(&projection.path).map_err(|e| e.to_string())?,
                    ))
                    .map_err(|e| e.to_string())?
                    .last_write_time();
                    Ok(volume == projection.volume_serial.unwrap_or(0)
                        && Some(format!("{id:016x}")) == projection.native_file_id
                        && os.os_in_sync
                        && os.modified_bytes == 0
                        && stamp == entry.last_write_time
                        && (!source_pinned || os.pinned || os.release_requested))
                },
            )
            .map_err(anyhow::Error::msg)
    }
}
