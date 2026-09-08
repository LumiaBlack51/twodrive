use fuser::{
    FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory,
    ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request, TimeOrNow,
};
use std::collections::{HashMap, HashSet};
use std::ffi::{CString, OsStr};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use twodrive_backend::{CloudBackend, GraphBackend, MockBackend};
use twodrive_core::{
    AppPaths, Config, Database, FileRecord, FileState, MetadataEntry, PendingMetadataKind,
    PendingMetadataOperation, join_cloud_path, normalize_cloud_path,
};

const ROOT_INO: u64 = 1;
const TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy)]
struct FilesystemStats {
    blocks: u64,
    blocks_free: u64,
    blocks_available: u64,
    files: u64,
    files_free: u64,
    block_size: u32,
    name_length: u32,
    fragment_size: u32,
}

fn filesystem_stats(path: &Path) -> io::Result<FilesystemStats> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    let mut stats = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is a valid NUL-terminated C string and `stats` points to writable memory.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful `statvfs` call initialized the output structure.
    let stats = unsafe { stats.assume_init() };
    let block_size = u32::try_from(stats.f_bsize).unwrap_or(u32::MAX);
    let fragment_size = if stats.f_frsize == 0 {
        block_size
    } else {
        u32::try_from(stats.f_frsize).unwrap_or(u32::MAX)
    };
    Ok(FilesystemStats {
        blocks: stats.f_blocks,
        blocks_free: stats.f_bfree,
        blocks_available: stats.f_bavail,
        files: stats.f_files,
        files_free: stats.f_ffree,
        block_size,
        name_length: u32::try_from(stats.f_namemax).unwrap_or(u32::MAX),
        fragment_size,
    })
}

pub fn sync_metadata<B: CloudBackend>(db: &Database, backend: &B) -> anyhow::Result<usize> {
    let entries = backend.list_all()?;
    let count = entries.len();
    db.upsert_metadata_batch(&entries)?;
    Ok(count)
}

pub fn sync_delta_metadata<B: CloudBackend>(db: &Database, backend: &B) -> anyhow::Result<usize> {
    let delta = backend.list_delta(db.delta_link()?.as_deref())?;
    for remote_id in &delta.deleted_remote_ids {
        db.remove_pending_delete(remote_id)?;
        let record = db.get_by_cloud_remote_id(remote_id)?;
        if record.as_ref().is_some_and(|record| {
            matches!(
                record.state,
                FileState::Writing | FileState::Dirty | FileState::Uploading
            ) || db
                .pending_metadata_operation(&record.metadata.remote_id)
                .ok()
                .flatten()
                .is_some()
        }) {
            continue;
        }
        if let Some(record) = record {
            db.remove_by_remote_id(&record.metadata.remote_id)?;
        }
    }

    let count = delta.entries.len();
    db.upsert_metadata_batch(&delta.entries)?;
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
    recover_dirty_uploads_concurrent(db, backend, 1)
}

pub fn recover_dirty_uploads_concurrent<B: CloudBackend>(
    db: &Database,
    backend: &B,
    concurrency: usize,
) -> anyhow::Result<usize> {
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
    if records.is_empty() {
        return Ok(0);
    }
    let jobs = Mutex::new(records.into_iter());
    let recovered = AtomicUsize::new(0);
    let first_error = Mutex::new(None);
    thread::scope(|scope| {
        for _ in 0..concurrency.max(1) {
            scope.spawn(|| {
                loop {
                    let record = match jobs.lock() {
                        Ok(mut jobs) => jobs.next(),
                        Err(_) => {
                            if let Ok(mut error) = first_error.lock() {
                                error.get_or_insert_with(|| {
                                    anyhow::anyhow!("upload job lock is poisoned")
                                });
                            }
                            return;
                        }
                    };
                    let Some(record) = record else {
                        return;
                    };
                    match recover_dirty_record(db, backend, record, &mut |_, _| Ok(())) {
                        Ok(true) => {
                            recovered.fetch_add(1, Ordering::Relaxed);
                        }
                        Ok(false) => {}
                        Err(err) => {
                            if let Ok(mut error) = first_error.lock() {
                                error.get_or_insert(err);
                            }
                        }
                    }
                }
            });
        }
    });
    if let Some(err) = first_error
        .into_inner()
        .map_err(|_| anyhow::anyhow!("upload error lock is poisoned"))?
    {
        return Err(err);
    }
    Ok(recovered.load(Ordering::Relaxed))
}

fn recover_dirty_record<B: CloudBackend>(
    db: &Database,
    backend: &B,
    record: FileRecord,
    on_progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
) -> anyhow::Result<bool> {
    let Some(cache_path) = record.cache_path.clone().filter(|path| path.exists()) else {
        return Ok(false);
    };
    if record.state == FileState::Conflict {
        return match preserve_conflict_copy(db, backend, &record, &cache_path, on_progress) {
            Ok(_) => Ok(true),
            Err(err) => {
                eprintln!(
                    "twodrive: conflict copy remains queued for {}: {err:#}",
                    record.metadata.path
                );
                Ok(false)
            }
        };
    }

    db.mark_state(&record.metadata.remote_id, FileState::Uploading)?;
    match backend.upload_file_with_version(
        &record.metadata.path,
        &cache_path,
        record_upload_remote_id(&record),
        record_upload_etag(&record).as_deref(),
        on_progress,
    ) {
        Ok(uploaded) => {
            let _commit_guard = upload_commit_lock()
                .lock()
                .map_err(|_| anyhow::anyhow!("upload commit lock is poisoned"))?;
            let committed = match db.commit_uploaded(
                &record.metadata.remote_id,
                &record.metadata.path,
                &uploaded,
                &cache_path,
            ) {
                Ok(committed) => committed,
                Err(err) => {
                    db.queue_remote_delete(&uploaded.remote_id, &record.metadata.path)?;
                    return Err(err);
                }
            };
            if committed.cloud_remote_id.as_deref() != Some(uploaded.remote_id.as_str()) {
                db.queue_remote_delete(&uploaded.remote_id, &record.metadata.path)?;
            }
            if let Err(err) = db.finish_pending_releases() {
                eprintln!("twodrive: deferred release remains queued: {err:#}");
            }
            Ok(true)
        }
        Err(err) => {
            let _commit_guard = upload_commit_lock()
                .lock()
                .map_err(|_| anyhow::anyhow!("upload commit lock is poisoned"))?;
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
                    on_progress,
                ) {
                    Ok(_) => return Ok(true),
                    Err(conflict_err) => {
                        eprintln!(
                            "twodrive: conflict copy remains queued for {}: {conflict_err:#}",
                            record.metadata.path
                        );
                    }
                }
            } else {
                let state = if err.to_string().contains("unsupported OneDrive file name") {
                    FileState::Error
                } else {
                    FileState::Dirty
                };
                db.mark_state(&record.metadata.remote_id, state)?;
            }
            eprintln!(
                "twodrive: dirty upload remains queued for {}: {err:#}",
                record.metadata.path
            );
            Ok(false)
        }
    }
}

