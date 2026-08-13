use fuser::{
    FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory,
    ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite, Request, TimeOrNow,
};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use twodrive_backend::{CloudBackend, GraphBackend, MockBackend};
use twodrive_core::{
    AppPaths, Database, FileRecord, FileState, MetadataEntry, join_cloud_path, normalize_cloud_path,
};

const ROOT_INO: u64 = 1;
const TTL: Duration = Duration::from_secs(30);

pub fn sync_metadata<B: CloudBackend>(db: &Database, backend: &B) -> anyhow::Result<usize> {
    let entries = backend.list_all()?;
    let count = entries.len();
    for entry in entries {
        db.upsert_metadata(&entry)?;
    }
    Ok(count)
}

pub fn sync_delta_metadata<B: CloudBackend>(db: &Database, backend: &B) -> anyhow::Result<usize> {
    let delta = backend.list_delta(db.delta_link()?.as_deref())?;
    for remote_id in &delta.deleted_remote_ids {
        db.remove_pending_delete(remote_id)?;
        if db
            .get_by_remote_id(remote_id)?
            .is_some_and(|record| matches!(record.state, FileState::Dirty | FileState::Uploading))
        {
            continue;
        }
        db.remove_by_remote_id(remote_id)?;
    }

    let count = delta.entries.len();
    for entry in delta.entries {
        db.upsert_metadata(&entry)?;
    }
    if let Some(delta_link) = delta.delta_link {
        db.set_delta_link(&delta_link)?;
    }
    Ok(count)
}

pub fn hydrate_pending_pins<B: CloudBackend>(
    db: &Database,
    cache_dir: &Path,
    backend: &B,
) -> anyhow::Result<usize> {
    let records = db
        .all_records()?
        .into_iter()
        .filter(|record| {
            !record.metadata.is_dir
                && record.effective_pinned()
                && !record.cache_path.as_deref().is_some_and(Path::exists)
        })
        .collect::<Vec<_>>();
    let mut hydrated = 0;
    for record in records {
        match hydrate_record(db, cache_dir, backend, &record) {
            Ok(_) => hydrated += 1,
            Err(err) => {
                eprintln!(
                    "twodrive: pinned hydration remains queued for {}: {err:#}",
                    record.metadata.path
                );
            }
        }
    }
    Ok(hydrated)
}

pub fn recover_dirty_uploads<B: CloudBackend>(db: &Database, backend: &B) -> anyhow::Result<usize> {
    let records = db
        .all_records()?
        .into_iter()
        .filter(|record| {
            matches!(
                record.state,
                FileState::Dirty | FileState::Uploading | FileState::Conflict
            ) && record.cache_path.as_deref().is_some_and(Path::exists)
        })
        .collect::<Vec<_>>();
    let mut recovered = 0;
    for record in records {
        let cache_path = record.cache_path.clone().expect("filtered cache path");
        if record.state == FileState::Conflict {
            match preserve_conflict_copy(db, backend, &record, &cache_path, &mut |_, _| Ok(())) {
                Ok(_) => recovered += 1,
                Err(err) => {
                    eprintln!(
                        "twodrive: conflict copy remains queued for {}: {err:#}",
                        record.metadata.path
                    );
                }
            }
            continue;
        }

        db.mark_state(&record.metadata.remote_id, FileState::Uploading)?;
        match backend.upload_file_with_version(
            &record.metadata.path,
            &cache_path,
            record_upload_remote_id(&record),
            record_upload_etag(&record).as_deref(),
            &mut |_, _| Ok(()),
        ) {
            Ok(uploaded) => {
                if uploaded.remote_id != record.metadata.remote_id {
                    db.remove_by_remote_id(&record.metadata.remote_id)?;
                }
                db.upsert_metadata(&uploaded)?;
                db.mark_cached(&uploaded.remote_id, &cache_path)?;
                recovered += 1;
            }
            Err(err) => {
                if is_conflict_error(&err) {
                    db.mark_state(&record.metadata.remote_id, FileState::Conflict)?;
                    let conflict_record = db
                        .get_by_remote_id(&record.metadata.remote_id)?
                        .unwrap_or(record.clone());
                    match preserve_conflict_copy(
                        db,
                        backend,
                        &conflict_record,
                        &cache_path,
                        &mut |_, _| Ok(()),
                    ) {
                        Ok(_) => {
                            recovered += 1;
                            continue;
                        }
                        Err(conflict_err) => {
                            eprintln!(
                                "twodrive: conflict copy remains queued for {}: {conflict_err:#}",
                                record.metadata.path
                            );
                        }
                    }
                } else {
                    db.mark_state(&record.metadata.remote_id, FileState::Dirty)?;
                }
                eprintln!(
                    "twodrive: dirty upload remains queued for {}: {err:#}",
                    record.metadata.path
                );
            }
        }
    }
    Ok(recovered)
}

pub fn recover_pending_deletes<B: CloudBackend>(
    db: &Database,
    backend: &B,
) -> anyhow::Result<usize> {
    let pending = db.pending_deletes()?;
    let mut recovered = 0;
    for delete in pending {
        match backend.delete(&delete.remote_id) {
            Ok(()) => {
                if let Some(cache_path) = &delete.cache_path {
                    match fs::remove_file(cache_path) {
                        Ok(()) => {}
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                        Err(err) => return Err(err.into()),
                    }
                }
                db.remove_pending_delete(&delete.remote_id)?;
                recovered += 1;
            }
            Err(err) => {
                eprintln!(
                    "twodrive: pending delete remains queued for {}: {err:#}",
                    delete.path
                );
            }
        }
    }
    Ok(recovered)
}

pub fn mount_mock(paths: AppPaths) -> anyhow::Result<()> {
    cleanup_stale_mountpoint(&paths.mount_dir);
    paths.ensure()?;
    let db = Database::new(paths.db_path.clone());
    db.init()?;
    let backend = MockBackend::new();
    let deleted = recover_pending_deletes(&db, &backend)?;
    let count = sync_metadata(&db, &backend)?;
    let recovered = recover_dirty_uploads(&db, &backend)?;
    let hydrated = hydrate_pending_pins(&db, &paths.cache_dir, &backend)?;
    println!("twodrive: loaded {count} mock metadata entries");
    if deleted > 0 || recovered > 0 || hydrated > 0 {
        println!(
            "twodrive: recovered {deleted} delete(s), {recovered} upload(s), hydrated {hydrated} pinned file(s)"
        );
    }
    println!("twodrive: database {}", db.path().display());
    println!("twodrive: cache {}", paths.cache_dir.display());
    println!("twodrive: mounting {}", paths.mount_dir.display());

    mount_backend(
        paths.mount_dir.clone(),
        db,
        paths.cache_dir.clone(),
        backend,
    )?;
    Ok(())
}

pub fn mount_graph(paths: AppPaths) -> anyhow::Result<()> {
    cleanup_stale_mountpoint(&paths.mount_dir);
    paths.ensure()?;
    let db = Database::new(paths.db_path.clone());
    db.init()?;
    let backend = GraphBackend::from_paths(&paths)?;
    let deleted = recover_pending_deletes(&db, &backend)?;
    let count = sync_delta_metadata(&db, &backend)?;
    let recovered = recover_dirty_uploads(&db, &backend)?;
    let hydrated = hydrate_pending_pins(&db, &paths.cache_dir, &backend)?;
    println!("twodrive: synced {count} OneDrive metadata entries");
    if recovered > 0 {
        println!("twodrive: recovered {recovered} interrupted upload(s)");
    }
    if deleted > 0 {
        println!("twodrive: recovered {deleted} interrupted delete(s)");
    }
    if hydrated > 0 {
        println!("twodrive: hydrated {hydrated} inherited pinned file(s)");
    }
    println!("twodrive: database {}", db.path().display());
    println!("twodrive: cache {}", paths.cache_dir.display());
    println!("twodrive: mounting {}", paths.mount_dir.display());

    mount_backend(
        paths.mount_dir.clone(),
        db,
        paths.cache_dir.clone(),
        backend,
    )
}

