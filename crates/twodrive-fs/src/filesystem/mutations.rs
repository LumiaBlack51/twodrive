use super::*;

impl<B: CloudBackend> TwoDriveFs<B> {
    pub(super) fn unlink_record_with_open_handles(
        &mut self,
        ino: u64,
        record: &FileRecord,
    ) -> anyhow::Result<bool> {
        let write_fhs = self
            .write_handles
            .iter()
            .filter_map(|(fh, handle)| {
                (handle.record_remote_id == record.metadata.remote_id).then_some(*fh)
            })
            .collect::<Vec<_>>();
        let read_fhs = self
            .read_handles
            .iter()
            .filter_map(|(fh, handle)| (handle.ino == ino).then_some(*fh))
            .collect::<Vec<_>>();
        if write_fhs.is_empty() && read_fhs.is_empty() {
            return Ok(false);
        }

        let created_locally = record.cloud_remote_id.is_none()
            || write_fhs.iter().any(|fh| {
                self.write_handles
                    .get(fh)
                    .is_some_and(|handle| handle.created_new_record)
            });
        if !created_locally {
            let mut queued_record = record.clone();
            queued_record.cache_path = None;
            self.db.queue_pending_delete(&queued_record)?;
            if let Some(cloud_remote_id) = &record.cloud_remote_id {
                self.upload_pool.enqueue_delete(cloud_remote_id.clone())?;
            }
        } else {
            self.db.remove_by_remote_id(&record.metadata.remote_id)?;
        }

        self.inodes.remove_ino(ino);
        for fh in write_fhs {
            if let Some(handle) = self.write_handles.get_mut(&fh) {
                handle.unlinked = true;
            }
        }
        for fh in read_fhs {
            if let Some(handle) = self.read_handles.get_mut(&fh) {
                handle.unlinked = true;
            }
        }
        Ok(true)
    }

    pub(super) fn delete_record(&mut self, ino: u64, record: &FileRecord) -> anyhow::Result<()> {
        if matches!(
            record.state,
            FileState::Writing | FileState::Dirty | FileState::Hydrating | FileState::Uploading
        ) {
            anyhow::bail!("cannot delete a file while it is changing");
        }

        if record.cloud_remote_id.is_none() {
            if let Some(cache_path) = &record.cache_path {
                match fs::remove_file(cache_path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err.into()),
                }
            }
            self.db.remove_pending_delete(&record.metadata.remote_id)?;
            self.db.remove_by_remote_id(&record.metadata.remote_id)?;
            self.inodes.remove_ino(ino);
            return Ok(());
        }

        self.db.queue_pending_delete(record)?;
        if let Some(cloud_remote_id) = &record.cloud_remote_id {
            self.upload_pool.enqueue_delete(cloud_remote_id.clone())?;
        }
        self.inodes.remove_ino(ino);
        Ok(())
    }

    pub(super) fn create_directory(
        &mut self,
        parent: u64,
        name: &OsStr,
        mode: u32,
    ) -> anyhow::Result<(u64, FileAttr)> {
        let parent_path = self
            .inodes
            .path_for_ino(parent)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("parent inode does not exist"))?;
        let name = name
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("directory name is not valid UTF-8"))?;
        if name.is_empty() || name.contains('/') {
            anyhow::bail!("invalid directory name");
        }

        let path = join_cloud_path(&parent_path, name);
        if self.db.get_by_path(&path)?.is_some() {
            anyhow::bail!("path already exists");
        }

        let local_id = format!("local-upload-{}", unique_suffix());
        let record = self
            .db
            .create_local_directory_with_mode(&local_id, &path, mode)?;
        let ino = self.inodes.insert_or_update(record.clone());
        self.upload_pool.enqueue(local_id)?;
        Ok((ino, attr_for_record(ino, &record)))
    }

    pub(super) fn rename_record(
        &mut self,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
    ) -> anyhow::Result<()> {
        let parent_path = self
            .inodes
            .path_for_ino(parent)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("source parent inode does not exist"))?;
        let newparent_path = self
            .inodes
            .path_for_ino(newparent)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("target parent inode does not exist"))?;
        let name = name
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("source name is not valid UTF-8"))?;
        let newname = newname
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("target name is not valid UTF-8"))?;
        let path = join_cloud_path(&parent_path, name);
        let new_path = join_cloud_path(&newparent_path, newname);
        if path == new_path {
            return Ok(());
        }

        let ino = self
            .inodes
            .ino_for_path(&path)
            .ok_or_else(|| anyhow::anyhow!("source inode does not exist"))?;
        let record = self
            .record_for_ino(ino)
            .map(|record| self.refresh_record(&record))
            .ok_or_else(|| anyhow::anyhow!("source record does not exist"))?;
        let active_fh = self.write_handles.iter().find_map(|(fh, handle)| {
            (handle.record_remote_id == record.metadata.remote_id && !handle.uploaded)
                .then_some(*fh)
        });
        if let Some(target) = self.db.get_by_path(&new_path)? {
            if target.metadata.is_dir || record.metadata.is_dir {
                anyhow::bail!("cannot replace directories during rename");
            }
            let target_ino = self
                .inodes
                .ino_for_path(&target.metadata.path)
                .ok_or_else(|| anyhow::anyhow!("target inode does not exist"))?;
            let source_cloud_id = record.cloud_remote_id.clone();
            let target_cloud_id = target.cloud_remote_id.clone();
            let updated = self.db.replace_file_locally(
                &record.metadata.remote_id,
                &target.metadata.remote_id,
                &new_path,
            )?;
            self.inodes.remove_ino(target_ino);
            self.inodes.replace_ino_record(ino, updated.clone());
            for handle in self
                .write_handles
                .values_mut()
                .filter(|handle| handle.record_remote_id == target.metadata.remote_id)
            {
                handle.unlinked = true;
            }
            for handle in self
                .read_handles
                .values_mut()
                .filter(|handle| handle.ino == target_ino)
            {
                handle.unlinked = true;
            }
            for handle in self
                .write_handles
                .values_mut()
                .filter(|handle| handle.record_remote_id == record.metadata.remote_id)
            {
                handle.path = new_path.clone();
                handle.record_remote_id = target.metadata.remote_id.clone();
                handle.created_new_record = target.cloud_remote_id.is_none();
                if !handle.uploaded {
                    handle.base_etag = record_upload_etag(&target);
                }
            }
            if let Some(source_cloud_id) = source_cloud_id
                && Some(source_cloud_id.as_str()) != target_cloud_id.as_deref()
            {
                self.upload_pool.enqueue_delete(source_cloud_id)?;
            }
            self.upload_pool
                .enqueue(updated.metadata.remote_id.clone())?;
            return Ok(());
        }

        self.db
            .move_subtree_and_queue(&record.metadata.remote_id, &new_path)?;
        if let Some(fh) = active_fh
            && let Some(handle) = self.write_handles.get_mut(&fh)
        {
            handle.path = new_path.clone();
        }
        for updated in std::iter::once(
            self.db
                .get_by_remote_id(&record.metadata.remote_id)?
                .ok_or_else(|| anyhow::anyhow!("locally renamed record disappeared"))?,
        )
        .chain(self.db.list_descendants(&new_path)?)
        {
            if let Some(updated_ino) = self.inodes.ino_for_remote_id(&updated.metadata.remote_id) {
                self.inodes.replace_ino_record(updated_ino, updated);
            }
        }
        self.upload_pool
            .enqueue(record.metadata.remote_id.clone())?;
        Ok(())
    }
}
