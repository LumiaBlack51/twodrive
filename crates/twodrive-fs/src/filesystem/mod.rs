use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request, TimeOrNow,
};
use std::collections::{HashMap, HashSet};
use std::ffi::{CString, OsStr};
use std::fs::{self, OpenOptions};
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use twodrive_backend::CloudBackend;
use twodrive_core::{Database, FileRecord, FileState, join_cloud_path};

use crate::activity::{ActivityGuard, clear_activity_file};
use crate::cache_io::{
    has_existing_cache, read_slice, sanitize_cache_name, unique_suffix, write_slice,
};
use crate::conflicts::record_upload_etag;
use crate::hydration::{hydrate_generation, hydrate_record};
use crate::inodes::{InodeTable, attr_for_record, file_type, root_attr, unlinked_file_attr};
use crate::probes::{is_gio_metadata_probe, request_process_identity, should_defer_hydration};
use crate::read_pool::ReadPool;
use crate::upload_queue::UploadPool;

pub struct TwoDriveFs<B: CloudBackend> {
    pub(crate) db: Database,
    cache_dir: PathBuf,
    pub(crate) backend: Arc<B>,
    pub(crate) upload_pool: UploadPool<B>,
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

    pub(crate) fn record_for_ino(&self, ino: u64) -> Option<FileRecord> {
        if ino == ROOT_INO {
            return None;
        }

        self.inodes.record_for_ino(ino).cloned()
    }

    pub(crate) fn attr_for_ino(&self, ino: u64) -> Option<FileAttr> {
        if ino == ROOT_INO {
            return Some(root_attr());
        }

        if let Some(record) = self.record_for_ino(ino) {
            return Some(attr_for_record(ino, &record));
        }

        // Atomic replacement removes the old directory entry, not an open
        // descriptor. Poppler calls fstat on that descriptor on later saves.
        for handle in self.read_handles.values() {
            if handle.ino == ino && handle.unlinked {
                let mut attr = handle.open_attr;
                attr.nlink = 0;
                return Some(attr);
            }
        }
        self.write_handles
            .values()
            .find(|handle| handle.ino == ino && handle.unlinked)
            .and_then(|handle| unlinked_file_attr(ino, &handle._cache_guard))
    }

    pub(crate) fn refresh_child_records(&mut self, parent_path: &str) {
        match self.db.list_children(parent_path) {
            Ok(records) => {
                self.inodes.refresh_children(parent_path, records);
            }
            Err(err) => {
                eprintln!("twodrive refresh children error for {parent_path}: {err:#}");
            }
        }
    }

    pub(crate) fn refresh_record(&self, record: &FileRecord) -> FileRecord {
        self.db
            .get_by_remote_id(&record.metadata.remote_id)
            .ok()
            .flatten()
            .or_else(|| self.db.get_by_path(&record.metadata.path).ok().flatten())
            .unwrap_or_else(|| record.clone())
    }

    pub(crate) fn ensure_cached(&self, record: &FileRecord) -> anyhow::Result<PathBuf> {
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
}
#[derive(Debug)]
struct ReadHandle {
    open_attr: FileAttr,
    ino: u64,
    cache_path: PathBuf,
    _cache_guard: Arc<Mutex<Option<fs::File>>>,
    unlinked: bool,
    deferred_record: Option<FileRecord>,
    download_generation: i64,
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

pub(crate) const ROOT_INO: u64 = 1;
pub(crate) const TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy)]
pub(crate) struct FilesystemStats {
    blocks: u64,
    blocks_free: u64,
    blocks_available: u64,
    files: u64,
    files_free: u64,
    block_size: u32,
    name_length: u32,
    fragment_size: u32,
}

pub(crate) fn filesystem_stats(path: &Path) -> io::Result<FilesystemStats> {
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

mod fuse;
mod mutations;
mod reads;
mod writes;

#[cfg(test)]
mod tests;