pub fn mount_backend<B: CloudBackend>(
    mount_dir: PathBuf,
    db: Database,
    cache_dir: PathBuf,
    backend: B,
) -> anyhow::Result<()> {
    let fs = TwoDriveFs::new(db, cache_dir, backend)?;
    let options = [MountOption::FSName("twodrive".to_string())];
    fuser::mount2(fs, &mount_dir, &options)?;
    Ok(())
}

pub fn hydrate_record<B: CloudBackend>(
    db: &Database,
    cache_dir: &Path,
    backend: &B,
    record: &FileRecord,
) -> anyhow::Result<PathBuf> {
    if record.metadata.is_dir {
        anyhow::bail!("cannot hydrate a directory");
    }

    if matches!(
        record.state,
        FileState::Cached | FileState::Pinned | FileState::Dirty | FileState::Uploading
    ) && let Some(cache_path) = &record.cache_path
        && cache_path.exists()
    {
        db.mark_cache_accessed(&record.metadata.remote_id)?;
        return Ok(cache_path.clone());
    }

    if record.state != FileState::Pinned {
        db.mark_state(&record.metadata.remote_id, FileState::Hydrating)?;
    }
    fs::create_dir_all(cache_dir)?;

    let mut activity = ActivityGuard::start(
        cache_dir,
        "download",
        &record.metadata.path,
        &record.metadata.name,
        Some(record.metadata.size),
    );
    let cache_path = cache_dir.join(sanitize_cache_name(&record.metadata.remote_id));
    let tmp_path = cache_path.with_extension("tmp");
    let mut tmp_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tmp_path)?;
    backend.download_sized_to(
        &record.metadata.remote_id,
        record.metadata.size,
        &mut tmp_file,
        &mut |bytes_done| {
            activity.set_progress(bytes_done, Some(record.metadata.size));
            Ok(())
        },
    )?;
    drop(tmp_file);
    fs::rename(&tmp_path, &cache_path)?;
    db.mark_cached(&record.metadata.remote_id, &cache_path)?;

    Ok(cache_path)
}

pub fn pin_path<B: CloudBackend>(
    db: &Database,
    cache_dir: &Path,
    backend: &B,
    path: &str,
) -> anyhow::Result<usize> {
    let Some(record) = db.get_by_path(path)? else {
        anyhow::bail!(
            "path is not in twodrive metadata: {}",
            normalize_cloud_path(path)
        );
    };

    db.set_explicit_pin(&record.metadata.remote_id, true)?;
    if !record.metadata.is_dir {
        let updated = db
            .get_by_remote_id(&record.metadata.remote_id)?
            .unwrap_or(record);
        hydrate_record(db, cache_dir, backend, &updated)?;
        return Ok(1);
    }

    let mut count = 0;
    for child in db.list_descendants(&record.metadata.path)? {
        if !child.metadata.is_dir && child.effective_pinned() {
            let updated = db
                .get_by_remote_id(&child.metadata.remote_id)?
                .unwrap_or(child);
            hydrate_record(db, cache_dir, backend, &updated)?;
            count += 1;
        }
    }
    Ok(count)
}

pub fn unpin_path(db: &Database, path: &str) -> anyhow::Result<usize> {
    let Some(record) = db.get_by_path(path)? else {
        anyhow::bail!(
            "path is not in twodrive metadata: {}",
            normalize_cloud_path(path)
        );
    };

    db.set_explicit_pin(&record.metadata.remote_id, false)?;
    if !record.metadata.is_dir {
        return Ok(1);
    }

    Ok(db.list_descendants(&record.metadata.path)?.len())
}

pub struct TwoDriveFs<B: CloudBackend> {
    db: Database,
    cache_dir: PathBuf,
    backend: Arc<B>,
    hydrating: Mutex<HashSet<String>>,
    inodes: InodeTable,
    read_handles: HashMap<u64, ReadHandle>,
    write_handles: HashMap<u64, WriteHandle>,
    next_fh: u64,
}

impl<B: CloudBackend> TwoDriveFs<B> {
    pub fn new(db: Database, cache_dir: PathBuf, backend: B) -> anyhow::Result<Self> {
        let records = db.all_records()?;
        let _ = clear_activity_file(&cache_dir);
        Ok(Self {
            db,
            cache_dir,
            backend: Arc::new(backend),
            hydrating: Mutex::new(HashSet::new()),
            inodes: InodeTable::new(&records),
            read_handles: HashMap::new(),
            write_handles: HashMap::new(),
            next_fh: 1,
        })
    }

    fn record_for_ino(&self, ino: u64) -> Option<FileRecord> {
        if ino == ROOT_INO {
            return None;
        }

        self.inodes.record_for_ino(ino).cloned()
    }

    fn attr_for_ino(&self, ino: u64) -> Option<FileAttr> {
        if ino == ROOT_INO {
            return Some(root_attr());
        }

        self.record_for_ino(ino)
            .map(|record| attr_for_record(ino, &record))
    }

    fn refresh_child_records(&mut self, parent_path: &str) {
        match self.db.list_children(parent_path) {
            Ok(records) => {
                for record in records {
                    self.inodes.insert_or_update(record);
                }
            }
            Err(err) => {
                eprintln!("twodrive refresh children error for {parent_path}: {err:#}");
            }
        }
    }

    fn refresh_record(&self, record: &FileRecord) -> FileRecord {
        self.db
            .get_by_remote_id(&record.metadata.remote_id)
            .ok()
            .flatten()
            .unwrap_or_else(|| record.clone())
    }

    fn ensure_cached(&self, record: &FileRecord) -> anyhow::Result<PathBuf> {
        let remote_id = record.metadata.remote_id.clone();
        loop {
            let mut hydrating = self
                .hydrating
                .lock()
                .map_err(|_| anyhow::anyhow!("hydration lock is poisoned"))?;
            if hydrating.insert(remote_id.clone()) {
                break;
            }
            drop(hydrating);
            std::thread::sleep(Duration::from_millis(50));
        }

        let result = hydrate_record(&self.db, &self.cache_dir, self.backend.as_ref(), record);
        if let Ok(mut hydrating) = self.hydrating.lock() {
            hydrating.remove(&remote_id);
        }
        result
    }

    fn open_read_handle(&mut self, ino: u64, cache_path: PathBuf) -> u64 {
        let fh = self.next_fh;
        self.next_fh += 1;
        self.read_handles.insert(
            fh,
            ReadHandle {
                ino,
                cache_path,
                unlinked: false,
            },
        );
        fh
    }

    fn read_open_handle(&self, fh: u64, offset: u64, size: u32) -> anyhow::Result<Vec<u8>> {
        let cache_path = self
            .read_handles
            .get(&fh)
            .map(|handle| &handle.cache_path)
            .or_else(|| self.write_handles.get(&fh).map(|handle| &handle.cache_path))
            .ok_or_else(|| anyhow::anyhow!("open file handle does not exist"))?;
        read_slice(cache_path, offset, size)
    }

    fn sync_read_handle(&self, fh: u64, datasync: bool) -> anyhow::Result<()> {
        let handle = self
            .read_handles
            .get(&fh)
            .ok_or_else(|| anyhow::anyhow!("read handle does not exist"))?;
        let file = fs::File::open(&handle.cache_path)?;
        if datasync {
            file.sync_data()?;
        } else {
            file.sync_all()?;
        }
        Ok(())
    }

