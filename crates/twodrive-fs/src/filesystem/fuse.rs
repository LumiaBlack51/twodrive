use super::*;

impl<B: CloudBackend> Filesystem for TwoDriveFs<B> {
    fn statfs(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        match filesystem_stats(&self.cache_dir) {
            Ok(stats) => reply.statfs(
                stats.blocks,
                stats.blocks_free,
                stats.blocks_available,
                stats.files,
                stats.files_free,
                stats.block_size,
                stats.name_length,
                stats.fragment_size,
            ),
            Err(err) => {
                eprintln!(
                    "twodrive statfs error for {}: {err}",
                    self.cache_dir.display()
                );
                reply.error(err.raw_os_error().unwrap_or(libc::EIO));
            }
        }
    }

    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let Some(parent_path) = self.inodes.path_for_ino(parent) else {
            reply.error(libc::ENOENT);
            return;
        };
        let Some(name) = name.to_str() else {
            reply.error(libc::EINVAL);
            return;
        };

        let path = join_cloud_path(parent_path, name);
        let ino = if let Some(ino) = self.inodes.ino_for_path(&path) {
            ino
        } else if let Ok(Some(record)) = self.db.get_by_path(&path) {
            self.inodes.insert_or_update(record)
        } else {
            reply.error(libc::ENOENT);
            return;
        };

