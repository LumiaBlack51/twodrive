//! Ordinary-file notifications supplement placeholder callbacks. Batches are
//! observations, not pre-commit delete authority; overflow requires a new scan.
use super::*;
use windows_sys::Win32::System::{
    IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
    Threading::{CreateEventW, ResetEvent, WaitForSingleObject},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DirectoryChange {
    pub action: u32,
    pub path: String,
    pub file_id: u64,
    pub parent_file_id: u64,
    pub size: u64,
    pub attributes: u32,
    pub modified_time: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DirectoryChangeBatch {
    pub rescan_required: bool,
    pub changes: Vec<DirectoryChange>,
}

fn decode(bytes: &[u8]) -> Result<DirectoryChangeBatch> {
    if bytes.is_empty() {
        return Ok(DirectoryChangeBatch {
            rescan_required: true,
            changes: Vec::new(),
        });
    }
    let mut offset = 0usize;
    let mut changes = Vec::new();
    loop {
        let record = bytes.get(offset..).ok_or("invalid directory offset")?;
        if record.len() < 84 {
            return Err("short directory notification".into());
        }
        let u32at = |index: usize| u32::from_le_bytes(record[index..index + 4].try_into().unwrap());
        let u64at = |index: usize| u64::from_le_bytes(record[index..index + 8].try_into().unwrap());
        let next = u32at(0) as usize;
        let action = u32at(4);
        let length = u32at(80) as usize;
        if !(1..=5).contains(&action)
            || length == 0
            || !length.is_multiple_of(2)
            || length > 65534
            || 84usize
                .checked_add(length)
                .is_none_or(|end| end > record.len())
        {
            return Err("invalid directory notification fields".into());
        }
        let name = record[84..84 + length]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|part| u16::from_le_bytes([part[0], part[1]]))
            .collect::<Vec<_>>();
        let path = String::from_utf16(&name).map_err(|_| "invalid directory notification name")?;
        if path.contains('\0')
            || Path::new(&path)
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err("directory notification escapes the root".into());
        }
        let size = u64at(48);
        if size > i64::MAX as u64 {
            return Err("negative directory notification size".into());
        }
        changes.push(DirectoryChange {
            action,
            path,
            file_id: u64at(64),
            parent_file_id: u64at(72),
            size,
            attributes: u32at(56),
            modified_time: u64at(16),
        });
        if changes.len() > 10000 {
            return Err("directory notification batch too large".into());
        }
        if next == 0 {
            if record.len() - (84 + length) > 7 {
                return Err("unlinked trailing directory notification".into());
            }
            break;
        }
        if !next.is_multiple_of(4) || next < 84 + length || next >= record.len() {
            return Err("invalid chained directory notification".into());
        }
        offset = offset
            .checked_add(next)
            .ok_or("directory offset overflow")?;
    }
    Ok(DirectoryChangeBatch {
        rescan_required: false,
        changes,
    })
}

pub struct DirectoryWatcher {
    directory: Handle,
    event: Handle,
    receipt: RootReceipt,
    overlapped: Box<OVERLAPPED>,
    buffer: Vec<u64>,
    pending: bool,
}

/// A new subscription always requires an inventory, including a clean restart.
/// On any read/persistence failure this wrapper stops: replaying an uncommitted
/// batch after rearming would silently lose observations.
pub struct DurableDirectoryWatcher {
    watcher: DirectoryWatcher,
    db: twodrive_core::Database,
    binding: String,
    epoch: i64,
    failed: bool,
}