    fn release_read_handle(&mut self, fh: u64) -> anyhow::Result<()> {
        let Some(handle) = self.read_handles.remove(&fh) else {
            return Ok(());
        };
        if handle.unlinked {
            self.cleanup_unlinked_cache(&handle.cache_path)?;
        }
        Ok(())
    }

    fn cleanup_unlinked_cache(&self, cache_path: &Path) -> anyhow::Result<()> {
        let still_open = self
            .read_handles
            .values()
            .any(|handle| handle.cache_path == cache_path)
            || self
                .write_handles
                .values()
                .any(|handle| handle.cache_path == cache_path);
        if !still_open {
            match fs::remove_file(cache_path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        }
        Ok(())
    }

    fn unlink_record_with_open_handles(
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

        let created_locally = write_fhs.iter().any(|fh| {
            self.write_handles
                .get(fh)
                .is_some_and(|handle| handle.created_new_record)
        });
        if !created_locally && let Err(err) = self.backend.delete(&record.metadata.remote_id) {
            eprintln!(
                "twodrive: queued open-file delete for {} after backend failure: {err:#}",
                record.metadata.path
            );
            let mut queued_record = record.clone();
            queued_record.cache_path = None;
            self.db.queue_pending_delete(&queued_record)?;
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

    fn delete_record(&mut self, ino: u64, record: &FileRecord) -> anyhow::Result<()> {
        if matches!(
            record.state,
            FileState::Dirty | FileState::Hydrating | FileState::Uploading
        ) {
            anyhow::bail!("cannot delete a file while it is changing");
        }

        if let Err(err) = self.backend.delete(&record.metadata.remote_id) {
            eprintln!(
                "twodrive: queued delete for {} after backend failure: {err:#}",
                record.metadata.path
            );
            self.db.queue_pending_delete(record)?;
            self.inodes.remove_ino(ino);
            return Ok(());
        }
        if let Some(cache_path) = &record.cache_path {
            match fs::remove_file(cache_path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        }
        self.db.remove_by_remote_id(&record.metadata.remote_id)?;
        self.inodes.remove_ino(ino);
        Ok(())
    }

    fn create_upload(
        &mut self,
        parent: u64,
        name: &OsStr,
        flags: i32,
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
        OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&cache_path)?;

        let now = current_unix_i64();
        let metadata = MetadataEntry::new_file(
            temporary_remote_id.clone(),
            path.clone(),
            0,
            now,
            format!("etag-{temporary_remote_id}"),
        );
        self.db.upsert_metadata(&metadata)?;
        self.db.mark_cached(&temporary_remote_id, &cache_path)?;
        self.db.mark_state(&temporary_remote_id, FileState::Dirty)?;
        let record = self
            .db
            .get_by_remote_id(&temporary_remote_id)?
            .ok_or_else(|| anyhow::anyhow!("created upload record disappeared"))?;
        let ino = self.inodes.insert_or_update(record.clone());
        let fh = self.next_fh;
        self.next_fh += 1;
        self.write_handles.insert(
            fh,
            WriteHandle {
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

    fn create_overwrite_upload(
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
        if truncate {
            options.truncate(true);
        }
        options.open(&cache_path)?;
        self.db
            .mark_cached(&record.metadata.remote_id, &cache_path)?;
        self.db
            .mark_state(&record.metadata.remote_id, FileState::Dirty)?;
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

    fn upload_handle(&mut self, fh: u64) -> anyhow::Result<()> {
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

        let remote_id = (!handle.record_remote_id.starts_with("local-upload-"))
            .then_some(handle.record_remote_id.as_str());
        let uploaded = match self.backend.upload_file_with_version(
            &handle.path,
            &handle.cache_path,
            remote_id,
            handle.base_etag.as_deref(),
            &mut |bytes_done, bytes_total| {
                if let Some(activity) = &mut handle.activity {
                    activity.set_progress(bytes_done, Some(bytes_total));
                }
                Ok(())
            },
        ) {
            Ok(uploaded) => uploaded,
            Err(err) if is_conflict_error(&err) => {
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
        if handle.record_remote_id != uploaded.remote_id {
            self.db.remove_by_remote_id(&handle.record_remote_id)?;
        }
        self.db.upsert_metadata(&uploaded)?;
        self.db
            .mark_cached(&uploaded.remote_id, &handle.cache_path)?;
        let record = self
            .db
            .get_by_remote_id(&uploaded.remote_id)?
            .ok_or_else(|| anyhow::anyhow!("uploaded record disappeared"))?;
        self.inodes.replace_ino_record(handle.ino, record);
        handle.uploaded = true;
        handle.activity.take();
        Ok(())
    }

    fn sync_handle(&self, fh: u64, datasync: bool) -> anyhow::Result<()> {
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
        self.db
            .mark_state(&handle.record_remote_id, FileState::Dirty)?;
        Ok(())
    }

    fn sync_cached_record(&self, ino: u64, datasync: bool) -> anyhow::Result<()> {
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

    fn create_directory(&mut self, parent: u64, name: &OsStr) -> anyhow::Result<(u64, FileAttr)> {
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

        let entry = self.backend.create_folder(&path)?;
        self.db.upsert_metadata(&entry)?;
        let record = self
            .db
            .get_by_remote_id(&entry.remote_id)?
            .ok_or_else(|| anyhow::anyhow!("created directory record disappeared"))?;
        let ino = self.inodes.insert_or_update(record.clone());
        Ok((ino, attr_for_record(ino, &record)))
    }

    fn rename_record(
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
            if active_fh.is_some() {
                if let Some(fh) = active_fh
                    && let Some(handle) = self.write_handles.get_mut(&fh)
                {
                    handle.base_etag = record_upload_etag(&target);
                }
                self.db.remove_by_remote_id(&target.metadata.remote_id)?;
                self.inodes.remove_ino(target_ino);
            } else {
                let cache_path = record
                    .cache_path
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("rename source has no stable local cache"))?;
                let uploaded = self.backend.upload_file_with_version(
                    &new_path,
                    &cache_path,
                    Some(&target.metadata.remote_id),
                    record_upload_etag(&target).as_deref(),
                    &mut |_, _| Ok(()),
                )?;
                if uploaded.remote_id != record.metadata.remote_id {
                    if let Err(err) = self.backend.delete(&record.metadata.remote_id) {
                        eprintln!(
                            "twodrive: replaced target but could not remove old source item {}: {err:#}",
                            record.metadata.remote_id
                        );
                    }
                    self.db.remove_by_remote_id(&record.metadata.remote_id)?;
                }
                if let Some(target_cache) = &target.cache_path
                    && target_cache != &cache_path
                {
                    let _ = fs::remove_file(target_cache);
                }
                self.db.remove_by_remote_id(&target.metadata.remote_id)?;
                self.inodes.remove_ino(target_ino);
                self.db.upsert_metadata(&uploaded)?;
                self.db.mark_cached(&uploaded.remote_id, &cache_path)?;
                let updated = self
                    .db
                    .get_by_remote_id(&uploaded.remote_id)?
                    .ok_or_else(|| anyhow::anyhow!("replacement record disappeared"))?;
                self.inodes.replace_ino_record(ino, updated);
                return Ok(());
            }
        }
        if let Some(fh) = active_fh {
            self.db
                .move_subtree(&record.metadata.remote_id, &new_path)?;
            let updated = self
                .db
                .get_by_remote_id(&record.metadata.remote_id)?
                .ok_or_else(|| anyhow::anyhow!("locally renamed record disappeared"))?;
            self.inodes.replace_ino_record(ino, updated);
            if let Some(handle) = self.write_handles.get_mut(&fh) {
                handle.path = new_path;
            }
            return Ok(());
        }
        let entry = self.backend.rename(&record.metadata.remote_id, &new_path)?;
        self.db.upsert_metadata(&entry)?;
        hydrate_pending_pins(&self.db, &self.cache_dir, self.backend.as_ref())?;
        for updated in std::iter::once(
            self.db
                .get_by_remote_id(&entry.remote_id)?
                .ok_or_else(|| anyhow::anyhow!("renamed record disappeared"))?,
        )
        .chain(self.db.list_descendants(&entry.path)?)
        {
            if let Some(updated_ino) = self.inodes.ino_for_remote_id(&updated.metadata.remote_id) {
                self.inodes.replace_ino_record(updated_ino, updated);
            }
        }
        Ok(())
    }
}

impl<B: CloudBackend> Filesystem for TwoDriveFs<B> {
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
        _mode: Option<u32>,
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
        if let Some(size) = size {
            let Some(handle) = fh.and_then(|fh| self.write_handles.get(&fh)) else {
                reply.error(libc::EBADF);
                return;
            };
            match OpenOptions::new()
                .write(true)
                .open(&handle.cache_path)
                .and_then(|file| file.set_len(size))
            {
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

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let Some(path) = self.inodes.path_for_ino(ino) else {
            reply.error(libc::ENOENT);
            return;
        };
        let path = path.to_string();
        self.refresh_child_records(&path);

        let mut entries = vec![
            (ino, FileType::Directory, ".".to_string()),
            (
                self.inodes.parent_ino(&path).unwrap_or(ROOT_INO),
                FileType::Directory,
                "..".to_string(),
            ),
        ];

        for (child_ino, child) in self.inodes.children_for_ino(ino) {
            entries.push((
                child_ino,
                file_type(&child.metadata),
                child.metadata.name.clone(),
            ));
        }

        for (index, (entry_ino, kind, name)) in
            entries.into_iter().enumerate().skip(offset as usize)
        {
            if reply.add(entry_ino, (index + 1) as i64, kind, name) {
                break;
            }
        }
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

                if should_defer_hydration(req, &record) {
                    reply.error(libc::ENODATA);
                    return;
                }

                match self.ensure_cached(&record) {
                    Ok(cache_path) => {
                        let fh = self.open_read_handle(ino, cache_path);
                        reply.opened(fh, 0);
                    }
                    Err(err) => {
                        eprintln!("twodrive open hydrate error: {err:#}");
                        reply.error(libc::EIO);
                    }
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
        let result = self.upload_handle(fh);
        self.write_handles.remove(&fh);
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
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        match self.create_directory(parent, name) {
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
        _mode: u32,
        _umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        match self.create_upload(parent, name, flags) {
            Ok((_ino, fh, attr)) => reply.created(&TTL, &attr, 0, fh, 0),
            Err(err) => {
                eprintln!("twodrive create upload error: {err:#}");
                reply.error(libc::EIO);
            }
        }
    }
}

#[derive(Debug)]
struct ReadHandle {
    ino: u64,
    cache_path: PathBuf,
    unlinked: bool,
}

#[derive(Debug)]
struct WriteHandle {
    ino: u64,
    path: String,
    record_remote_id: String,
    cache_path: PathBuf,
    activity: Option<ActivityGuard>,
    uploaded: bool,
    created_new_record: bool,
    unlinked: bool,
    base_etag: Option<String>,
}

#[derive(Debug)]
struct ActivityGuard {
    path: PathBuf,
    id: String,
    last_bytes_done: u64,
    last_update: SystemTime,
    finished: bool,
}

impl ActivityGuard {
    fn start(
        cache_dir: &Path,
        kind: &str,
        cloud_path: &str,
        name: &str,
        bytes_total: Option<u64>,
    ) -> Self {
        let path = activity_file_path(cache_dir);
        let id = format!("{kind}-{}", unique_suffix());
        let mut guard = Self {
            path,
            id,
            last_bytes_done: 0,
            last_update: SystemTime::now(),
            finished: false,
        };
        let _ = guard.write_entry(kind, cloud_path, name, 0, bytes_total);
        guard
    }

    fn set_progress(&mut self, bytes_done: u64, bytes_total: Option<u64>) {
        let now_time = SystemTime::now();
        let enough_bytes = bytes_done >= self.last_bytes_done.saturating_add(1024 * 1024);
        let enough_time = now_time
            .duration_since(self.last_update)
            .unwrap_or_default()
            >= Duration::from_millis(750);
        let complete = bytes_total.is_some_and(|total| bytes_done >= total);
        if !enough_bytes && !enough_time && !complete {
            return;
        }
        self.last_bytes_done = bytes_done;
        self.last_update = now_time;
        let _ = update_activity_file(&self.path, |active| {
            let now = current_unix_i64();
            if let Some(item) = active
                .iter_mut()
                .find(|item| item.get("id").and_then(serde_json::Value::as_str) == Some(&self.id))
            {
                item["bytes_done"] = serde_json::json!(bytes_done);
                if let Some(total) = bytes_total {
                    item["bytes_total"] = serde_json::json!(total);
                }
                item["updated_unix"] = serde_json::json!(now);
            }
        });
    }

    fn write_entry(
        &mut self,
        kind: &str,
        cloud_path: &str,
        name: &str,
        bytes_done: u64,
        bytes_total: Option<u64>,
    ) -> anyhow::Result<()> {
        let id = self.id.clone();
        let item = serde_json::json!({
            "id": id,
            "kind": kind,
            "path": cloud_path,
            "name": name,
            "bytes_done": bytes_done,
            "bytes_total": bytes_total,
            "started_unix": current_unix_i64(),
            "updated_unix": current_unix_i64(),
        });
        update_activity_file(&self.path, |active| {
            active.retain(|entry| {
                entry.get("id").and_then(serde_json::Value::as_str) != Some(&self.id)
            });
            active.push(item);
        })
    }

    fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let _ = update_activity_file(&self.path, |active| {
            active.retain(|entry| {
                entry.get("id").and_then(serde_json::Value::as_str) != Some(&self.id)
            });
        });
    }
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        self.finish();
    }
}

#[derive(Debug, Clone)]
struct InodeTable {
    path_by_ino: HashMap<u64, String>,
    ino_by_path: HashMap<String, u64>,
    record_by_ino: HashMap<u64, FileRecord>,
    children_by_parent_ino: HashMap<u64, Vec<(u64, FileRecord)>>,
}

impl InodeTable {
    fn new(records: &[FileRecord]) -> Self {
        let mut path_by_ino = HashMap::from([(ROOT_INO, "/".to_string())]);
        let mut ino_by_path = HashMap::from([("/".to_string(), ROOT_INO)]);
        let mut record_by_ino = HashMap::new();
        let mut sorted_records = records.to_vec();
        sorted_records.sort_by(|left, right| left.metadata.path.cmp(&right.metadata.path));

        for (next_ino, record) in (ROOT_INO + 1..).zip(sorted_records) {
            let path = record.metadata.path.clone();
            path_by_ino.insert(next_ino, path.clone());
            ino_by_path.insert(path, next_ino);
            record_by_ino.insert(next_ino, record);
        }

        let mut children_by_parent_ino: HashMap<u64, Vec<(u64, FileRecord)>> = HashMap::new();
        for (child_ino, record) in &record_by_ino {
            let parent_ino = parent_ino_for_path(&ino_by_path, &record.metadata.path);
            children_by_parent_ino
                .entry(parent_ino)
                .or_default()
                .push((*child_ino, record.clone()));
        }
        for children in children_by_parent_ino.values_mut() {
            children.sort_by(|(_, left), (_, right)| {
                right
                    .metadata
                    .is_dir
                    .cmp(&left.metadata.is_dir)
                    .then_with(|| {
                        left.metadata
                            .name
                            .to_lowercase()
                            .cmp(&right.metadata.name.to_lowercase())
                    })
                    .then_with(|| left.metadata.name.cmp(&right.metadata.name))
            });
        }

        Self {
            path_by_ino,
            ino_by_path,
            record_by_ino,
            children_by_parent_ino,
        }
    }

    fn path_for_ino(&self, ino: u64) -> Option<&str> {
        self.path_by_ino.get(&ino).map(String::as_str)
    }

    fn ino_for_path(&self, path: &str) -> Option<u64> {
        self.ino_by_path.get(&normalize_cloud_path(path)).copied()
    }

    fn ino_for_remote_id(&self, remote_id: &str) -> Option<u64> {
        self.record_by_ino
            .iter()
            .find_map(|(ino, record)| (record.metadata.remote_id == remote_id).then_some(*ino))
    }

    fn record_for_ino(&self, ino: u64) -> Option<&FileRecord> {
        self.record_by_ino.get(&ino)
    }

    fn insert_or_update(&mut self, record: FileRecord) -> u64 {
        if let Some(ino) = self.ino_for_path(&record.metadata.path) {
            self.replace_ino_record(ino, record);
            return ino;
        }

        let ino = self
            .path_by_ino
            .keys()
            .copied()
            .max()
            .unwrap_or(ROOT_INO)
            .saturating_add(1);
        self.path_by_ino.insert(ino, record.metadata.path.clone());
        self.ino_by_path.insert(record.metadata.path.clone(), ino);
        self.record_by_ino.insert(ino, record.clone());
        let parent_ino = parent_ino_for_path(&self.ino_by_path, &record.metadata.path);
        self.children_by_parent_ino
            .entry(parent_ino)
            .or_default()
            .push((ino, record));
        self.sort_children(parent_ino);
        ino
    }

    fn replace_ino_record(&mut self, ino: u64, record: FileRecord) {
        let previous_path = self.path_by_ino.insert(ino, record.metadata.path.clone());
        if let Some(previous_path) = previous_path {
            self.ino_by_path.remove(&previous_path);
        }
        self.ino_by_path.insert(record.metadata.path.clone(), ino);
        self.record_by_ino.insert(ino, record.clone());

        for children in self.children_by_parent_ino.values_mut() {
            children.retain(|(child_ino, _)| *child_ino != ino);
        }
        let parent_ino = parent_ino_for_path(&self.ino_by_path, &record.metadata.path);
        self.children_by_parent_ino
            .entry(parent_ino)
            .or_default()
            .push((ino, record));
        self.sort_children(parent_ino);
    }

    fn set_size(&mut self, ino: u64, size: u64) {
        if let Some(record) = self.record_by_ino.get_mut(&ino) {
            record.metadata.size = record.metadata.size.max(size);
        }
        for children in self.children_by_parent_ino.values_mut() {
            for (child_ino, child_record) in children {
                if *child_ino == ino {
                    child_record.metadata.size = child_record.metadata.size.max(size);
                    return;
                }
            }
        }
    }

    fn replace_size(&mut self, ino: u64, size: u64) {
        if let Some(record) = self.record_by_ino.get_mut(&ino) {
            record.metadata.size = size;
        }
        for children in self.children_by_parent_ino.values_mut() {
            for (child_ino, child_record) in children {
                if *child_ino == ino {
                    child_record.metadata.size = size;
                    return;
                }
            }
        }
    }

    fn remove_ino(&mut self, ino: u64) {
        if let Some(path) = self.path_by_ino.remove(&ino) {
            self.ino_by_path.remove(&path);
        }
        self.record_by_ino.remove(&ino);
        self.children_by_parent_ino.remove(&ino);
        for children in self.children_by_parent_ino.values_mut() {
            children.retain(|(child_ino, _)| *child_ino != ino);
        }
    }

    fn children_for_ino(&self, ino: u64) -> Vec<(u64, FileRecord)> {
        self.children_by_parent_ino
            .get(&ino)
            .cloned()
            .unwrap_or_default()
    }

    fn sort_children(&mut self, parent_ino: u64) {
        if let Some(children) = self.children_by_parent_ino.get_mut(&parent_ino) {
            sort_child_records(children);
        }
    }

    fn parent_ino(&self, path: &str) -> Option<u64> {
        let path = normalize_cloud_path(path);
        if path == "/" {
            return Some(ROOT_INO);
        }

        let parent = match path.rfind('/') {
            Some(0) | None => "/".to_string(),
            Some(index) => path[..index].to_string(),
        };
        self.ino_for_path(&parent)
    }
}

fn parent_ino_for_path(ino_by_path: &HashMap<String, u64>, path: &str) -> u64 {
    let path = normalize_cloud_path(path);
    if path == "/" {
        return ROOT_INO;
    }

    let parent = match path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(index) => path[..index].to_string(),
    };
    ino_by_path.get(&parent).copied().unwrap_or(ROOT_INO)
}

fn attr_for_record(ino: u64, record: &FileRecord) -> FileAttr {
    let kind = file_type(&record.metadata);
    let size = if record.metadata.is_dir {
        0
    } else {
        record.metadata.size
    };
    let time = unix_time(record.metadata.modified_unix);

    FileAttr {
        ino,
        size,
        blocks: size.div_ceil(512),
        atime: time,
        mtime: time,
        ctime: time,
        crtime: time,
        kind,
        perm: if record.metadata.is_dir { 0o755 } else { 0o644 },
        nlink: if record.metadata.is_dir { 2 } else { 1 },
        uid: current_uid(),
        gid: current_gid(),
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}

fn root_attr() -> FileAttr {
    let time = SystemTime::now();
    FileAttr {
        ino: ROOT_INO,
        size: 0,
        blocks: 0,
        atime: time,
        mtime: time,
        ctime: time,
        crtime: time,
        kind: FileType::Directory,
        perm: 0o755,
        nlink: 2,
        uid: current_uid(),
        gid: current_gid(),
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}

fn file_type(entry: &MetadataEntry) -> FileType {
    if entry.is_dir {
        FileType::Directory
    } else {
        FileType::RegularFile
    }
}

fn unix_time(seconds: i64) -> SystemTime {
    if seconds >= 0 {
        UNIX_EPOCH + Duration::from_secs(seconds as u64)
    } else {
        UNIX_EPOCH
    }
}

fn read_slice(path: &Path, offset: u64, size: u32) -> anyhow::Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut data = vec![0; size as usize];
    let read = file.read(&mut data)?;
    data.truncate(read);
    Ok(data)
}

fn write_slice(path: &Path, offset: u64, data: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(data)?;
    Ok(())
}

fn activity_file_path(cache_dir: &Path) -> PathBuf {
    cache_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cache_dir.to_path_buf())
        .join("activity.json")
}

fn activity_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn update_activity_file(
    path: &Path,
    update: impl FnOnce(&mut Vec<serde_json::Value>),
) -> anyhow::Result<()> {
    let _guard = activity_lock()
        .lock()
        .map_err(|_| anyhow::anyhow!("activity lock is poisoned"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let now = current_unix_i64();
    let existing = fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .unwrap_or_else(|| serde_json::json!({"updated_unix": now, "active": []}));
    let mut active = existing
        .get("active")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();

    active.retain(|entry| {
        entry
            .get("updated_unix")
            .and_then(serde_json::Value::as_i64)
            .map(|updated| now.saturating_sub(updated) < 6 * 60 * 60)
            .unwrap_or(false)
    });
    update(&mut active);

    let snapshot = serde_json::json!({
        "updated_unix": now,
        "active": active,
    });
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, serde_json::to_vec_pretty(&snapshot)?)?;
    fs::rename(tmp_path, path)?;
    Ok(())
}

fn clear_activity_file(cache_dir: &Path) -> anyhow::Result<()> {
    let path = activity_file_path(cache_dir);
    update_activity_file(&path, |active| active.clear())
}

fn sort_child_records(children: &mut [(u64, FileRecord)]) {
    children.sort_by(|(_, left), (_, right)| {
        right
            .metadata
            .is_dir
            .cmp(&left.metadata.is_dir)
            .then_with(|| {
                left.metadata
                    .name
                    .to_lowercase()
                    .cmp(&right.metadata.name.to_lowercase())
            })
            .then_with(|| left.metadata.name.cmp(&right.metadata.name))
    });
}

fn should_defer_hydration(req: &Request<'_>, record: &FileRecord) -> bool {
    !has_existing_cache(record) && is_thumbnail_or_indexer_request(req)
}

fn has_existing_cache(record: &FileRecord) -> bool {
    matches!(
        record.state,
        FileState::Cached | FileState::Pinned | FileState::Dirty | FileState::Uploading
    ) && record.cache_path.as_deref().is_some_and(Path::exists)
}

fn record_upload_etag(record: &FileRecord) -> Option<String> {
    if record.metadata.remote_id.starts_with("local-upload-") || record.metadata.etag.is_empty() {
        None
    } else {
        Some(record.metadata.etag.clone())
    }
}

fn record_upload_remote_id(record: &FileRecord) -> Option<&str> {
    (!record.metadata.remote_id.starts_with("local-upload-"))
        .then_some(record.metadata.remote_id.as_str())
}

fn preserve_conflict_copy<B: CloudBackend>(
    db: &Database,
    backend: &B,
    original: &FileRecord,
    cache_path: &Path,
    on_progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
) -> anyhow::Result<MetadataEntry> {
    let conflict_path = conflict_copy_path(original, cache_path);
    let conflict =
        backend.upload_file_with_version(&conflict_path, cache_path, None, None, on_progress)?;
    let latest_original = backend
        .get_metadata(&original.metadata.remote_id)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "original remote item disappeared while preserving conflict {}",
                original.metadata.path
            )
        })?;
    db.upsert_metadata(&conflict)?;
    db.mark_cached(&conflict.remote_id, cache_path)?;
    db.upsert_metadata(&latest_original)?;
    db.mark_online_only(&original.metadata.remote_id)?;
    Ok(conflict)
}

fn conflict_copy_path(record: &FileRecord, cache_path: &Path) -> String {
    let modified = cache_path
        .metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(record.metadata.modified_unix.max(0) as u64);
    let fingerprint = stable_path_fingerprint(&record.metadata.remote_id);
    let suffix = format!(" (TwoDrive conflict {modified}-{fingerprint:08x})");
    let name = &record.metadata.name;
    let extension_start = name.rfind('.').filter(|index| *index > 0);
    let (stem, extension) = extension_start
        .map(|index| (&name[..index], &name[index..]))
        .unwrap_or((name.as_str(), ""));
    let extension = truncate_utf8(extension, 32);
    let max_stem_bytes = 240_usize
        .saturating_sub(suffix.len())
        .saturating_sub(extension.len());
    let stem = truncate_utf8(stem, max_stem_bytes);
    join_cloud_path(
        &record.metadata.parent_path,
        &format!("{stem}{suffix}{extension}"),
    )
}

fn stable_path_fingerprint(value: &str) -> u32 {
    value.as_bytes().iter().fold(0x811c9dc5_u32, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(0x01000193)
    })
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn is_conflict_error(err: &anyhow::Error) -> bool {
    let text = err.to_string();
    text.contains("HTTP 412") || text.contains("Precondition Failed")
}

fn is_thumbnail_or_indexer_request(req: &Request<'_>) -> bool {
    let process = request_process_text(req).to_lowercase();
    if process.is_empty() {
        return false;
    }

    const EXACT_HINTS: &[&str] = &[
        "thumbnail",
        "thumbnailer",
        "tracker-extract",
        "tracker-miner",
        "tracker3",
        "localsearch",
        "evince-thumbnailer",
        "ffmpegthumbnailer",
        "totem-video-thumbnailer",
        "gdk-pixbuf-thumbnailer",
        "gnome-epub-thumbnailer",
    ];
    if EXACT_HINTS.iter().any(|hint| process.contains(hint)) {
        return true;
    }

    (process.contains("soffice") || process.contains("libreoffice"))
        && (process.contains("--headless")
            || process.contains("--convert-to")
            || process.contains("thumbnail"))
}

fn request_process_text(req: &Request<'_>) -> String {
    let pid = req.pid();
    let proc_dir = PathBuf::from(format!("/proc/{pid}"));
    let comm = fs::read_to_string(proc_dir.join("comm")).unwrap_or_default();
    let cmdline = fs::read(proc_dir.join("cmdline"))
        .map(|bytes| String::from_utf8_lossy(&bytes).replace('\0', " "))
        .unwrap_or_default();
    format!("{comm} {cmdline}")
}

fn sanitize_cache_name(remote_id: &str) -> String {
    remote_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

fn current_gid() -> u32 {
    unsafe { libc::getgid() }
}

fn current_unix_i64() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{nanos}-{}", unsafe { libc::getpid() })
}

fn cleanup_stale_mountpoint(mount_dir: &Path) {
    match fs::metadata(mount_dir) {
        Ok(_) => {}
        Err(err) if err.raw_os_error() == Some(libc::ENOTCONN) => {
            let _ = Command::new("fusermount3")
                .arg("-uz")
                .arg(mount_dir)
                .status();
        }
        Err(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use twodrive_backend::DeltaResult;

    #[derive(Debug)]
    struct FailingBackend {
        inner: MockBackend,
        fail_upload: bool,
        fail_delete: bool,
    }

    impl FailingBackend {
        fn new(fail_upload: bool, fail_delete: bool) -> Self {
            Self {
                inner: MockBackend::new(),
                fail_upload,
                fail_delete,
            }
        }
    }

    impl CloudBackend for FailingBackend {
        fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
            self.inner.list_all()
        }

        fn list_delta(&self, delta_link: Option<&str>) -> anyhow::Result<DeltaResult> {
            self.inner.list_delta(delta_link)
        }

        fn download(&self, remote_id: &str) -> anyhow::Result<Vec<u8>> {
            self.inner.download(remote_id)
        }

        fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry> {
            if self.fail_upload {
                anyhow::bail!("simulated upload outage");
            }
            self.inner.upload(path, content)
        }

        fn upload_with_etag(
            &self,
            path: &str,
            content: Vec<u8>,
            if_match: Option<&str>,
        ) -> anyhow::Result<MetadataEntry> {
            if self.fail_upload {
                anyhow::bail!("simulated upload outage");
            }
            self.inner.upload_with_etag(path, content, if_match)
        }

        fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
            self.inner.create_folder(path)
        }

        fn rename(&self, remote_id: &str, new_path: &str) -> anyhow::Result<MetadataEntry> {
            self.inner.rename(remote_id, new_path)
        }

        fn delete(&self, remote_id: &str) -> anyhow::Result<()> {
            if self.fail_delete {
                anyhow::bail!("simulated delete outage");
            }
            self.inner.delete(remote_id)
        }
    }

    #[derive(Debug)]
    struct ConflictBackend {
        inner: MockBackend,
    }

    impl ConflictBackend {
        fn new() -> Self {
            Self {
                inner: MockBackend::new(),
            }
        }
    }

    impl CloudBackend for ConflictBackend {
        fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
            self.inner.list_all()
        }

        fn get_metadata(&self, remote_id: &str) -> anyhow::Result<Option<MetadataEntry>> {
            if remote_id == "file-readme" {
                return Ok(Some(MetadataEntry::new_file(
                    "file-readme",
                    "/README-cloud.txt",
                    42,
                    1_719_000_100,
                    "etag-cloud-winner",
                )));
            }
            self.inner.get_metadata(remote_id)
        }

        fn download(&self, remote_id: &str) -> anyhow::Result<Vec<u8>> {
            self.inner.download(remote_id)
        }

        fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry> {
            self.inner.upload(path, content)
        }

        fn upload_with_etag(
            &self,
            path: &str,
            content: Vec<u8>,
            if_match: Option<&str>,
        ) -> anyhow::Result<MetadataEntry> {
            if if_match.is_some() {
                anyhow::bail!("Graph request failed with HTTP 412 Precondition Failed");
            }
            self.inner.upload(path, content)
        }

        fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
            self.inner.create_folder(path)
        }

        fn rename(&self, remote_id: &str, new_path: &str) -> anyhow::Result<MetadataEntry> {
            self.inner.rename(remote_id, new_path)
        }

        fn delete(&self, remote_id: &str) -> anyhow::Result<()> {
            self.inner.delete(remote_id)
        }
    }

    struct TestFs {
        fs: TwoDriveFs<MockBackend>,
        root: PathBuf,
    }

    impl TestFs {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "twodrive-fs-{name}-{}-{}",
                std::process::id(),
                current_unix_i64()
            ));
            fs::create_dir_all(&root).unwrap();
            let db = Database::new(root.join("test.sqlite3"));
            db.init().unwrap();
            let backend = MockBackend::new();
            sync_metadata(&db, &backend).unwrap();
            let fs =
                TwoDriveFs::new(db, root.join("cache"), backend).expect("create test filesystem");
            Self { fs, root }
        }
    }

    impl Drop for TestFs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "twodrive-fs-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn local_fsync_preserves_random_offset_writes_before_upload() {
        let mut test = TestFs::new("fsync");
        let (ino, fh, _) = test
            .fs
            .create_upload(ROOT_INO, OsStr::new("random.bin"), libc::O_CREAT)
            .unwrap();
        let cache_path = test.fs.write_handles[&fh].cache_path.clone();

        write_slice(&cache_path, 4, b"EFGH").unwrap();
        write_slice(&cache_path, 0, b"ABCD").unwrap();
        test.fs.inodes.set_size(ino, 8);
        test.fs.sync_handle(fh, false).unwrap();

        assert_eq!(fs::read(cache_path).unwrap(), b"ABCDEFGH");
        let record = test
            .fs
            .db
            .get_by_remote_id(&test.fs.write_handles[&fh].record_remote_id)
            .unwrap()
            .unwrap();
        assert_eq!(record.state, FileState::Dirty);
        assert_eq!(
            hydrate_record(
                &test.fs.db,
                &test.fs.cache_dir,
                test.fs.backend.as_ref(),
                &record
            )
            .unwrap(),
            test.fs.write_handles[&fh].cache_path
        );
    }