fn upload_commit_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub fn recover_pending_deletes<B: CloudBackend>(
    db: &Database,
    backend: &B,
) -> anyhow::Result<usize> {
    let pending = db.pending_deletes()?;
    let mut recovered = 0;
    for delete in pending {
        if delete.remote_id.starts_with("local-upload-") {
            if let Some(cache_path) = &delete.cache_path {
                match fs::remove_file(cache_path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err.into()),
                }
            }
            db.remove_pending_delete(&delete.remote_id)?;
            recovered += 1;
            continue;
        }
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

fn recover_pending_delete_id<B: CloudBackend>(
    db: &Database,
    backend: &B,
    cloud_remote_id: &str,
) -> anyhow::Result<bool> {
    let Some(delete) = db
        .pending_deletes()?
        .into_iter()
        .find(|delete| delete.remote_id == cloud_remote_id)
    else {
        return Ok(false);
    };
    if !delete.remote_id.starts_with("local-upload-") {
        backend.delete(&delete.remote_id)?;
    }
    if let Some(cache_path) = &delete.cache_path {
        match fs::remove_file(cache_path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }
    db.remove_pending_delete(&delete.remote_id)?;
    Ok(true)
}

pub fn recover_pending_metadata_operations<B: CloudBackend>(
    db: &Database,
    backend: &B,
) -> anyhow::Result<usize> {
    let operations = db.pending_metadata_operations()?;
    let mut recovered = 0;
    for operation in operations {
        match recover_pending_metadata_record(db, backend, &operation) {
            Ok(true) => recovered += 1,
            Ok(false) => {}
            Err(err) => eprintln!(
                "twodrive: pending metadata operation remains queued for {}: {err:#}",
                operation.path
            ),
        }
    }
    Ok(recovered)
}

fn recover_pending_metadata_record<B: CloudBackend>(
    db: &Database,
    backend: &B,
    operation: &PendingMetadataOperation,
) -> anyhow::Result<bool> {
    let Some(record) = db.get_by_remote_id(&operation.local_id)? else {
        return Ok(false);
    };
    let entry = match operation.kind {
        PendingMetadataKind::CreateFolder => match backend.create_folder(&operation.path) {
            Ok(entry) => entry,
            Err(create_err) => backend
                .get_metadata_by_path(&operation.path)?
                .filter(|entry| entry.is_dir)
                .ok_or(create_err)?,
        },
        PendingMetadataKind::Move => {
            let cloud_remote_id = record
                .cloud_remote_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("move has no cloud identity"))?;
            backend.rename(cloud_remote_id, &operation.path)?
        }
    };
    db.complete_metadata_operation(&operation.local_id, &operation.path, &entry)
}

pub fn mount_mock(paths: AppPaths) -> anyhow::Result<()> {
    cleanup_stale_mountpoint(&paths.mount_dir);
    paths.ensure()?;
    let db = Database::new(paths.db_path.clone());
    db.init()?;
    let backend = MockBackend::new();
    let upload_concurrency = Config::load_or_create(&paths)?
        .power
        .ac_upload_concurrency
        .max(1) as usize;
    let count = sync_metadata(&db, &backend)?;
    println!("twodrive: loaded {count} mock metadata entries");
    println!("twodrive: database {}", db.path().display());
    println!("twodrive: cache {}", paths.cache_dir.display());
    println!("twodrive: mounting {}", paths.mount_dir.display());

    mount_backend(
        paths.mount_dir.clone(),
        db,
        paths.cache_dir.clone(),
        backend,
        upload_concurrency,
    )?;
    Ok(())
}

pub fn mount_graph(paths: AppPaths) -> anyhow::Result<()> {
    cleanup_stale_mountpoint(&paths.mount_dir);
    paths.ensure()?;
    let db = Database::new(paths.db_path.clone());
    db.init()?;
    let backend = GraphBackend::from_paths(&paths)?;
    let upload_concurrency = Config::load_or_create(&paths)?
        .power
        .ac_upload_concurrency
        .max(1) as usize;
    println!("twodrive: database {}", db.path().display());
    println!("twodrive: cache {}", paths.cache_dir.display());
    println!("twodrive: mounting {}", paths.mount_dir.display());

    mount_backend(
        paths.mount_dir.clone(),
        db,
        paths.cache_dir.clone(),
        backend,
        upload_concurrency,
    )
}

pub fn mount_backend<B: CloudBackend>(
    mount_dir: PathBuf,
    db: Database,
    cache_dir: PathBuf,
    backend: B,
    upload_concurrency: usize,
) -> anyhow::Result<()> {
    // No live handles exist yet; reconcile the durable local cache before serving requests.
    recover_interrupted_writes(&db)?;
    let fs = TwoDriveFs::new_with_upload_concurrency(db, cache_dir, backend, upload_concurrency)?;
    let (stop_tx, stop_rx) = mpsc::channel();
    let recovery_db = fs.db.clone();
    let recovery_backend = Arc::clone(&fs.backend);
    let sender = fs.upload_pool.sender.clone();
    let recovery = thread::spawn(move || {
        loop {
            // Replay via the normal workers so recovery and new saves share item locks.
            if let Err(err) = enqueue_recovery(&recovery_db, &sender) {
                eprintln!("twodrive: recovery scan failed: {err:#}");
            }
            if let Err(err) = sync_delta_metadata(&recovery_db, recovery_backend.as_ref()) {
                eprintln!("twodrive: metadata refresh deferred: {err:#}");
            }
            match stop_rx.recv_timeout(Duration::from_secs(60)) {
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                _ => break,
            }
        }
    });
    let options = [MountOption::FSName("twodrive".to_string())];
    let result = fuser::mount2(fs, &mount_dir, &options);
    let _ = stop_tx.send(());
    let _ = recovery.join();
    result?;
    Ok(())
}

fn recover_interrupted_writes(db: &Database) -> anyhow::Result<()> {
    // Only at startup: there are no live writers yet. Keep every byte left in cache.
    for record in db.all_records()? {
        if record.state == FileState::Writing
            && let Some(path) = record.cache_path
            && let Ok(metadata) = fs::metadata(path)
        {
            db.mark_dirty_with_size(&record.metadata.remote_id, metadata.len())?;
        }
    }
    Ok(())
}

fn enqueue_recovery(db: &Database, sender: &UploadQueue) -> anyhow::Result<()> {
    for delete in db.pending_deletes()? {
        sender.send(UploadCommand::Delete(delete.remote_id))?;
    }
    let mut queued = HashSet::new();
    for operation in db.pending_metadata_operations()? {
        queued.insert(operation.local_id.clone());
        sender.send(UploadCommand::Upload(operation.local_id))?;
    }
    for record in db.all_records()? {
        if (matches!(
            record.state,
            FileState::Dirty | FileState::Uploading | FileState::Conflict
        ) || (!record.metadata.is_dir
            && record.effective_pinned()
            && !has_existing_cache(&record)))
            && queued.insert(record.metadata.remote_id.clone())
        {
            sender.send(UploadCommand::Upload(record.metadata.remote_id))?;
        }
    }
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
    let lock = item_sync_lock(&format!("hydrate:{}", record.metadata.remote_id));
    let _guard = lock
        .lock()
        .map_err(|_| anyhow::anyhow!("hydration lock poisoned"))?;
    let refreshed = db.get_by_remote_id(&record.metadata.remote_id)?;
    let record = refreshed.as_ref().unwrap_or(record);

    if matches!(
        record.state,
        FileState::Cached
            | FileState::Pinned
            | FileState::Writing
            | FileState::Dirty
            | FileState::Uploading
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
    let cloud_remote_id = record
        .cloud_remote_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("local-only file has no cloud content to hydrate"))?;
    backend.download_sized_to(
        cloud_remote_id,
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

#[derive(Clone)]
enum UploadCommand {
    Upload(String),
    Delete(String),
    Shutdown,
}

#[derive(Clone)]
struct UploadQueue {
    sender: Sender<UploadCommand>,
    pending: Arc<Mutex<HashMap<String, bool>>>,
}

impl UploadQueue {
    fn send(&self, command: UploadCommand) -> anyhow::Result<()> {
        let key = match &command {
            UploadCommand::Upload(id) => Some(format!("upload:{id}")),
            UploadCommand::Delete(id) => Some(format!("delete:{id}")),
            UploadCommand::Shutdown => None,
        };
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| anyhow::anyhow!("upload queue poisoned"))?;
        if let Some(key) = &key {
            if let Some(rerun) = pending.get_mut(key) {
                *rerun = true;
                return Ok(());
            }
            pending.insert(key.clone(), false);
        }
        if self.sender.send(command).is_err() {
            if let Some(key) = key {
                pending.remove(&key);
            }
            anyhow::bail!("upload queue closed");
        }
        Ok(())
    }
}

struct UploadRun {
    queue: UploadQueue,
    command: UploadCommand,
    key: String,
}

impl UploadRun {
    fn start(queue: &UploadQueue, command: &UploadCommand) -> Option<Self> {
        let key = match command {
            UploadCommand::Upload(id) => format!("upload:{id}"),
            UploadCommand::Delete(id) => format!("delete:{id}"),
            UploadCommand::Shutdown => return None,
        };
        if let Ok(mut pending) = queue.pending.lock() {
            pending.insert(key.clone(), false);
        }
        Some(Self {
            queue: queue.clone(),
            command: command.clone(),
            key,
        })
    }
}

impl Drop for UploadRun {
    fn drop(&mut self) {
        let rerun = self
            .queue
            .pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(&self.key))
            .unwrap_or(false);
        if rerun {
            let _ = self.queue.send(self.command.clone());
        }
    }
}

struct UploadPool<B: CloudBackend> {
    sender: UploadQueue,
    workers: Vec<JoinHandle<()>>,
    _backend: std::marker::PhantomData<B>,
}

impl<B: CloudBackend> UploadPool<B> {
    fn new(db: Database, cache_dir: PathBuf, backend: Arc<B>, concurrency: usize) -> Self {
        let (sender, receiver) = mpsc::channel();
        let receiver = Arc::new(Mutex::new(receiver));
        let sender = UploadQueue {
            sender,
            pending: Arc::new(Mutex::new(HashMap::new())),
        };
        let mut workers = Vec::new();
        for _ in 0..concurrency.max(1) {
            let db = db.clone();
            let cache_dir = cache_dir.clone();
            let backend = Arc::clone(&backend);
            let receiver = Arc::clone(&receiver);
            let queue = sender.clone();
            workers.push(thread::spawn(move || {
                run_upload_worker(db, cache_dir, backend, receiver, queue)
            }));
        }
        Self {
            sender,
            workers,
            _backend: std::marker::PhantomData,
        }
    }

    fn enqueue(&self, remote_id: String) -> anyhow::Result<()> {
        self.sender
            .send(UploadCommand::Upload(remote_id))
            .map_err(|_| anyhow::anyhow!("upload worker queue is closed"))
    }

    fn enqueue_delete(&self, cloud_remote_id: String) -> anyhow::Result<()> {
        self.sender
            .send(UploadCommand::Delete(cloud_remote_id))
            .map_err(|_| anyhow::anyhow!("sync worker queue is closed"))
    }
}

impl<B: CloudBackend> Drop for UploadPool<B> {
    fn drop(&mut self) {
        for _ in &self.workers {
            let _ = self.sender.send(UploadCommand::Shutdown);
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn run_upload_worker<B: CloudBackend>(
    db: Database,
    cache_dir: PathBuf,
    backend: Arc<B>,
    receiver: Arc<Mutex<Receiver<UploadCommand>>>,
    queue: UploadQueue,
) {
    loop {
        let command = match receiver.lock() {
            Ok(receiver) => receiver.recv(),
            Err(_) => return,
        };
        let Ok(command) = command else {
            return;
        };
        let _running = UploadRun::start(&queue, &command);
        let remote_id = match command {
            UploadCommand::Upload(remote_id) => remote_id,
            UploadCommand::Delete(cloud_remote_id) => {
                if let Err(err) = recover_pending_delete_id(&db, backend.as_ref(), &cloud_remote_id)
                {
                    eprintln!("twodrive: queued delete failed for {cloud_remote_id}: {err:#}");
                }
                continue;
            }
            UploadCommand::Shutdown => return,
        };
        let item_lock = item_sync_lock(&remote_id);
        let Ok(_item_guard) = item_lock.lock() else {
            continue;
        };
        if let Ok(Some(operation)) = db.pending_metadata_operation(&remote_id)
            && let Err(err) = recover_pending_metadata_record(&db, backend.as_ref(), &operation)
        {
            eprintln!(
                "twodrive: queued metadata operation failed for {}: {err:#}",
                operation.path
            );
            continue;
        }
        if let Ok(Some(record)) = db.get_by_remote_id(&remote_id)
            && !record.metadata.is_dir
            && record.effective_pinned()
            && !record.cache_path.as_deref().is_some_and(Path::exists)
            && let Err(err) = hydrate_record(&db, &cache_dir, backend.as_ref(), &record)
        {
            eprintln!("twodrive: queued pinned hydration failed for {remote_id}: {err:#}");
        }
        let record = match db.get_by_remote_id(&remote_id) {
            Ok(Some(record))
                if matches!(
                    record.state,
                    FileState::Dirty | FileState::Uploading | FileState::Conflict
                ) =>
            {
                record
            }
            Ok(_) => continue,
            Err(err) => {
                eprintln!("twodrive: queued upload lookup failed for {remote_id}: {err:#}");
                continue;
            }
        };
        let mut activity = ActivityGuard::start(
            &cache_dir,
            "upload",
            &record.metadata.path,
            &record.metadata.name,
            Some(record.metadata.size),
        );
        if let Err(err) = recover_dirty_record(&db, backend.as_ref(), record, &mut |done, total| {
            activity.set_progress(done, Some(total));
            Ok(())
        }) {
            eprintln!("twodrive: queued upload failed for {remote_id}: {err:#}");
        }
        activity.finish();
    }
}

fn item_sync_lock(local_id: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Weak<Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks.lock().expect("item sync lock registry is poisoned");
    if let Some(lock) = locks.get(local_id).and_then(Weak::upgrade) {
        return lock;
    }
    locks.retain(|_, lock| lock.strong_count() > 0);
    let lock = Arc::new(Mutex::new(()));
    locks.insert(local_id.to_string(), Arc::downgrade(&lock));
    lock
}

type ReadJob = Box<dyn FnOnce() + Send>;

struct ReadPool {
    sender: Option<Sender<ReadJob>>,
    workers: Vec<JoinHandle<()>>,
}

impl ReadPool {
    fn new() -> Self {
        let (sender, receiver) = mpsc::channel::<ReadJob>();
        let receiver = Arc::new(Mutex::new(receiver));
        let workers = (0..4)
            .map(|_| {
                let receiver = Arc::clone(&receiver);
                thread::spawn(move || {
                    loop {
                        let job = match receiver.lock() {
                            Ok(receiver) => receiver.recv(),
                            Err(_) => return,
                        };
                        match job {
                            Ok(job) => job(),
                            Err(_) => return,
                        }
                    }
                })
            })
            .collect();
        Self {
            sender: Some(sender),
            workers,
        }
    }

    fn spawn(&self, job: impl FnOnce() + Send + 'static) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(Box::new(job));
        }
    }
}

impl Drop for ReadPool {
    fn drop(&mut self) {
        self.sender.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

pub struct TwoDriveFs<B: CloudBackend> {
    db: Database,
    cache_dir: PathBuf,
    backend: Arc<B>,
    upload_pool: UploadPool<B>,
    read_pool: ReadPool,
    hydrating: Mutex<HashSet<String>>,
    inodes: InodeTable,
    read_handles: HashMap<u64, ReadHandle>,
    write_handles: HashMap<u64, WriteHandle>,
    next_fh: u64,
    directory_handles: HashMap<u64, Vec<(u64, FileType, String)>>,
}

impl<B: CloudBackend> TwoDriveFs<B> {
    pub fn new(db: Database, cache_dir: PathBuf, backend: B) -> anyhow::Result<Self> {
        Self::new_with_upload_concurrency(db, cache_dir, backend, 4)
    }

    pub fn new_with_upload_concurrency(
        db: Database,
        cache_dir: PathBuf,
        backend: B,
        upload_concurrency: usize,
    ) -> anyhow::Result<Self> {
        let records = db.all_records()?;
        let _ = clear_activity_file(&cache_dir);
        db.finish_pending_releases()?;
        let backend = Arc::new(backend);
        let upload_pool = UploadPool::new(
            db.clone(),
            cache_dir.clone(),
            Arc::clone(&backend),
            upload_concurrency,
        );
        Ok(Self {
            db,
            cache_dir,
            backend,
            upload_pool,
            read_pool: ReadPool::new(),
            hydrating: Mutex::new(HashSet::new()),
            inodes: InodeTable::new(&records),
            read_handles: HashMap::new(),
            write_handles: HashMap::new(),
            next_fh: 1,
            directory_handles: HashMap::new(),
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
                self.inodes.refresh_children(parent_path, records);
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
            .or_else(|| self.db.get_by_path(&record.metadata.path).ok().flatten())
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

    fn open_read_handle(&mut self, ino: u64, cache_path: PathBuf) -> anyhow::Result<u64> {
        let cache_guard = fs::File::open(&cache_path)?;
        cache_guard.lock_shared()?;
        let fh = self.next_fh;
        self.next_fh += 1;
        self.read_handles.insert(
            fh,
            ReadHandle {
                _cache_guard: Arc::new(Mutex::new(Some(cache_guard))),
                ino,
                cache_path,
                unlinked: false,
                deferred_record: None,
            },
        );
        Ok(fh)
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

    fn release_read_handle(&mut self, fh: u64) -> anyhow::Result<()> {
        let Some(handle) = self.read_handles.remove(&fh) else {
            return Ok(());
        };
        if handle.unlinked {
            self.cleanup_unlinked_cache(&handle.cache_path)?;
        }
        drop(handle);
        self.db.finish_pending_releases()?;
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

    fn delete_record(&mut self, ino: u64, record: &FileRecord) -> anyhow::Result<()> {
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
        let cache_guard = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&cache_path)?;
        cache_guard.lock_shared()?;

        let now = current_unix_i64();
        let metadata = MetadataEntry::new_file(
            temporary_remote_id.clone(),
            path.clone(),
            0,
            now,
            format!("etag-{temporary_remote_id}"),
        );
        let record = match (|| -> anyhow::Result<FileRecord> {
            self.db.upsert_metadata(&metadata)?;
            self.db.mark_cached(&temporary_remote_id, &cache_path)?;
            self.db
                .mark_state(&temporary_remote_id, FileState::Writing)?;
            self.db
                .get_by_remote_id(&temporary_remote_id)?
                .ok_or_else(|| anyhow::anyhow!("created upload record disappeared"))
        })() {
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

    fn queue_upload_handle(&mut self, fh: u64) -> anyhow::Result<()> {
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
        self.inodes.set_size(handle.ino, size);
        self.upload_pool.enqueue(handle.record_remote_id.clone())?;
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

    fn truncate_without_handle(&mut self, ino: u64, size: u64) -> anyhow::Result<()> {
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

        let local_id = format!("local-upload-{}", unique_suffix());
        let record = self.db.create_local_directory(&local_id, &path)?;
        let ino = self.inodes.insert_or_update(record.clone());
        self.upload_pool.enqueue(local_id)?;
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

                if should_defer_hydration(req, &record) {
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
                    let fh = self.next_fh;
                    self.next_fh += 1;
                    self.read_handles.insert(
                        fh,
                        ReadHandle {
                            ino,
                            cache_path: self
                                .cache_dir
                                .join(sanitize_cache_name(&record.metadata.remote_id)),
                            _cache_guard: Arc::new(Mutex::new(None)),
                            unlinked: false,
                            deferred_record: Some(record),
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
            let record = record.clone();
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
                        let path = hydrate_record(&db, &cache_dir, backend.as_ref(), &record)?;
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
                        reply.error(libc::EIO);
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
    _cache_guard: Arc<Mutex<Option<fs::File>>>,
    unlinked: bool,
    deferred_record: Option<FileRecord>,
}

#[derive(Debug)]
struct WriteHandle {
    ino: u64,
    path: String,
    record_remote_id: String,
    cache_path: PathBuf,
    _cache_guard: fs::File,
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
    next_ino: u64,
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
            next_ino: records.len() as u64 + ROOT_INO + 1,
            path_by_ino,
            ino_by_path,
            record_by_ino,
            children_by_parent_ino,
        }
    }

    fn refresh_children(&mut self, parent_path: &str, records: Vec<FileRecord>) {
        let Some(parent_ino) = self.ino_for_path(parent_path) else {
            return;
        };
        let mut children = Vec::with_capacity(records.len());
        for record in records {
            let ino = self.ino_for_path(&record.metadata.path).unwrap_or_else(|| {
                let ino = self.next_ino;
                self.next_ino += 1;
                ino
            });
            self.path_by_ino.insert(ino, record.metadata.path.clone());
            self.ino_by_path.insert(record.metadata.path.clone(), ino);
            self.record_by_ino.insert(ino, record.clone());
            children.push((ino, record));
        }
        let present: HashSet<u64> = children.iter().map(|(ino, _)| *ino).collect();
        if let Some(previous) = self.children_by_parent_ino.get(&parent_ino) {
            for (ino, record) in previous {
                if !present.contains(ino)
                    && self.ino_by_path.get(&record.metadata.path) == Some(ino)
                {
                    self.ino_by_path.remove(&record.metadata.path);
                }
            }
        }
        self.children_by_parent_ino.insert(parent_ino, children);
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

        let ino = self.next_ino;
        self.next_ino += 1;
        self.path_by_ino.insert(ino, record.metadata.path.clone());
        self.ino_by_path.insert(record.metadata.path.clone(), ino);
        self.record_by_ino.insert(ino, record.clone());
        let parent_ino = parent_ino_for_path(&self.ino_by_path, &record.metadata.path);
        self.children_by_parent_ino
            .entry(parent_ino)
            .or_default()
            .push((ino, record));
        ino
    }

    fn replace_ino_record(&mut self, ino: u64, record: FileRecord) {
        let previous_path = self.path_by_ino.insert(ino, record.metadata.path.clone());
        if let Some(previous_path) = previous_path {
            let previous_parent = parent_ino_for_path(&self.ino_by_path, &previous_path);
            if let Some(children) = self.children_by_parent_ino.get_mut(&previous_parent) {
                children.retain(|(child_ino, _)| *child_ino != ino);
            }
            self.ino_by_path.remove(&previous_path);
        }
        self.ino_by_path.insert(record.metadata.path.clone(), ino);
        self.record_by_ino.insert(ino, record.clone());

        let parent_ino = parent_ino_for_path(&self.ino_by_path, &record.metadata.path);
        self.children_by_parent_ino
            .entry(parent_ino)
            .or_default()
            .push((ino, record));
    }

    fn set_size(&mut self, ino: u64, size: u64) {
        if let Some(record) = self.record_by_ino.get_mut(&ino) {
            record.metadata.size = record.metadata.size.max(size);
        }
    }

    fn replace_size(&mut self, ino: u64, size: u64) {
        if let Some(record) = self.record_by_ino.get_mut(&ino) {
            record.metadata.size = size;
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
        let mut children: Vec<_> = self
            .children_by_parent_ino
            .get(&ino)
            .into_iter()
            .flatten()
            .filter_map(|(ino, _)| {
                self.record_by_ino
                    .get(ino)
                    .cloned()
                    .map(|record| (*ino, record))
            })
            .collect();
        // Sort once when building a directory snapshot, never on every file creation.
        sort_child_records(&mut children);
        children
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
        FileState::Cached
            | FileState::Pinned
            | FileState::Writing
            | FileState::Dirty
            | FileState::Uploading
    ) && record.cache_path.as_deref().is_some_and(Path::exists)
}

fn record_upload_etag(record: &FileRecord) -> Option<String> {
    if record.cloud_remote_id.is_none() || record.metadata.etag.is_empty() {
        None
    } else {
        Some(record.metadata.etag.clone())
    }
}

fn record_upload_remote_id(record: &FileRecord) -> Option<&str> {
    record.cloud_remote_id.as_deref()
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
    let cloud_remote_id = original
        .cloud_remote_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("local-only item cannot have an etag conflict"))?;
    let latest_original = backend.get_metadata(cloud_remote_id)?.ok_or_else(|| {
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
    struct ConcurrentBackend {
        inner: MockBackend,
        active_uploads: AtomicUsize,
        max_active_uploads: AtomicUsize,
    }

    impl ConcurrentBackend {
        fn new() -> Self {
            Self {
                inner: MockBackend::new(),
                active_uploads: AtomicUsize::new(0),
                max_active_uploads: AtomicUsize::new(0),
            }
        }
    }

    impl CloudBackend for ConcurrentBackend {
        fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
            self.inner.list_all()
        }

        fn download(&self, remote_id: &str) -> anyhow::Result<Vec<u8>> {
            self.inner.download(remote_id)
        }

        fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry> {
            self.inner.upload(path, content)
        }

        fn upload_file_with_version(
            &self,
            path: &str,
            source_path: &Path,
            remote_id: Option<&str>,
            if_match: Option<&str>,
            on_progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
        ) -> anyhow::Result<MetadataEntry> {
            let active = self.active_uploads.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active_uploads.fetch_max(active, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(100));
            let result = self.inner.upload_file_with_version(
                path,
                source_path,
                remote_id,
                if_match,
                on_progress,
            );
            self.active_uploads.fetch_sub(1, Ordering::SeqCst);
            result
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

    #[derive(Debug)]
    struct LostCreateAckBackend {
        inner: MockBackend,
    }

    impl CloudBackend for LostCreateAckBackend {
        fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
            self.inner.list_all()
        }

        fn download(&self, remote_id: &str) -> anyhow::Result<Vec<u8>> {
            self.inner.download(remote_id)
        }

        fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry> {
            self.inner.upload(path, content)
        }

        fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
            self.inner.create_folder(path)?;
            anyhow::bail!("simulated lost create acknowledgement")
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

    #[derive(Debug)]
    struct SlowDownloadBackend {
        inner: MockBackend,
        started: Sender<()>,
    }

    impl CloudBackend for SlowDownloadBackend {
        fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
            self.inner.list_all()
        }
        fn download(&self, remote_id: &str) -> anyhow::Result<Vec<u8>> {
            let _ = self.started.send(());
            thread::sleep(Duration::from_secs(3));
            self.inner.download(remote_id)
        }
        fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry> {
            self.inner.upload(path, content)
        }
        fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
            self.inner.create_folder(path)
        }
        fn rename(&self, remote_id: &str, path: &str) -> anyhow::Result<MetadataEntry> {
            self.inner.rename(remote_id, path)
        }
        fn delete(&self, remote_id: &str) -> anyhow::Result<()> {
            self.inner.delete(remote_id)
        }
    }

    #[test]
    #[ignore = "requires /dev/fuse and fusermount3; uses an isolated temporary mount"]
    fn mounted_large_listing_and_copy_remain_responsive_during_download() {
        let root = test_root("mounted-responsiveness");
        let mount = root.join("mount");
        fs::create_dir_all(&mount).unwrap();
        let db = Database::new(root.join("test.sqlite3"));
        db.init().unwrap();
        let (started, waiting) = mpsc::channel();
        let backend = SlowDownloadBackend {
            inner: MockBackend::new(),
            started,
        };
        sync_metadata(&db, &backend).unwrap();
        let mut entries = vec![MetadataEntry::new_dir("listing", "/listing", 0, "etag")];
        entries.extend((0..10_000).map(|index| {
            MetadataEntry::new_file(
                format!("list-{index}"),
                format!("/listing/file-{index:05}"),
                0,
                0,
                "etag",
            )
        }));
        db.upsert_metadata_batch(&entries).unwrap();
        let filesystem = TwoDriveFs::new(db, root.join("cache"), backend).unwrap();
        let session = fuser::spawn_mount2(filesystem, &mount, &[]).unwrap();
        let cloud_file = mount.join("README-cloud.txt");
        let reader = thread::spawn(move || fs::read(cloud_file).unwrap());
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        let start = std::time::Instant::now();
        let names: Vec<_> = fs::read_dir(mount.join("listing"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 10_000);
        assert_eq!(names.iter().collect::<HashSet<_>>().len(), 10_000);
        let destination = mount.join("large-copy.bin");
        let bytes = vec![0x51; 16 * 1024 * 1024];
        fs::write(&destination, &bytes).unwrap();
        let foreground = start.elapsed();
        assert!(
            foreground < Duration::from_secs(2),
            "foreground blocked: {foreground:?}"
        );
        assert_eq!(fs::read(destination).unwrap(), bytes);
        assert!(!reader.join().unwrap().is_empty());
        eprintln!("10,000-entry listing + 16 MiB copy during delayed download: {foreground:?}");
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interrupted_open_write_is_recovered_with_actual_cache_size() {
        let mut test = TestFs::new("restart-open-write");
        let (_, fh, _) = test
            .fs
            .create_upload(ROOT_INO, OsStr::new("interrupted.bin"), 0)
            .unwrap();
        let handle = test.fs.write_handles.get(&fh).unwrap();
        let id = handle.record_remote_id.clone();
        fs::write(&handle.cache_path, b"durable bytes before shutdown").unwrap();
        test.fs.write_handles.clear(); // Simulated lost process, without release/queue.
        recover_interrupted_writes(&test.fs.db).unwrap();
        let record = test.fs.db.get_by_remote_id(&id).unwrap().unwrap();
        assert_eq!(record.state, FileState::Dirty);
        assert_eq!(record.metadata.size, 29);
        assert_eq!(
            recover_dirty_uploads(&test.fs.db, test.fs.backend.as_ref()).unwrap(),
            1
        );
        assert_eq!(
            test.fs.db.get_by_remote_id(&id).unwrap().unwrap().state,
            FileState::Cached
        );
    }

    #[test]
    fn recovery_queue_coalesces_repeated_scans_and_retains_work() {
        let (sender, receiver) = mpsc::channel();
        let queue = UploadQueue {
            sender,
            pending: Arc::new(Mutex::new(HashMap::new())),
        };
        for _ in 0..100 {
            queue.send(UploadCommand::Upload("same".into())).unwrap();
            queue.send(UploadCommand::Delete("same".into())).unwrap();
        }
        assert_eq!(receiver.try_iter().count(), 2);
    }

    #[test]
    fn running_upload_coalesces_retries_without_occupying_other_workers() {
        let (sender, receiver) = mpsc::channel();
        let queue = UploadQueue {
            sender,
            pending: Arc::new(Mutex::new(HashMap::new())),
        };
        queue.send(UploadCommand::Upload("large".into())).unwrap();
        let command = receiver.recv().unwrap();
        let running = UploadRun::start(&queue, &command).unwrap();
        for _ in 0..100 {
            queue.send(UploadCommand::Upload("large".into())).unwrap();
        }
        assert!(receiver.try_recv().is_err());
        queue.send(UploadCommand::Upload("small".into())).unwrap();
        assert!(matches!(receiver.try_recv().unwrap(), UploadCommand::Upload(id) if id == "small"));
        drop(running);
        assert!(matches!(receiver.try_recv().unwrap(), UploadCommand::Upload(id) if id == "large"));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn large_directory_refresh_preserves_inodes_and_removes_stale_names() {
        let test = TestFs::new("directory-refresh");
        let template = test
            .fs
            .db
            .all_records()
            .unwrap()
            .into_iter()
            .find(|r| !r.metadata.is_dir)
            .unwrap();
        let records: Vec<_> = (0..10_000)
            .map(|index| {
                let mut record = template.clone();
                record.metadata = MetadataEntry::new_file(
                    format!("id-{index}"),
                    format!("/file-{index:05}"),
                    index,
                    0,
                    "etag",
                );
                record
            })
            .collect();
        let mut table = InodeTable::new(&[]);
        table.refresh_children("/", records.clone());
        let stable = table.ino_for_path("/file-00001").unwrap();
        table.refresh_children("/", records[1..].to_vec());
        assert_eq!(table.ino_for_path("/file-00001"), Some(stable));
        assert_eq!(table.ino_for_path("/file-00000"), None);
        table.set_size(stable, 12345);
        let children = table.children_for_ino(ROOT_INO);
        assert_eq!(children.len(), 9999);
        assert_eq!(children[0].1.metadata.size, 12345);
    }

    #[test]
    fn filesystem_stats_report_real_backing_store_capacity() {
        let root = test_root("statfs");
        fs::create_dir_all(&root).unwrap();

        let stats = filesystem_stats(&root).unwrap();

        assert!(stats.blocks > 0);
        assert!(stats.blocks_available > 0);
        assert!(stats.block_size > 0);
        assert!(stats.fragment_size > 0);
        assert!(stats.name_length >= 255);
        fs::remove_dir_all(root).unwrap();
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
        assert_eq!(record.state, FileState::Writing);
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
    fn path_truncate_is_local_and_queues_an_upload() {
        let mut test = TestFs::new("path-truncate");
        let ino = test.fs.inodes.ino_for_path("/README-cloud.txt").unwrap();

        test.fs.truncate_without_handle(ino, 0).unwrap();

        let local = test
            .fs
            .db
            .get_by_path("/README-cloud.txt")
            .unwrap()
            .unwrap();
        assert_eq!(local.metadata.size, 0);
        assert!(matches!(
            local.state,
            FileState::Dirty | FileState::Uploading | FileState::Cached
        ));
        assert_eq!(fs::metadata(local.cache_path.unwrap()).unwrap().len(), 0);
    }

    #[test]
    fn folder_create_recovery_rebinds_after_a_lost_remote_acknowledgement() {
        let root = test_root("lost-folder-create-ack");
        fs::create_dir_all(&root).unwrap();
        let db = Database::new(root.join("test.sqlite3"));
        db.init().unwrap();
        let backend = LostCreateAckBackend {
            inner: MockBackend::new(),
        };
        db.create_local_directory("local-folder", "/Recovered folder")
            .unwrap();

        assert_eq!(
            recover_pending_metadata_operations(&db, &backend).unwrap(),
            1
        );
        let record = db.get_by_remote_id("local-folder").unwrap().unwrap();
        assert_eq!(record.metadata.path, "/Recovered folder");
        assert!(record.cloud_remote_id.is_some());
        assert!(db.pending_metadata_operations().unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn immediate_reopen_during_upload_keeps_the_local_record() {
        let root = test_root("immediate-reopen");
        fs::create_dir_all(&root).unwrap();
        let db = Database::new(root.join("test.sqlite3"));
        db.init().unwrap();
        let backend = ConcurrentBackend::new();
        sync_metadata(&db, &backend).unwrap();
        let mut fuse =
            TwoDriveFs::new_with_upload_concurrency(db, root.join("cache"), backend, 2).unwrap();

        let (ino, first_fh, _) = fuse
            .create_upload(ROOT_INO, OsStr::new("rapid.pdf"), libc::O_CREAT)
            .unwrap();
        let local_id = fuse.write_handles[&first_fh].record_remote_id.clone();
        let cache_path = fuse.write_handles[&first_fh].cache_path.clone();
        write_slice(&cache_path, 0, b"first generation").unwrap();
        fuse.inodes.set_size(ino, 16);
        fuse.queue_upload_handle(first_fh).unwrap();
        fuse.write_handles.remove(&first_fh);

        let record = fuse.db.get_by_path("/rapid.pdf").unwrap().unwrap();
        let (_, second_fh, _) = fuse.create_overwrite_upload(record, true).unwrap();
        let second_cache = fuse.write_handles[&second_fh].cache_path.clone();
        write_slice(&second_cache, 0, b"second generation").unwrap();
        fuse.queue_upload_handle(second_fh).unwrap();
        fuse.write_handles.remove(&second_fh);

        for _ in 0..80 {
            let record = fuse.db.get_by_remote_id(&local_id).unwrap().unwrap();
            if record.state == FileState::Cached && record.cloud_remote_id.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        let record = fuse.db.get_by_remote_id(&local_id).unwrap().unwrap();
        assert_eq!(record.metadata.remote_id, local_id);
        assert_eq!(record.state, FileState::Cached);
        assert_eq!(
            fs::read(record.cache_path.unwrap()).unwrap(),
            b"second generation"
        );
        drop(fuse);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repeated_atomic_replacements_during_initial_upload_keep_latest_generation() {
        let root = test_root("repeated-atomic-replace");
        fs::create_dir_all(&root).unwrap();
        let db = Database::new(root.join("test.sqlite3"));
        db.init().unwrap();
        let backend = ConcurrentBackend::new();
        sync_metadata(&db, &backend).unwrap();
        let mut fuse =
            TwoDriveFs::new_with_upload_concurrency(db, root.join("cache"), backend, 2).unwrap();

        let (target_ino, target_fh, _) = fuse
            .create_upload(ROOT_INO, OsStr::new("rapid.pdf"), libc::O_CREAT)
            .unwrap();
        let target_id = fuse.write_handles[&target_fh].record_remote_id.clone();
        let target_cache = fuse.write_handles[&target_fh].cache_path.clone();
        write_slice(&target_cache, 0, b"initial generation").unwrap();
        fuse.inodes.set_size(target_ino, 18);
        fuse.queue_upload_handle(target_fh).unwrap();
        fuse.write_handles.remove(&target_fh);

        let mut temp_ids = Vec::new();
        for (name, content) in [
            (".goutputstream-first", b"first saved generation".as_slice()),
            (
                ".goutputstream-second",
                b"second saved generation".as_slice(),
            ),
        ] {
            let (temp_ino, temp_fh, _) = fuse
                .create_upload(ROOT_INO, OsStr::new(name), libc::O_CREAT | libc::O_EXCL)
                .unwrap();
            temp_ids.push(fuse.write_handles[&temp_fh].record_remote_id.clone());
            let temp_cache = fuse.write_handles[&temp_fh].cache_path.clone();
            write_slice(&temp_cache, 0, content).unwrap();
            fuse.inodes.set_size(temp_ino, content.len() as u64);
            fuse.rename_record(
                ROOT_INO,
                OsStr::new(name),
                ROOT_INO,
                OsStr::new("rapid.pdf"),
            )
            .unwrap();
            assert_eq!(fuse.write_handles[&temp_fh].record_remote_id, target_id);
            fuse.queue_upload_handle(temp_fh).unwrap();
            fuse.write_handles.remove(&temp_fh);
        }

        for _ in 0..120 {
            let record = fuse.db.get_by_remote_id(&target_id).unwrap().unwrap();
            if record.state == FileState::Cached
                && record.cloud_remote_id.is_some()
                && fuse
                    .backend
                    .download(record.cloud_remote_id.as_deref().unwrap())
                    .ok()
                    .as_deref()
                    == Some(b"second saved generation")
            {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }

        let record = fuse.db.get_by_path("/rapid.pdf").unwrap().unwrap();
        assert_eq!(record.metadata.remote_id, target_id);
        assert_eq!(record.state, FileState::Cached);
        assert_eq!(
            fs::read(record.cache_path.as_ref().unwrap()).unwrap(),
            b"second saved generation"
        );
        assert_eq!(
            fuse.backend
                .download(record.cloud_remote_id.as_deref().unwrap())
                .unwrap(),
            b"second saved generation"
        );
        for temp_id in temp_ids {
            assert!(fuse.db.get_by_remote_id(&temp_id).unwrap().is_none());
        }
        drop(fuse);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn release_requested_during_upload_finishes_after_last_open_handle() {
        let mut test = TestFs::new("release-after-upload");
        let (ino, fh, _) = test
            .fs
            .create_upload(ROOT_INO, OsStr::new("release.txt"), libc::O_CREAT)
            .unwrap();
        let cache = test.fs.write_handles[&fh].cache_path.clone();
        write_slice(&cache, 0, b"safe upload").unwrap();
        test.fs.inodes.set_size(ino, 11);
        let read_fh = test.fs.open_read_handle(ino, cache.clone()).unwrap();
        assert_eq!(test.fs.db.release_path("/release.txt").unwrap(), 0);
        test.fs.queue_upload_handle(fh).unwrap();
        test.fs.write_handles.remove(&fh);
        for _ in 0..120 {
            if test
                .fs
                .db
                .get_by_path("/release.txt")
                .unwrap()
                .unwrap()
                .state
                == FileState::Cached
            {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        let uploaded = test.fs.db.get_by_path("/release.txt").unwrap().unwrap();
        assert_eq!(uploaded.state, FileState::Cached);
        assert_eq!(
            test.fs
                .backend
                .download(uploaded.cloud_remote_id.as_deref().unwrap())
                .unwrap(),
            b"safe upload"
        );
        assert_eq!(
            test.fs.read_open_handle(read_fh, 0, 11).unwrap(),
            b"safe upload"
        );
        assert!(cache.exists());
        test.fs.release_read_handle(read_fh).unwrap();
        assert!(!cache.exists());
        assert_eq!(
            test.fs
                .db
                .get_by_path("/release.txt")
                .unwrap()
                .unwrap()
                .state,
            FileState::OnlineOnly
        );
    }

    #[test]
    fn editor_style_temp_file_can_replace_an_existing_remote_file() {
        let mut test = TestFs::new("atomic-replace");
        let old_target = test
            .fs
            .db
            .get_by_path("/README-cloud.txt")
            .unwrap()
            .unwrap();
        let old_target_ino = test.fs.inodes.ino_for_path("/README-cloud.txt").unwrap();
        let old_target_cache = test.fs.ensure_cached(&old_target).unwrap();
        let old_target_fh = test
            .fs
            .open_read_handle(old_target_ino, old_target_cache.clone())
            .unwrap();
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

        assert_eq!(
            test.fs.read_open_handle(old_target_fh, 0, 7).unwrap(),
            b"Welcome"
        );
        assert!(old_target_cache.exists());
        test.fs.release_read_handle(old_target_fh).unwrap();
        assert!(!old_target_cache.exists());

        for _ in 0..40 {
            let current = test
                .fs
                .db
                .get_by_path("/README-cloud.txt")
                .unwrap()
                .unwrap();
            if test
                .fs
                .backend
                .download(current.cloud_remote_id.as_deref().unwrap())
                .ok()
                .as_deref()
                == Some(b"new atomically saved content\n")
            {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        let record = test
            .fs
            .db
            .get_by_path("/README-cloud.txt")
            .unwrap()
            .unwrap();
        assert_eq!(
            test.fs
                .backend
                .download(record.cloud_remote_id.as_deref().unwrap())
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
        let fh = test.fs.open_read_handle(ino, cache_path.clone()).unwrap();

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
        let remote_id = test.fs.write_handles[&fh].record_remote_id.clone();
        test.fs.db.mark_state(&remote_id, FileState::Dirty).unwrap();

        assert_eq!(
            recover_dirty_uploads(&test.fs.db, test.fs.backend.as_ref()).unwrap(),
            1
        );
        let recovered = test.fs.db.get_by_path("/recover-me.txt").unwrap().unwrap();
        assert_eq!(recovered.state, FileState::Cached);
        assert_eq!(
            test.fs
                .backend
                .download(recovered.cloud_remote_id.as_deref().unwrap())
                .unwrap(),
            b"durable before restart"
        );
    }

    #[test]
    fn incomplete_writing_cache_is_not_uploaded_after_restart() {
        let mut test = TestFs::new("writing-not-recovered");
        let (_ino, fh, _) = test
            .fs
            .create_upload(ROOT_INO, OsStr::new("partial.txt"), libc::O_CREAT)
            .unwrap();
        let remote_id = test.fs.write_handles[&fh].record_remote_id.clone();
        let cache_path = test.fs.write_handles[&fh].cache_path.clone();
        write_slice(&cache_path, 0, b"partial data").unwrap();
        test.fs.sync_handle(fh, false).unwrap();

        assert_eq!(
            recover_dirty_uploads(&test.fs.db, test.fs.backend.as_ref()).unwrap(),
            0
        );
        let record = test.fs.db.get_by_remote_id(&remote_id).unwrap().unwrap();
        assert_eq!(record.state, FileState::Writing);
        assert!(cache_path.exists());
        assert!(test.fs.backend.download(&remote_id).is_err());
    }

    #[test]
    fn closed_small_files_upload_concurrently() {
        let root = test_root("concurrent-uploads");
        fs::create_dir_all(&root).unwrap();
        let db = Database::new(root.join("test.sqlite3"));
        db.init().unwrap();
        let backend = ConcurrentBackend::new();
        sync_metadata(&db, &backend).unwrap();
        let mut fuse =
            TwoDriveFs::new_with_upload_concurrency(db, root.join("cache"), backend, 4).unwrap();
        let mut stale_record = None;

        for index in 0..4 {
            let name = format!("small-{index}.txt");
            let (_ino, fh, _) = fuse
                .create_upload(ROOT_INO, OsStr::new(&name), libc::O_CREAT)
                .unwrap();
            let cache_path = fuse.write_handles[&fh].cache_path.clone();
            write_slice(&cache_path, 0, name.as_bytes()).unwrap();
            if index == 0 {
                stale_record = fuse
                    .db
                    .get_by_remote_id(&fuse.write_handles[&fh].record_remote_id)
                    .unwrap();
            }
            fuse.queue_upload_handle(fh).unwrap();
            fuse.write_handles.remove(&fh);
        }

        for _ in 0..100 {
            let completed = (0..4)
                .filter(|index| {
                    fuse.db
                        .get_by_path(&format!("/small-{index}.txt"))
                        .ok()
                        .flatten()
                        .is_some_and(|record| record.state == FileState::Cached)
                })
                .count();
            if completed == 4 {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }

        assert_eq!(
            (0..4)
                .filter(|index| fuse
                    .db
                    .get_by_path(&format!("/small-{index}.txt"))
                    .unwrap()
                    .is_some_and(|record| record.state == FileState::Cached))
                .count(),
            4
        );
        assert!(
            fuse.backend.max_active_uploads.load(Ordering::SeqCst) >= 2,
            "expected at least two simultaneous uploads"
        );
        let stale_record = stale_record.unwrap();
        let refreshed = fuse.refresh_record(&stale_record);
        assert_eq!(refreshed.metadata.path, "/small-0.txt");
        assert_eq!(
            refreshed.metadata.remote_id, stale_record.metadata.remote_id,
            "the local identity must remain stable after upload"
        );
        assert!(refreshed.cloud_remote_id.is_some());
        drop(fuse);
        fs::remove_dir_all(root).unwrap();
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
                .download(recovered.cloud_remote_id.as_deref().unwrap())
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
    fn stale_local_upload_placeholder_can_be_deleted_and_recreated() {
        let mut test = TestFs::new("stale-local-placeholder");
        let metadata = MetadataEntry::new_file(
            "local-upload-stale-placeholder",
            "/stale-placeholder.bin",
            0,
            1,
            "etag-local-upload-stale-placeholder",
        );
        test.fs.db.upsert_metadata(&metadata).unwrap();
        let record = test
            .fs
            .db
            .get_by_remote_id(&metadata.remote_id)
            .unwrap()
            .unwrap();
        let ino = test.fs.inodes.insert_or_update(record.clone());

        test.fs.delete_record(ino, &record).unwrap();

        assert!(test.fs.db.pending_deletes().unwrap().is_empty());
        assert!(
            test.fs
                .db
                .get_by_path("/stale-placeholder.bin")
                .unwrap()
                .is_none()
        );
        let (_, fh, _) = test
            .fs
            .create_upload(ROOT_INO, OsStr::new("stale-placeholder.bin"), libc::O_CREAT)
            .unwrap();
        assert!(test.fs.write_handles.contains_key(&fh));
    }

    #[test]
    fn legacy_pending_delete_for_local_placeholder_is_cleaned_without_cloud_call() {
        let test = TestFs::new("legacy-local-pending-delete");
        let metadata = MetadataEntry::new_file(
            "local-upload-legacy-pending",
            "/legacy-pending.bin",
            0,
            1,
            "etag-local-upload-legacy-pending",
        );
        test.fs.db.upsert_metadata(&metadata).unwrap();
        let record = test
            .fs
            .db
            .get_by_remote_id(&metadata.remote_id)
            .unwrap()
            .unwrap();
        test.fs.db.queue_pending_delete(&record).unwrap();

        assert_eq!(
            recover_pending_deletes(&test.fs.db, test.fs.backend.as_ref()).unwrap(),
            1
        );
        assert!(test.fs.db.pending_deletes().unwrap().is_empty());
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

        for _ in 0..40 {
            let moved = test
                .fs
                .db
                .get_by_path("/Courses/twodrive-notes.txt")
                .unwrap()
                .unwrap();
            if moved.state == FileState::Pinned
                && moved.cache_path.as_deref().is_some_and(Path::exists)
            {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }

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

    #[test]
    fn cloud_file_discovered_later_stays_online_only_under_a_pinned_directory() {
        let test = TestFs::new("cloud-file-online-only");
        let courses = test.fs.db.get_by_path("/Courses").unwrap().unwrap();
        test.fs
            .db
            .set_explicit_pin(&courses.metadata.remote_id, true)
            .unwrap();
        test.fs
            .backend
            .upload("/Courses/new-from-cloud.txt", b"cloud content".to_vec())
            .unwrap();

        sync_metadata(&test.fs.db, test.fs.backend.as_ref()).unwrap();
        hydrate_pending_pins(&test.fs.db, &test.fs.cache_dir, test.fs.backend.as_ref()).unwrap();

        let discovered = test
            .fs
            .db
            .get_by_path("/Courses/new-from-cloud.txt")
            .unwrap()
            .unwrap();
        assert_eq!(discovered.state, FileState::OnlineOnly);
        assert!(discovered.pin_inheritance_blocked);
        assert!(!discovered.effective_pinned());
        assert!(discovered.cache_path.is_none());
    }
}
