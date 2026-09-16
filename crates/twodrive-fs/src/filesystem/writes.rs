use super::*;
#[cfg(test)]
use crate::conflicts::preserve_conflict_copy;

impl<B: CloudBackend> TwoDriveFs<B> {
    #[cfg(test)]
    pub(super) fn create_upload(
        &mut self,
        parent: u64,
        name: &OsStr,
        flags: i32,
    ) -> anyhow::Result<(u64, u64, FileAttr)> {
        self.create_upload_with_mode(parent, name, flags, 0o644)
    }

    pub(super) fn create_upload_with_mode(
        &mut self,
        parent: u64,
        name: &OsStr,
        flags: i32,
        mode: u32,
    ) -> anyhow::Result<(u64, u64, FileAttr)> {
        let parent_path = self
            .inodes
            .path_for_ino(parent)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("parent inode does not exist"))?;
        let parent_record = if parent == ROOT_INO {
            None
        } else {
            self.record_for_ino(parent)
        };
        if parent_record
            .as_ref()
            .is_some_and(|record| !record.metadata.is_dir)
        {
            anyhow::bail!("parent is not a directory");
        }
        let name = name
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("file name is not valid UTF-8"))?;
        if name.is_empty() || name.contains('/') {
            anyhow::bail!("invalid file name");
        }

        let path = join_cloud_path(&parent_path, name);
        if let Some(existing) = self.db.get_by_path(&path)? {
            if flags & libc::O_EXCL != 0 {
                anyhow::bail!("path already exists");
            }
            return self.create_overwrite_upload(existing, flags & libc::O_TRUNC != 0);
        }

        fs::create_dir_all(&self.cache_dir)?;
        let temporary_remote_id = format!("local-upload-{}", unique_suffix());
        let cache_path = self
            .cache_dir
            .join(sanitize_cache_name(&temporary_remote_id));
        let cache_guard = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&cache_path)?;
        cache_guard.lock_shared()?;

        let record = match self.db.create_local_file_with_mode(
            &temporary_remote_id,
            &path,
            &cache_path,
            mode,
        ) {
            Ok(record) => record,
            Err(err) => {
                if let Err(cleanup_err) = self.db.remove_by_remote_id(&temporary_remote_id) {
                    eprintln!(
                        "twodrive: failed to roll back upload metadata for {path}: {cleanup_err:#}"
                    );
                }
                let _ = fs::remove_file(&cache_path);
                return Err(err);
            }
        };
        let ino = self.inodes.insert_or_update(record.clone());
        let fh = self.next_fh;
        self.next_fh += 1;
        self.write_handles.insert(
            fh,
            WriteHandle {
                _cache_guard: cache_guard,
                ino,
                path,
                record_remote_id: temporary_remote_id,
                cache_path,
                activity: Some(ActivityGuard::start(
                    &self.cache_dir,
                    "upload",
                    &record.metadata.path,
                    &record.metadata.name,
                    None,
                )),
                uploaded: false,
                created_new_record: true,
                unlinked: false,
                base_etag: None,
            },
        );

        Ok((ino, fh, attr_for_record(ino, &record)))
    }

    pub(super) fn create_overwrite_upload(
        &mut self,
        record: FileRecord,
        truncate: bool,
    ) -> anyhow::Result<(u64, u64, FileAttr)> {
        if record.metadata.is_dir {
            anyhow::bail!("cannot overwrite a directory as a file");
        }
        let ino = self
            .inodes
            .ino_for_path(&record.metadata.path)
            .ok_or_else(|| anyhow::anyhow!("existing inode disappeared"))?;

        fs::create_dir_all(&self.cache_dir)?;
        let cache_path = if truncate {
            record.cache_path.clone().unwrap_or_else(|| {
                self.cache_dir
                    .join(sanitize_cache_name(&record.metadata.remote_id))
            })
        } else {
            self.ensure_cached(&record)?
        };
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        let cache_guard = options.open(&cache_path)?;
        cache_guard.lock_shared()?;
        // A release may have won before the shared lock. Never write to an
        // unlinked cache inode and then claim the save succeeded.
        if !cache_path.exists() {
            anyhow::bail!("cache was released while opening; retry the save");
        }
        if truncate {
            cache_guard.set_len(0)?;
        }
        self.db
            .mark_cached(&record.metadata.remote_id, &cache_path)?;
        self.db
            .mark_state(&record.metadata.remote_id, FileState::Writing)?;
        let updated = self
            .db
            .get_by_remote_id(&record.metadata.remote_id)?
            .ok_or_else(|| anyhow::anyhow!("overwrite record disappeared"))?;
        self.inodes.replace_ino_record(ino, updated.clone());
        let base_etag = record_upload_etag(&record);

        let fh = self.next_fh;
        self.next_fh += 1;
        self.write_handles.insert(
            fh,
            WriteHandle {
                _cache_guard: cache_guard,
                ino,
                path: record.metadata.path,
                record_remote_id: record.metadata.remote_id,
                cache_path,
                activity: Some(ActivityGuard::start(
                    &self.cache_dir,
                    "upload",
                    &updated.metadata.path,
                    &updated.metadata.name,
                    None,
                )),
                uploaded: false,
                created_new_record: false,
                unlinked: false,
                base_etag,
            },
        );
        Ok((ino, fh, attr_for_record(ino, &updated)))
    }

    #[cfg(test)]
    pub(super) fn upload_handle(&mut self, fh: u64) -> anyhow::Result<()> {
        let Some(handle) = self.write_handles.get_mut(&fh) else {
            return Ok(());
        };
        if handle.uploaded {
            return Ok(());
        }
        if handle.unlinked {
            handle.uploaded = true;
            handle.activity.take();
            return Ok(());
        }

        if let Ok(Some(record)) = self.db.get_by_remote_id(&handle.record_remote_id) {
            let _ = self
                .db
                .mark_state(&record.metadata.remote_id, FileState::Uploading);
        }

        let remote_id = self
            .db
            .get_by_remote_id(&handle.record_remote_id)?
            .and_then(|record| record.cloud_remote_id);
        let uploaded = match self.backend.upload_file_with_version(
            &handle.path,
            &handle.cache_path,
            remote_id.as_deref(),
            handle.base_etag.as_deref(),
            &mut |bytes_done, bytes_total| {
                if let Some(activity) = &mut handle.activity {
                    activity.set_progress(bytes_done, Some(bytes_total));
                }
                Ok(())
            },
        ) {
            Ok(uploaded) => uploaded,
            Err(err) if self.backend.is_conflict_error(&err) => {
                self.db
                    .mark_state(&handle.record_remote_id, FileState::Conflict)?;
                let record = self
                    .db
                    .get_by_remote_id(&handle.record_remote_id)?
                    .ok_or_else(|| anyhow::anyhow!("conflicting upload record disappeared"))?;
                let conflict = preserve_conflict_copy(
                    &self.db,
                    self.backend.as_ref(),
                    &record,
                    &handle.cache_path,
                    &mut |bytes_done, bytes_total| {
                        if let Some(activity) = &mut handle.activity {
                            activity.set_progress(bytes_done, Some(bytes_total));
                        }
                        Ok(())
                    },
                )?;
                let original = self
                    .db
                    .get_by_remote_id(&handle.record_remote_id)?
                    .ok_or_else(|| anyhow::anyhow!("original conflict record disappeared"))?;
                self.inodes.replace_ino_record(handle.ino, original);
                let conflict_record = self
                    .db
                    .get_by_remote_id(&conflict.remote_id)?
                    .ok_or_else(|| anyhow::anyhow!("conflict copy record disappeared"))?;
                self.inodes.insert_or_update(conflict_record);
                handle.uploaded = true;
                handle.activity.take();
                return Ok(());
            }
            Err(err) => {
                let _ = self
                    .db
                    .mark_state(&handle.record_remote_id, FileState::Dirty);
                return Err(err);
            }
        };
        let record = match self.db.commit_uploaded(
            &handle.record_remote_id,
            &handle.path,
            &uploaded,
            &handle.cache_path,
        ) {
            Ok(record) => record,
            Err(err) => {
                self.db
                    .queue_remote_delete(&uploaded.remote_id, &handle.path)?;
                return Err(err);
            }
        };
        if record.cloud_remote_id.as_deref() != Some(uploaded.remote_id.as_str()) {
            self.db
                .queue_remote_delete(&uploaded.remote_id, &handle.path)?;
        }
        self.inodes.replace_ino_record(handle.ino, record);
        handle.uploaded = true;
        handle.activity.take();
        Ok(())
    }

    pub(super) fn queue_upload_handle(&mut self, fh: u64) -> anyhow::Result<()> {
        let Some(handle) = self.write_handles.get_mut(&fh) else {
            return Ok(());
        };
        if handle.uploaded {
            return Ok(());
        }
        if handle.unlinked {
            handle.uploaded = true;
            handle.activity.take();
            return Ok(());
        }
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&handle.cache_path)?
            .sync_all()?;
        let size = fs::metadata(&handle.cache_path)?.len();
        self.db
            .mark_dirty_with_size(&handle.record_remote_id, size)?;
        // Publish the completed local generation before an editor can reopen
        // it. A later directory refresh must not reveal a different mtime.
        let record = self
            .db
            .get_by_remote_id(&handle.record_remote_id)?
            .ok_or_else(|| anyhow::anyhow!("closed upload record disappeared"))?;
        self.inodes.replace_ino_record(handle.ino, record);
        self.upload_pool.enqueue(handle.record_remote_id.clone())?;
        handle.uploaded = true;
        handle.activity.take();
        Ok(())
    }

    pub(super) fn sync_handle(&self, fh: u64, datasync: bool) -> anyhow::Result<()> {
        let handle = self
            .write_handles
            .get(&fh)
            .ok_or_else(|| anyhow::anyhow!("write handle does not exist"))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&handle.cache_path)?;
        if datasync {
            file.sync_data()?;
        } else {
            file.sync_all()?;
        }
        Ok(())
    }

    pub(super) fn sync_cached_record(&self, ino: u64, datasync: bool) -> anyhow::Result<()> {
        let record = self
            .record_for_ino(ino)
            .map(|record| self.refresh_record(&record))
            .ok_or_else(|| anyhow::anyhow!("file record does not exist"))?;
        let cache_path = record
            .cache_path
            .ok_or_else(|| anyhow::anyhow!("file has no local cache to sync"))?;
        let file = fs::File::open(cache_path)?;
        if datasync {
            file.sync_data()?;
        } else {
            file.sync_all()?;
        }
        Ok(())
    }

    pub(super) fn truncate_without_handle(&mut self, ino: u64, size: u64) -> anyhow::Result<()> {
        let record = self
            .record_for_ino(ino)
            .map(|record| self.refresh_record(&record))
            .ok_or_else(|| anyhow::anyhow!("truncate record does not exist"))?;
        if record.metadata.is_dir {
            anyhow::bail!("cannot truncate a directory");
        }
        fs::create_dir_all(&self.cache_dir)?;
        let cache_path = if size == 0 {
            record.cache_path.clone().unwrap_or_else(|| {
                self.cache_dir
                    .join(sanitize_cache_name(&record.metadata.remote_id))
            })
        } else {
            self.ensure_cached(&record)?
        };
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&cache_path)?;
        file.set_len(size)?;
        file.sync_all()?;
        self.db
            .mark_cached(&record.metadata.remote_id, &cache_path)?;
        self.db
            .mark_dirty_with_size(&record.metadata.remote_id, size)?;
        self.inodes.replace_size(ino, size);
        self.upload_pool
            .enqueue(record.metadata.remote_id.clone())?;
        Ok(())
    }
}