    #[test]
    fn editor_style_temp_file_can_replace_an_existing_remote_file() {
        let mut test = TestFs::new("atomic-replace");
        let (ino, fh, _) = test
            .fs
            .create_upload(
                ROOT_INO,
                OsStr::new(".README-cloud.txt.tmp"),
                libc::O_CREAT | libc::O_EXCL,
            )
            .unwrap();
        let cache_path = test.fs.write_handles[&fh].cache_path.clone();
        write_slice(&cache_path, 0, b"new atomically saved content\n").unwrap();
        test.fs.inodes.set_size(ino, 29);
        test.fs.sync_handle(fh, false).unwrap();
        test.fs.upload_handle(fh).unwrap();

        test.fs
            .rename_record(
                ROOT_INO,
                OsStr::new(".README-cloud.txt.tmp"),
                ROOT_INO,
                OsStr::new("README-cloud.txt"),
            )
            .unwrap();

        let record = test
            .fs
            .db
            .get_by_path("/README-cloud.txt")
            .unwrap()
            .unwrap();
        assert_eq!(
            test.fs
                .backend
                .download(&record.metadata.remote_id)
                .unwrap(),
            b"new atomically saved content\n"
        );
        assert!(
            test.fs
                .db
                .get_by_path("/.README-cloud.txt.tmp")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn read_only_handle_survives_unlink_until_the_last_release() {
        let mut test = TestFs::new("read-open-unlink");
        let record = test
            .fs
            .db
            .get_by_path("/README-cloud.txt")
            .unwrap()
            .unwrap();
        let ino = test.fs.inodes.ino_for_path("/README-cloud.txt").unwrap();
        let cache_path = test.fs.ensure_cached(&record).unwrap();
        let fh = test.fs.open_read_handle(ino, cache_path.clone());

        assert!(
            test.fs
                .unlink_record_with_open_handles(ino, &record)
                .unwrap()
        );
        assert!(
            test.fs
                .db
                .get_by_path("/README-cloud.txt")
                .unwrap()
                .is_none()
        );
        assert_eq!(test.fs.read_open_handle(fh, 0, 7).unwrap(), b"Welcome");
        assert!(cache_path.exists());

        test.fs.release_read_handle(fh).unwrap();
        assert!(!cache_path.exists());
    }

    #[test]
    fn dirty_cache_is_replayed_after_a_simulated_restart() {
        let mut test = TestFs::new("dirty-recovery");
        let (_ino, fh, _) = test
            .fs
            .create_upload(ROOT_INO, OsStr::new("recover-me.txt"), libc::O_CREAT)
            .unwrap();
        let cache_path = test.fs.write_handles[&fh].cache_path.clone();
        write_slice(&cache_path, 0, b"durable before restart").unwrap();
        test.fs.sync_handle(fh, false).unwrap();

        assert_eq!(
            recover_dirty_uploads(&test.fs.db, test.fs.backend.as_ref()).unwrap(),
            1
        );
        let recovered = test.fs.db.get_by_path("/recover-me.txt").unwrap().unwrap();
        assert_eq!(recovered.state, FileState::Cached);
        assert_eq!(
            test.fs
                .backend
                .download(&recovered.metadata.remote_id)
                .unwrap(),
            b"durable before restart"
        );
    }

    #[test]
    fn uploading_state_is_replayed_after_a_simulated_restart() {
        let test = TestFs::new("uploading-recovery");
        let cache_path = test.root.join("uploading.cache");
        fs::write(&cache_path, b"uploading before restart").unwrap();
        let metadata = MetadataEntry::new_file("uploading-id", "/uploading.txt", 0, 1, "etag");
        test.fs.db.upsert_metadata(&metadata).unwrap();
        test.fs.db.mark_cached("uploading-id", &cache_path).unwrap();
        test.fs
            .db
            .mark_state("uploading-id", FileState::Uploading)
            .unwrap();

        assert_eq!(
            recover_dirty_uploads(&test.fs.db, test.fs.backend.as_ref()).unwrap(),
            1
        );
        let recovered = test.fs.db.get_by_path("/uploading.txt").unwrap().unwrap();
        assert_eq!(recovered.state, FileState::Cached);
        assert_eq!(
            test.fs
                .backend
                .download(&recovered.metadata.remote_id)
                .unwrap(),
            b"uploading before restart"
        );
    }

    #[test]
    fn failed_upload_on_release_leaves_a_dirty_retry_record() {
        let root = test_root("failed-upload-release");
        fs::create_dir_all(&root).unwrap();
        let db = Database::new(root.join("test.sqlite3"));
        db.init().unwrap();
        let backend = FailingBackend::new(true, false);
        sync_metadata(&db, &backend).unwrap();
        let mut fuse =
            TwoDriveFs::new(db, root.join("cache"), backend).expect("create failing test fs");

        let (_ino, fh, _) = fuse
            .create_upload(ROOT_INO, OsStr::new("offline.txt"), libc::O_CREAT)
            .unwrap();
        let cache_path = fuse.write_handles[&fh].cache_path.clone();
        write_slice(&cache_path, 0, b"kept dirty after failed upload").unwrap();
        fuse.sync_handle(fh, false).unwrap();

        assert!(fuse.upload_handle(fh).is_err());
        let record = fuse.db.get_by_path("/offline.txt").unwrap().unwrap();
        assert_eq!(record.state, FileState::Dirty);
        assert_eq!(record.cache_path.as_deref(), Some(cache_path.as_path()));
        assert_eq!(
            fs::read(cache_path).unwrap(),
            b"kept dirty after failed upload"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_etag_upload_preserves_local_content_as_a_conflict_copy() {
        let root = test_root("conflict-upload");
        fs::create_dir_all(&root).unwrap();
        let db = Database::new(root.join("test.sqlite3"));
        db.init().unwrap();
        let backend = ConflictBackend::new();
        sync_metadata(&db, &backend).unwrap();
        let mut fuse =
            TwoDriveFs::new(db, root.join("cache"), backend).expect("create conflict test fs");

        let (_ino, fh, _) = fuse
            .create_overwrite_upload(
                fuse.db.get_by_path("/README-cloud.txt").unwrap().unwrap(),
                true,
            )
            .unwrap();
        let cache_path = fuse.write_handles[&fh].cache_path.clone();
        write_slice(&cache_path, 0, b"local edit that lost the race").unwrap();
        fuse.sync_handle(fh, false).unwrap();

        fuse.upload_handle(fh).unwrap();
        let original = fuse.db.get_by_path("/README-cloud.txt").unwrap().unwrap();
        assert_eq!(original.state, FileState::OnlineOnly);
        assert!(original.cache_path.is_none());
        assert_eq!(original.metadata.size, 42);
        assert_eq!(original.metadata.etag, "etag-cloud-winner");

        let conflict = fuse
            .db
            .all_records()
            .unwrap()
            .into_iter()
            .find(|record| record.metadata.name.contains("TwoDrive conflict"))
            .expect("conflict copy metadata");
        assert_eq!(conflict.state, FileState::Cached);
        assert_eq!(
            fs::read(conflict.cache_path.unwrap()).unwrap(),
            b"local edit that lost the race"
        );
        assert_eq!(
            fuse.backend.download(&conflict.metadata.remote_id).unwrap(),
            b"local edit that lost the race"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn conflict_state_is_preserved_as_a_copy_after_restart() {
        let root = test_root("conflict-recovery");
        fs::create_dir_all(&root).unwrap();
        let db = Database::new(root.join("test.sqlite3"));
        db.init().unwrap();
        let backend = ConflictBackend::new();
        sync_metadata(&db, &backend).unwrap();
        let mut fuse =
            TwoDriveFs::new(db, root.join("cache"), backend).expect("create conflict test fs");

        let (_ino, fh, _) = fuse
            .create_overwrite_upload(
                fuse.db.get_by_path("/README-cloud.txt").unwrap().unwrap(),
                true,
            )
            .unwrap();
        let cache_path = fuse.write_handles[&fh].cache_path.clone();
        write_slice(&cache_path, 0, b"conflict durable across restart").unwrap();
        fuse.sync_handle(fh, false).unwrap();
        fuse.db
            .mark_state("file-readme", FileState::Conflict)
            .unwrap();

        assert_eq!(
            recover_dirty_uploads(&fuse.db, fuse.backend.as_ref()).unwrap(),
            1
        );
        assert_eq!(
            recover_dirty_uploads(&fuse.db, fuse.backend.as_ref()).unwrap(),
            0
        );
        let original = fuse.db.get_by_path("/README-cloud.txt").unwrap().unwrap();
        assert_eq!(original.state, FileState::OnlineOnly);
        let conflict = fuse
            .db
            .all_records()
            .unwrap()
            .into_iter()
            .find(|record| record.metadata.name.contains("TwoDrive conflict"))
            .unwrap();
        assert_eq!(
            fuse.backend.download(&conflict.metadata.remote_id).unwrap(),
            b"conflict durable across restart"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_backend_delete_is_hidden_locally_and_retried_later() {
        let root = test_root("pending-delete");
        fs::create_dir_all(&root).unwrap();
        let db = Database::new(root.join("test.sqlite3"));
        db.init().unwrap();
        let backend = FailingBackend::new(false, true);
        sync_metadata(&db, &backend).unwrap();
        let mut fuse =
            TwoDriveFs::new(db, root.join("cache"), backend).expect("create failing delete fs");
        let ino = fuse.inodes.ino_for_path("/README-cloud.txt").unwrap();
        let record = fuse.record_for_ino(ino).unwrap();

        fuse.delete_record(ino, &record).unwrap();
        assert!(fuse.db.get_by_path("/README-cloud.txt").unwrap().is_none());
        let pending = fuse.db.pending_deletes().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].remote_id, "file-readme");
        assert!(fuse.inodes.ino_for_path("/README-cloud.txt").is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pending_delete_is_replayed_after_a_simulated_restart() {
        let test = TestFs::new("pending-delete-recovery");
        let record = test
            .fs
            .db
            .get_by_path("/README-cloud.txt")
            .unwrap()
            .unwrap();
        test.fs.db.queue_pending_delete(&record).unwrap();
        assert_eq!(test.fs.db.pending_deletes().unwrap().len(), 1);

        assert_eq!(
            recover_pending_deletes(&test.fs.db, test.fs.backend.as_ref()).unwrap(),
            1
        );
        assert!(test.fs.db.pending_deletes().unwrap().is_empty());
        assert!(test.fs.backend.download("file-readme").is_err());
    }

    #[test]
    fn local_move_into_a_pinned_directory_hydrates_the_file() {
        let mut test = TestFs::new("move-into-pin");
        let courses = test.fs.db.get_by_path("/Courses").unwrap().unwrap();
        test.fs
            .db
            .set_explicit_pin(&courses.metadata.remote_id, true)
            .unwrap();
        let root_ino = ROOT_INO;
        let documents_ino = test.fs.inodes.ino_for_path("/Documents").unwrap();
        let courses_ino = test.fs.inodes.ino_for_path("/Courses").unwrap();

        test.fs
            .rename_record(
                documents_ino,
                OsStr::new("twodrive-notes.txt"),
                courses_ino,
                OsStr::new("twodrive-notes.txt"),
            )
            .unwrap();

        let moved = test
            .fs
            .db
            .get_by_path("/Courses/twodrive-notes.txt")
            .unwrap()
            .unwrap();
        assert!(moved.effective_pinned());
        assert_eq!(moved.state, FileState::Pinned);
        assert!(moved.cache_path.as_deref().is_some_and(Path::exists));
        assert_eq!(
            test.fs.inodes.parent_ino(&moved.metadata.path),
            Some(courses_ino)
        );
        assert_eq!(test.fs.inodes.parent_ino("/Courses"), Some(root_ino));
    }
}