        match self.attr_for_ino(ino) {
            Some(attr) => reply.entry(&TTL, &attr, 0),
            None => reply.error(libc::ENOENT),
        }
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        match self.attr_for_ino(ino) {
            Some(attr) => reply.attr(&TTL, &attr),
            None => reply.error(libc::ENOENT),
        }
    }

    fn setattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        if let Some(mode) = mode {
            let Some(mut record) = self.record_for_ino(ino) else {
                reply.error(if ino == ROOT_INO {
                    libc::EOPNOTSUPP
                } else {
                    libc::ENOENT
                });
                return;
            };
            if let Err(err) = self.db.set_local_mode(&record.metadata.remote_id, mode) {
                eprintln!("twodrive chmod error: {err:#}");
                reply.error(libc::EIO);
                return;
            }
            record.local_mode = Some((mode & 0o777) as u16);
            self.inodes.replace_ino_record(ino, record);
        }
        if let Some(size) = size {
            let result = if let Some(handle) = fh.and_then(|fh| self.write_handles.get(&fh)) {
                OpenOptions::new()
                    .write(true)
                    .open(&handle.cache_path)
                    .and_then(|file| file.set_len(size))
                    .map_err(anyhow::Error::from)
            } else {
                self.truncate_without_handle(ino, size)
            };
            match result {
                Ok(()) => self.inodes.replace_size(ino, size),
                Err(err) => {
                    eprintln!("twodrive truncate error: {err}");
                    reply.error(libc::EIO);
                    return;
                }
            }
        }
        match self.attr_for_ino(ino) {
            Some(attr) => reply.attr(&TTL, &attr),
            None => reply.error(libc::ENOENT),
        }
    }

    fn opendir(&mut self, _req: &Request<'_>, ino: u64, _flags: i32, reply: ReplyOpen) {
        let Some(path) = self.inodes.path_for_ino(ino).map(str::to_string) else {
            reply.error(libc::ENOENT);
            return;
        };
        self.refresh_child_records(&path);
        let mut entries = vec![
            (ino, FileType::Directory, ".".to_string()),
            (
                self.inodes.parent_ino(&path).unwrap_or(ROOT_INO),
                FileType::Directory,
                "..".to_string(),
            ),
        ];
        entries.extend(
            self.inodes
                .children_for_ino(ino)
                .into_iter()
                .map(|(ino, record)| (ino, file_type(&record.metadata), record.metadata.name)),
        );
        let fh = self.next_fh;
        self.next_fh += 1;
        self.directory_handles.insert(fh, entries);
        reply.opened(fh, 0);
    }

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        let Some(entries) = self.directory_handles.get(&fh) else {
            reply.error(libc::EBADF);
            return;
        };
        for (index, (ino, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
            if reply.add(*ino, (index + 1) as i64, *kind, name) {
                break;
            }
        }
        reply.ok();
    }

    fn releasedir(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        reply: ReplyEmpty,
    ) {
        self.directory_handles.remove(&fh);
        reply.ok();
    }

    fn open(&mut self, req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        match self.record_for_ino(ino) {
            Some(record) if !record.metadata.is_dir => {
                let record = self.refresh_record(&record);
                if flags & libc::O_ACCMODE != libc::O_RDONLY {
                    match self.create_overwrite_upload(record, flags & libc::O_TRUNC != 0) {
                        Ok((_ino, fh, _attr)) => reply.opened(fh, 0),
                        Err(err) => {
                            eprintln!("twodrive open overwrite error: {err:#}");
                            reply.error(libc::EIO);
                        }
                    }
                    return;
                }

                if should_defer_hydration(req, &record)
                    || (!has_existing_cache(&record) && is_gio_metadata_probe(req, flags))
                {
                    reply.error(libc::ENODATA);
                    return;
                }

                if has_existing_cache(&record) {
                    match self.open_read_handle(ino, record.cache_path.unwrap()) {
                        Ok(fh) => reply.opened(fh, 0),
                        Err(err) => {
                            eprintln!("twodrive open cache error: {err:#}");
                            reply.error(libc::EIO);
                        }
                    }
                } else {
                    let generation = match self.db.download_generation(&record.metadata.remote_id) {
                        Ok(generation) => generation,
                        Err(_) => {
                            reply.error(libc::EIO);
                            return;
                        }
                    };
                    let fh = self.next_fh;
                    self.next_fh += 1;
                    self.read_handles.insert(
                        fh,
                        ReadHandle {
                            open_attr: attr_for_record(ino, &record),
                            ino,
                            cache_path: self
                                .cache_dir
                                .join(sanitize_cache_name(&record.metadata.remote_id)),
                            _cache_guard: Arc::new(Mutex::new(None)),
                            unlinked: false,
                            deferred_record: Some(record),
                            download_generation: generation,
                        },
                    );
                    reply.opened(fh, 0);
                }
            }
            Some(_) => reply.error(libc::EISDIR),
            None => reply.error(libc::ENOENT),
        }
    }

    fn read(
        &mut self,
        req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }

        if let Some(handle) = self.read_handles.get(&_fh)
            && let Some(record) = &handle.deferred_record
        {
            // A descriptor can be passed to an indexer after another process opens it.
            // Check the reader too, before the deferred handle starts a download.
            let record = self.refresh_record(record);
            if should_defer_hydration(req, &record) {
                reply.error(libc::ENODATA);
                return;
            }
            let reader = request_process_identity(req);
            let generation = handle.download_generation;
            let guard = Arc::clone(&handle._cache_guard);
            let db = self.db.clone();
            let cache_dir = self.cache_dir.clone();
            let backend = Arc::clone(&self.backend);
            // Reply objects are owned by the worker; the FUSE dispatcher stays available
            // to serve directory listings and unrelated reads/writes during downloads.
            self.read_pool.spawn(move || {
                let result = (|| -> anyhow::Result<Vec<u8>> {
                    let mut guard = guard
                        .lock()
                        .map_err(|_| anyhow::anyhow!("read handle poisoned"))?;
                    if guard.is_none() {
                        if !has_existing_cache(&record) {
                            eprintln!(
                                "twodrive: on-demand read path={:?} reader={} offset={} size={}",
                                record.metadata.path, reader, offset, size
                            );
                        }
                        let path = hydrate_generation(
                            &db,
                            &cache_dir,
                            backend.as_ref(),
                            &record,
                            generation,
                        )?;
                        let file = fs::File::open(path)?;
                        file.lock_shared()?;
                        *guard = Some(file);
                    }
                    use std::os::unix::fs::FileExt;
                    let mut bytes = vec![0; size as usize];
                    let count = guard.as_ref().unwrap().read_at(&mut bytes, offset as u64)?;
                    bytes.truncate(count);
                    Ok(bytes)
                })();
                match result {
                    Ok(bytes) => reply.data(&bytes),
                    Err(err) => {
                        eprintln!("twodrive background read failed: {err:#}");
                        let errno = err
                            .downcast_ref::<io::Error>()
                            .and_then(io::Error::raw_os_error)
                            .unwrap_or(libc::EIO);
                        reply.error(errno);
                    }
                }
            });
            return;
        }

        if self.read_handles.contains_key(&_fh) || self.write_handles.contains_key(&_fh) {
            match self.read_open_handle(_fh, offset as u64, size) {
                Ok(data) => reply.data(&data),
                Err(err) => {
                    eprintln!("twodrive read open handle error: {err:#}");
                    reply.error(libc::EIO);
                }
            }
            return;
        }

        match self.record_for_ino(ino) {
            Some(record) if !record.metadata.is_dir => {
                let record = self.refresh_record(&record);
                if should_defer_hydration(req, &record) {
                    reply.error(libc::ENODATA);
                    return;
                }

                if !has_existing_cache(&record) {
                    eprintln!(
                        "twodrive: on-demand read path={:?} reader={} offset={} size={}",
                        record.metadata.path,
                        request_process_identity(req),
                        offset,
                        size
                    );
                }
                match self.ensure_cached(&record) {
                    Ok(cache_path) => match read_slice(&cache_path, offset as u64, size) {
                        Ok(data) => reply.data(&data),
                        Err(err) => {
                            eprintln!("twodrive read cache error: {err:#}");
                            reply.error(libc::EIO);
                        }
                    },
                    Err(err) => {
                        eprintln!("twodrive read hydrate error: {err:#}");
                        reply.error(libc::EIO);
                    }
                }
            }
            Some(_) => reply.error(libc::EISDIR),
            None => reply.error(libc::ENOENT),
        }
    }

    fn write(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        let Some(handle) = self.write_handles.get(&fh) else {
            reply.error(libc::EBADF);
            return;
        };

        match write_slice(&handle.cache_path, offset as u64, data) {
            Ok(()) => {
                self.inodes
                    .set_size(handle.ino, offset as u64 + data.len() as u64);
                if let Some(handle) = self.write_handles.get_mut(&fh)
                    && let Some(activity) = &mut handle.activity
                {
                    activity.set_progress(offset as u64 + data.len() as u64, None);
                }
                reply.written(data.len() as u32);
            }
            Err(err) => {
                eprintln!("twodrive write cache error: {err:#}");
                reply.error(libc::EIO);
            }
        }
    }

    fn flush(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        _lock_owner: u64,
        reply: ReplyEmpty,
    ) {
        let result = if self.write_handles.contains_key(&fh) {
            self.sync_handle(fh, false)
        } else if self.read_handles.contains_key(&fh) {
            self.sync_read_handle(fh, false)
        } else {
            self.sync_cached_record(ino, false)
        };
        match result {
            Ok(()) => reply.ok(),
            Err(err) => {
                eprintln!("twodrive local flush error: {err:#}");
                reply.error(libc::EIO);
            }
        }
    }

    fn fsync(&mut self, _req: &Request<'_>, ino: u64, fh: u64, datasync: bool, reply: ReplyEmpty) {
        let result = if self.write_handles.contains_key(&fh) {
            self.sync_handle(fh, datasync)
        } else if self.read_handles.contains_key(&fh) {
            self.sync_read_handle(fh, datasync)
        } else {
            self.sync_cached_record(ino, datasync)
        };
        match result {
            Ok(()) => reply.ok(),
            Err(err) => {
                eprintln!("twodrive fsync error: {err:#}");
                reply.error(libc::EIO);
            }
        }
    }

    fn release(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        if self.read_handles.contains_key(&fh) {
            match self.release_read_handle(fh) {
                Ok(()) => reply.ok(),
                Err(err) => {
                    eprintln!("twodrive read release error: {err:#}");
                    reply.error(libc::EIO);
                }
            }
            return;
        }

        let unlinked_cache = self
            .write_handles
            .get(&fh)
            .and_then(|handle| handle.unlinked.then_some(handle.cache_path.clone()));
        let result = self.queue_upload_handle(fh);
        self.write_handles.remove(&fh);
        if let Err(err) = self.db.finish_pending_releases() {
            eprintln!("twodrive: deferred release remains queued: {err:#}");
        }
        if let Some(cache_path) = unlinked_cache
            && let Err(err) = self.cleanup_unlinked_cache(&cache_path)
        {
            eprintln!("twodrive unlinked cache cleanup error: {err:#}");
        }
        match result {
            Ok(()) => reply.ok(),
            Err(err) => {
                eprintln!("twodrive upload release deferred after error: {err:#}");
                reply.ok();
            }
        }
    }

    fn mkdir(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        match self.create_directory(parent, name, mode & !umask) {
            Ok((_ino, attr)) => reply.entry(&TTL, &attr, 0),
            Err(err) => {
                eprintln!("twodrive create directory error: {err:#}");
                reply.error(libc::EIO);
            }
        }
    }

    fn unlink(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let Some(parent_path) = self.inodes.path_for_ino(parent) else {
            reply.error(libc::ENOENT);
            return;
        };
        let Some(name) = name.to_str() else {
            reply.error(libc::EINVAL);
            return;
        };

        let path = join_cloud_path(parent_path, name);
        let Some(ino) = self.inodes.ino_for_path(&path) else {
            reply.error(libc::ENOENT);
            return;
        };
        let Some(record) = self
            .record_for_ino(ino)
            .map(|record| self.refresh_record(&record))
        else {
            reply.error(libc::ENOENT);
            return;
        };
        if record.metadata.is_dir {
            reply.error(libc::EISDIR);
            return;
        }

        match self.unlink_record_with_open_handles(ino, &record) {
            Ok(true) => {
                reply.ok();
                return;
            }
            Ok(false) => {}
            Err(err) => {
                eprintln!("twodrive unlink open file error: {err:#}");
                reply.error(libc::EIO);
                return;
            }
        }

        match self.delete_record(ino, &record) {
            Ok(()) => reply.ok(),
            Err(err) => {
                eprintln!("twodrive delete file error: {err:#}");
                reply.error(libc::EIO);
            }
        }
    }

    fn rmdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let Some(parent_path) = self.inodes.path_for_ino(parent) else {
            reply.error(libc::ENOENT);
            return;
        };
        let Some(name) = name.to_str() else {
            reply.error(libc::EINVAL);
            return;
        };

        let path = join_cloud_path(parent_path, name);
        let Some(ino) = self.inodes.ino_for_path(&path) else {
            reply.error(libc::ENOENT);
            return;
        };
        let Some(record) = self
            .record_for_ino(ino)
            .map(|record| self.refresh_record(&record))
        else {
            reply.error(libc::ENOENT);
            return;
        };
        if !record.metadata.is_dir {
            reply.error(libc::ENOTDIR);
            return;
        }
        if !self.inodes.children_for_ino(ino).is_empty() {
            reply.error(libc::ENOTEMPTY);
            return;
        }

        match self.delete_record(ino, &record) {
            Ok(()) => reply.ok(),
            Err(err) => {
                eprintln!("twodrive delete directory error: {err:#}");
                reply.error(libc::EIO);
            }
        }
    }

    fn rename(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        _flags: u32,
        reply: ReplyEmpty,
    ) {
        match self.rename_record(parent, name, newparent, newname) {
            Ok(()) => reply.ok(),
            Err(err) => {
                eprintln!("twodrive rename error: {err:#}");
                reply.error(libc::EIO);
            }
        }
    }

    fn create(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        match self.create_upload_with_mode(parent, name, flags, mode & !umask) {
            Ok((_ino, fh, attr)) => reply.created(&TTL, &attr, 0, fh, 0),
            Err(err) => {
                eprintln!("twodrive create upload error: {err:#}");
                reply.error(libc::EIO);
            }
        }
    }
}