impl DurableDirectoryWatcher {
    pub fn binding(&self) -> &str {
        &self.binding
    }
    pub fn state(&self) -> Result<twodrive_core::NativeWatchState> {
        if self.failed {
            return Err("directory watcher requires restart and reconciliation".into());
        }
        let state = self
            .db
            .native_watch_state(&self.binding)
            .map_err(|e| e.to_string())?;
        if state.epoch != self.epoch {
            return Err("directory watcher was superseded".into());
        }
        Ok(state)
    }
    /// Returns a durable watermark, never an uncommitted observation batch.
    pub fn poll(&mut self, timeout_ms: u32) -> Result<Option<twodrive_core::NativeWatchState>> {
        self.state()?;
        let result = (|| {
            let Some(batch) = self.watcher.poll(timeout_ms)? else {
                return Ok(None);
            };
            let payloads = batch
                .changes
                .iter()
                .map(serde_json::to_string)
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            self.db
                .append_native_observations(
                    &self.binding,
                    self.epoch,
                    &payloads,
                    batch.rescan_required,
                )
                .map(Some)
                .map_err(|e| e.to_string())
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}
impl PersistentSession {
    pub fn watch_directory_durable(
        &self,
        db: twodrive_core::Database,
    ) -> Result<DurableDirectoryWatcher> {
        let binding = serde_json::to_string(&self.receipt).map_err(|e| e.to_string())?;
        let state = db.begin_native_watch(&binding).map_err(|e| e.to_string())?;
        let watcher = self.watch_directory()?;
        Ok(DurableDirectoryWatcher {
            watcher,
            db,
            binding,
            epoch: state.epoch,
            failed: false,
        })
    }

    pub fn watch_directory(&self) -> Result<DirectoryWatcher> {
        let raw = unsafe {
            CreateFileW(
                wide(self.root()).as_ptr(),
                FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OVERLAPPED,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err("cannot open directory watcher".into());
        }
        let directory = Handle(raw, false);
        let (volume, id, kind) = file_binding(directory.0)?;
        if !kind || volume != self.receipt.volume_serial || id != self.receipt.file_id {
            return Err("watcher root binding changed".into());
        }
        let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if event.is_null() {
            return Err("cannot allocate directory watcher event".into());
        }
        let mut watcher = DirectoryWatcher {
            directory,
            event: Handle(event, false),
            receipt: self.receipt.clone(),
            overlapped: Box::default(),
            buffer: vec![0; 128 * 1024],
            pending: false,
        };
        watcher.arm()?;
        Ok(watcher)
    }
}
impl DirectoryWatcher {
    fn arm(&mut self) -> Result<()> {
        *self.overlapped = OVERLAPPED {
            hEvent: self.event.0,
            ..Default::default()
        };
        if unsafe { ResetEvent(self.event.0) } == 0 {
            return Err("cannot reset watcher event".into());
        }
        let result = unsafe {
            ReadDirectoryChangesExW(
                self.directory.0,
                self.buffer.as_mut_ptr().cast(),
                (self.buffer.len() * 8) as u32,
                1,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_DIR_NAME
                    | FILE_NOTIFY_CHANGE_SIZE
                    | FILE_NOTIFY_CHANGE_LAST_WRITE
                    | FILE_NOTIFY_CHANGE_ATTRIBUTES,
                ptr::null_mut(),
                &mut *self.overlapped,
                None,
                ReadDirectoryNotifyExtendedInformation,
            )
        };
        if result == 0 && unsafe { GetLastError() } != ERROR_IO_PENDING {
            return Err(format!("directory watch could not start: {}", unsafe {
                GetLastError()
            }));
        }
        self.pending = true;
        Ok(())
    }
    /// At most one second per poll. Persist returned batches before performing
    /// cloud work, and start with a fresh inventory after process restart.
    pub fn poll(&mut self, timeout_ms: u32) -> Result<Option<DirectoryChangeBatch>> {
        if timeout_ms > 1000 {
            return Err("bounded watcher poll required".into());
        }
        let (id, identity) = root_identity(self.directory.0)?;
        if id != self.receipt.file_id || identity != self.receipt.identity {
            return Err("watcher registration identity changed".into());
        }
        let status = unsafe { WaitForSingleObject(self.event.0, timeout_ms) };
        if status == WAIT_TIMEOUT {
            return Ok(None);
        }
        if status != WAIT_OBJECT_0 {
            return Err("directory watcher wait failed".into());
        }
        let mut count = 0;
        let result =
            unsafe { GetOverlappedResult(self.directory.0, &*self.overlapped, &mut count, 0) };
        if result == 0 && unsafe { GetLastError() } == ERROR_IO_INCOMPLETE {
            return Ok(None);
        }
        self.pending = false;
        let batch = if result == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_NOTIFY_ENUM_DIR {
                Ok(DirectoryChangeBatch {
                    rescan_required: true,
                    changes: Vec::new(),
                })
            } else {
                Err(format!("directory watcher completion: {error}"))
            }
        } else if count as usize > self.buffer.len() * 8 {
            Err("oversized directory watcher completion".into())
        } else {
            decode(unsafe {
                std::slice::from_raw_parts(self.buffer.as_ptr().cast::<u8>(), count as usize)
            })
        };
        // Rearm before handing the batch to slow consumers. The kernel retains
        // intervening changes, and explicit overflow remains a rescan marker.
        self.arm()?;
        batch.map(Some)
    }
}
impl Drop for DirectoryWatcher {
    fn drop(&mut self) {
        if self.pending {
            unsafe { CancelIoEx(self.directory.0, &*self.overlapped) };
            let mut ignored = 0;
            // The OS must finish referencing OVERLAPPED/buffer before they drop.
            unsafe { GetOverlappedResult(self.directory.0, &*self.overlapped, &mut ignored, 1) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(name: &str) -> Vec<u8> {
        let wide = name.encode_utf16().collect::<Vec<_>>();
        let mut data = vec![0; 84 + wide.len() * 2];
        data[4..8].copy_from_slice(&1u32.to_le_bytes());
        data[64..72].copy_from_slice(&123u64.to_le_bytes());
        data[80..84].copy_from_slice(&((wide.len() * 2) as u32).to_le_bytes());
        for (i, value) in wide.iter().enumerate() {
            data[84 + i * 2..86 + i * 2].copy_from_slice(&value.to_le_bytes());
        }
        data
    }
    #[test]
    fn notification_parser_preserves_ids_and_rejects_truncation_escape_and_bad_offsets() {
        let data = record("目录\\文件.txt");
        let batch = decode(&data).unwrap();
        assert_eq!(batch.changes[0].file_id, 123);
        assert_eq!(batch.changes[0].path, "目录\\文件.txt");
        assert!(decode(&[]).unwrap().rescan_required);
        for length in 1..data.len() {
            assert!(decode(&data[..length]).is_err());
        }
        assert!(decode(&record("..\\escape")).is_err());
        assert!(decode(&record("C:\\outside")).is_err());
        let mut broken = data.clone();
        broken[..4].copy_from_slice(&4u32.to_le_bytes());
        assert!(decode(&broken).is_err());
        let mut odd = data;
        odd[80..84].copy_from_slice(&3u32.to_le_bytes());
        assert!(decode(&odd).is_err());
    }
    #[test]
    #[ignore = "explicit NEW generated CFAPI root; ordinary create/write/rename/delete notifications"]
    #[cfg(feature = "native-test-hooks")]
    fn ordinary_changes_keep_file_ids_and_pending_watches_cancel_cleanly() {
        struct NoData;
        impl HydrationSource for NoData {
            fn size(&self, _: &[u8]) -> Result<usize> {
                Err("no content in watcher fixture".into())
            }
            fn read_range(&self, _: &[u8], _: usize, _: usize, _: &AtomicBool) -> Result<Vec<u8>> {
                Err("no content".into())
            }
        }
        let root = PathBuf::from(
            std::env::var_os("TWODRIVE_APPROVED_WATCHER_ROOT")
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
        let mut owner = PersistentSession::create_new(
            &root,
            b"twodrive-dev-native-v1:generated-office-only",
            &checkpoint,
            Arc::new(NoData),
        )
        .unwrap();
        let private = tempfile::tempdir().unwrap();
        let db = twodrive_core::Database::new(private.path().join("watch.sqlite3"));
        db.init().unwrap();
        let mut watch = owner.watch_directory_durable(db.clone()).unwrap();
        let first = watch.state().unwrap();
        assert!(first.rescan_required);
        assert!(
            db.complete_native_watch_scan(watch.binding(), first)
                .unwrap()
        );
        let original = root.join("created-中文.txt");
        let renamed = root.join("renamed-中文.txt");
        std::fs::write(&original, b"generated ordinary file").unwrap();
        std::fs::rename(&original, &renamed).unwrap();
        std::fs::remove_file(&renamed).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut changes = Vec::new();
        while std::time::Instant::now() < deadline
            && !changes
                .iter()
                .any(|event: &DirectoryChange| event.action == FILE_ACTION_REMOVED)
        {
            if let Some(state) = watch.poll(100).unwrap() {
                assert!(!state.rescan_required);
                changes = db
                    .pending_native_observations(watch.binding(), 1000)
                    .unwrap()
                    .iter()
                    .map(|(_, _, payload)| {
                        serde_json::from_str::<DirectoryChange>(payload).unwrap()
                    })
                    .collect();
            }
        }
        for action in [
            FILE_ACTION_ADDED,
            FILE_ACTION_RENAMED_OLD_NAME,
            FILE_ACTION_RENAMED_NEW_NAME,
            FILE_ACTION_REMOVED,
        ] {
            assert!(
                changes.iter().any(|event| event.action == action),
                "missing ordinary action {action}"
            );
        }
        let id = changes
            .iter()
            .find(|event| event.action == FILE_ACTION_ADDED)
            .unwrap()
            .file_id;
        assert!(
            changes
                .iter()
                .all(|event| event.file_id == id && event.parent_file_id == owner.receipt.file_id)
        );
        assert!(changes.iter().any(|event| event.path == "renamed-中文.txt"));
        assert_eq!(
            owner.source.as_ref().unwrap().calls.load(Ordering::SeqCst),
            0
        );
        let binding = watch.binding().to_string();
        let saved = db.pending_native_observations(&binding, 1000).unwrap();
        drop(watch);
        let reopened = twodrive_core::Database::new(db.path());
        reopened.init().unwrap();
        let watch = owner.watch_directory_durable(reopened.clone()).unwrap();
        assert!(watch.state().unwrap().rescan_required);
        assert_eq!(
            reopened
                .pending_native_observations(&binding, 1000)
                .unwrap(),
            saved
        );
        drop(watch);
        owner.retire_empty_office_fixture().unwrap();
        drop(owner);
        std::fs::remove_dir(&root).unwrap();
        std::fs::remove_file(checkpoint).unwrap();
    }
}
