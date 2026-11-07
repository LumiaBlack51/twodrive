use super::*;

impl<B: CloudBackend> TwoDriveFs<B> {
    pub(super) fn open_read_handle(
        &mut self,
        ino: u64,
        cache_path: PathBuf,
    ) -> anyhow::Result<u64> {
        let open_attr = self
            .attr_for_ino(ino)
            .ok_or_else(|| anyhow::anyhow!("read inode does not exist"))?;
        let cache_guard = fs::File::open(&cache_path)?;
        cache_guard.lock_shared()?;
        let fh = self.next_fh;
        self.next_fh += 1;
        self.read_handles.insert(
            fh,
            ReadHandle {
                open_attr,
                _cache_guard: Arc::new(Mutex::new(Some(cache_guard))),
                ino,
                cache_path,
                unlinked: false,
                deferred_record: None,
                download_generation: 0,
            },
        );
        Ok(fh)
    }

    pub(super) fn read_open_handle(
        &self,
        fh: u64,
        offset: u64,
        size: u32,
    ) -> anyhow::Result<Vec<u8>> {
        if let Some(handle) = self.read_handles.get(&fh) {
            use std::os::unix::fs::FileExt;
            let guard = handle
                ._cache_guard
                .lock()
                .map_err(|_| anyhow::anyhow!("read handle poisoned"))?;
            let file = guard
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("read is still deferred"))?;
            let mut bytes = vec![0; size as usize];
            let count = file.read_at(&mut bytes, offset)?;
            bytes.truncate(count);
            return Ok(bytes);
        }
        let cache_path = self
            .write_handles
            .get(&fh)
            .map(|handle| &handle.cache_path)
            .ok_or_else(|| anyhow::anyhow!("open file handle does not exist"))?;
        read_slice(cache_path, offset, size)
    }

    pub(super) fn sync_read_handle(&self, fh: u64, datasync: bool) -> anyhow::Result<()> {
        let handle = self
            .read_handles
            .get(&fh)
            .ok_or_else(|| anyhow::anyhow!("read handle does not exist"))?;
        if handle.deferred_record.is_some() {
            return Ok(());
        }
        let file = fs::File::open(&handle.cache_path)?;
        if datasync {
            file.sync_data()?;
        } else {
            file.sync_all()?;
        }
        Ok(())
    }

    pub(super) fn release_read_handle(&mut self, fh: u64) -> anyhow::Result<()> {
        let Some(handle) = self.read_handles.remove(&fh) else {
            return Ok(());
        };
        let cache_path = handle.unlinked.then(|| handle.cache_path.clone());
        drop(handle);
        if let Some(cache_path) = cache_path {
            self.cleanup_unlinked_cache(&cache_path)?;
        }
        self.db.finish_pending_releases()?;
        Ok(())
    }

    pub(super) fn cleanup_unlinked_cache(&self, cache_path: &Path) -> anyhow::Result<()> {
        if let Some(delete) = self.db.pending_deletes()?.into_iter().find(|delete| {
            delete.cache_path.as_deref() == Some(cache_path)
                || delete.upload_cache_path.as_deref() == Some(cache_path)
        }) {
            // The worker owns cleanup while an upload may still be using this path.
            self.upload_pool.enqueue_delete(delete.remote_id)?;
            return Ok(());
        }
        let still_open = self
            .read_handles
            .values()
            .any(|handle| handle.cache_path == cache_path)
            || self
                .write_handles
                .values()
                .any(|handle| handle.cache_path == cache_path);
        if !still_open {
            self.db.remove_unreferenced_cache(cache_path)?;
        }
        Ok(())
    }
}
