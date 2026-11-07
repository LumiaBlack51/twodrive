//! Persistent CFAPI registration. This platform layer owns no cloud credentials
//! and performs no adoption: new roots must not exist; resume requires the exact
//! registration identity, volume and file ID saved by the engine.
use super::*;
use serde::{Deserialize, Serialize};
#[path = "native_namespace.rs"]
mod namespace;
pub use namespace::NativeEntry;
#[path = "native_events.rs"]
pub(crate) mod events;
pub use events::{DatabaseEventSink, NativeEvent, NativeEventKind, NativeEventSink};

pub const ROOT_IDENTITY_PREFIX: &[u8] = b"twodrive-dev-native-v1:";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootReceipt {
    pub schema_version: u32,
    pub volume_serial: u32,
    pub file_id: u64,
    pub identity: Vec<u8>,
}

pub struct PersistentSession {
    guard: RootGuard,
    receipt: RootReceipt,
    source: Option<Box<Source>>,
    connection: Option<CF_CONNECTION_KEY>,
}

fn valid_identity(identity: &[u8]) -> Result<()> {
    if !identity.starts_with(ROOT_IDENTITY_PREFIX)
        || identity.len() <= ROOT_IDENTITY_PREFIX.len()
        || identity.len() > 4096
    {
        return Err("invalid Development native root identity".into());
    }
    Ok(())
}

fn root_identity(handle: HANDLE) -> Result<(u64, Vec<u8>)> {
    let mut buffer = [0u64; 1024];
    let mut returned = 0;
    hr(
        unsafe {
            CfGetSyncRootInfoByHandle(
                handle,
                CF_SYNC_ROOT_INFO_STANDARD,
                buffer.as_mut_ptr().cast(),
                size_of_val(&buffer) as u32,
                &mut returned,
            )
        },
        "query owned root",
    )?;
    let start = offset_of!(CF_SYNC_ROOT_STANDARD_INFO, SyncRootIdentity);
    if (returned as usize) < start {
        return Err("short root identity".into());
    }
    let info = unsafe { &*buffer.as_ptr().cast::<CF_SYNC_ROOT_STANDARD_INFO>() };
    let len = info.SyncRootIdentityLength as usize;
    if len > 4096
        || start
            .checked_add(len)
            .is_none_or(|end| end > returned as usize)
    {
        return Err("invalid root identity length".into());
    }
    let identity =
        unsafe { std::slice::from_raw_parts(info.SyncRootIdentity.as_ptr(), len) }.to_vec();
    Ok((info.SyncRootFileId as u64, identity))
}

impl PersistentSession {
    /// Persist the exact filesystem binding BEFORE registration or connection.
    /// The checkpoint must be a new file outside the new root. Keep it with the
    /// account state; failed startup never unregisters or deletes user content.
    pub fn create_new(
        root: &Path,
        identity: &[u8],
        checkpoint: &Path,
        engine: Arc<dyn HydrationSource>,
    ) -> Result<Self> {
        let (guard, receipt) = Self::prepare_new(root, identity, checkpoint)?;
        Self::register_owned(&guard, &receipt.identity)?;
        Self::connect_owned(guard, receipt, engine)
    }

    pub fn create_new_recording(
        root: &Path,
        identity: &[u8],
        checkpoint: &Path,
        engine: Arc<dyn HydrationSource>,
        sink: Arc<dyn NativeEventSink>,
    ) -> Result<Self> {
        let (guard, receipt) = Self::prepare_new(root, identity, checkpoint)?;
        Self::register_owned(&guard, &receipt.identity)?;
        Self::connect_recording(guard, receipt, engine, Some(sink))
    }

    fn prepare_new(
        root: &Path,
        identity: &[u8],
        checkpoint: &Path,
    ) -> Result<(RootGuard, RootReceipt)> {
        use std::io::Write;
        use std::os::windows::fs::OpenOptionsExt;
        valid_identity(identity)?;
        let mut guard = RootGuard::lock_parent(root)?;
        guard.require_unmanaged_parent()?;
        // Validate the checkpoint chain before creating any native root. It
        // cannot be nested below the root, which does not yet exist.
        let checkpoint_guard = RootGuard::lock_parent(checkpoint)?;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_WRITE_THROUGH)
            .open(&checkpoint_guard.root)
            .map_err(|e| e.to_string())?;
        guard.create_leaf()?;
        let handle = guard._handles.last().ok_or("missing root handle")?.0;
        let (volume_serial, file_id, directory) = file_binding(handle)?;
        if !directory {
            return Err("native root must be a directory".into());
        }
        let receipt = RootReceipt {
            schema_version: 1,
            volume_serial,
            file_id,
            identity: identity.to_vec(),
        };
        let bytes = serde_json::to_vec(&receipt).map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        Ok((guard, receipt))
    }

    fn register_owned(guard: &RootGuard, identity: &[u8]) -> Result<()> {
        let name = wide(Path::new("TwoDrive Development"));
        let version = wide(Path::new(env!("CARGO_PKG_VERSION")));
        let registration = CF_SYNC_REGISTRATION {
            StructSize: size_of::<CF_SYNC_REGISTRATION>() as u32,
            ProviderName: name.as_ptr(),
            ProviderVersion: version.as_ptr(),
            SyncRootIdentity: identity.as_ptr().cast(),
            SyncRootIdentityLength: identity.len() as u32,
            ..Default::default()
        };
        let policies = CF_SYNC_POLICIES {
            StructSize: size_of::<CF_SYNC_POLICIES>() as u32,
            Hydration: CF_HYDRATION_POLICY {
                Primary: CF_HYDRATION_POLICY_FULL,
                Modifier: CF_HYDRATION_POLICY_MODIFIER_NONE,
            },
            Population: CF_POPULATION_POLICY {
                Primary: CF_POPULATION_POLICY_ALWAYS_FULL,
                Modifier: CF_POPULATION_POLICY_MODIFIER_NONE,
            },
            InSync: CF_INSYNC_POLICY_TRACK_ALL,
            HardLink: CF_HARDLINK_POLICY_NONE,
            PlaceholderManagement: CF_PLACEHOLDER_MANAGEMENT_POLICY_DEFAULT,
        };
        hr(
            unsafe {
                CfRegisterSyncRoot(
                    wide(&guard.root).as_ptr(),
                    &registration,
                    &policies,
                    CF_REGISTER_FLAG_NONE,
                )
            },
            "register new Development root",
        )?;
        Ok(())
    }

    /// Resume an interrupted first registration as well as a normal restart.
    /// Registration is only completed for the exact empty ordinary directory
    /// recorded before the first attempt. Foreign/replaced roots fail closed.
    pub fn resume_from_checkpoint(
        root: &Path,
        checkpoint: &Path,
        engine: Arc<dyn HydrationSource>,
    ) -> Result<Self> {
        Self::resume_checkpoint(root, checkpoint, engine, None)
    }

    pub fn resume_from_checkpoint_recording(
        root: &Path,
        checkpoint: &Path,
        engine: Arc<dyn HydrationSource>,
        sink: Arc<dyn NativeEventSink>,
    ) -> Result<Self> {
        Self::resume_checkpoint(root, checkpoint, engine, Some(sink))
    }

    fn resume_checkpoint(
        root: &Path,
        checkpoint: &Path,
        engine: Arc<dyn HydrationSource>,
        sink: Option<Arc<dyn NativeEventSink>>,
    ) -> Result<Self> {
        use std::io::Read;
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        let checkpoint_guard = RootGuard::lock_parent(checkpoint)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&checkpoint_guard.root)
            .map_err(|e| e.to_string())?;
        if file
            .metadata()
            .map_err(|e| e.to_string())?
            .file_attributes()
            & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY)
            != 0
        {
            return Err("checkpoint must be an ordinary retained file".into());
        }
        let mut bytes = Vec::new();
        file.take(65537)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 65536 {
            return Err("oversized root checkpoint".into());
        }
        let receipt = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        Self::resume_owned(root, &receipt, engine, true, sink)
    }

    pub fn resume(
        root: &Path,
        receipt: &RootReceipt,
        engine: Arc<dyn HydrationSource>,
    ) -> Result<Self> {
        Self::resume_owned(root, receipt, engine, false, None)
    }

    fn resume_owned(
        root: &Path,
        receipt: &RootReceipt,
        engine: Arc<dyn HydrationSource>,
        finish_registration: bool,
        sink: Option<Arc<dyn NativeEventSink>>,
    ) -> Result<Self> {
        valid_identity(&receipt.identity)?;
        if receipt.schema_version != 1 {
            return Err("unsupported native root receipt".into());
        }
        let mut guard = RootGuard::lock_parent(root)?;
        guard.require_unmanaged_parent()?;
        let raw = unsafe {
            CreateFileW(
                wide(root).as_ptr(),
                FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err("native root is unavailable".into());
        }
        let handle = Handle(raw, false);
        let (volume, id, directory) = file_binding(handle.0)?;
        let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
        if unsafe {
            GetFileInformationByHandleEx(
                handle.0,
                FileAttributeTagInfo,
                (&mut tag as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                size_of_val(&tag) as u32,
            )
        } == 0
            || !directory
            || volume != receipt.volume_serial
            || id != receipt.file_id
            || tag.ReparseTag & 0x20000000 != 0
        {
            return Err("native root filesystem identity changed".into());
        }
        let mut basic = CF_SYNC_ROOT_BASIC_INFO::default();
        let query = unsafe {
            CfGetSyncRootInfoByHandle(
                handle.0,
                CF_SYNC_ROOT_INFO_BASIC,
                (&mut basic as *mut CF_SYNC_ROOT_BASIC_INFO).cast(),
                size_of_val(&basic) as u32,
                ptr::null_mut(),
            )
        };
        let needs_registration = if query >= 0 {
            let (registered_id, identity) = root_identity(handle.0)?;
            if id != registered_id || identity != receipt.identity {
                return Err("native root registration identity changed".into());
            }
            false
        } else {
            require_unmanaged(query)?;
            if !finish_registration
                || tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
                || std::fs::read_dir(root)
                    .map_err(|e| e.to_string())?
                    .next()
                    .is_some()
            {
                return Err(
                    "interrupted registration requires the original empty ordinary root".into(),
                );
            }
            true
        };
        guard._handles.push(handle);
        guard.root = root.canonicalize().map_err(|e| e.to_string())?;
        if needs_registration {
            Self::register_owned(&guard, &receipt.identity)?;
        }
        Self::connect_recording(guard, receipt.clone(), engine, sink)
    }

    fn connect_owned(
        guard: RootGuard,
        receipt: RootReceipt,
        engine: Arc<dyn HydrationSource>,
    ) -> Result<Self> {
        Self::connect_recording(guard, receipt, engine, None)
    }

    fn connect_recording(
        guard: RootGuard,
        receipt: RootReceipt,
        engine: Arc<dyn HydrationSource>,
        sink: Option<Arc<dyn NativeEventSink>>,
    ) -> Result<Self> {
        let source = Box::new(Source {
            engine: Some(engine),
            events: sink.map(|sink| events::Context::new(receipt.clone(), sink)),
            ..Default::default()
        });
        let mut table = vec![
            CF_CALLBACK_REGISTRATION {
                Type: CF_CALLBACK_TYPE_FETCH_DATA,
                Callback: Some(fetch),
            },
            CF_CALLBACK_REGISTRATION {
                Type: CF_CALLBACK_TYPE_CANCEL_FETCH_DATA,
                Callback: Some(cancel),
            },
            // Automatic dehydration remains refused until the durable owner
            // authorizes it with cache/USN/open-handle evidence.
            CF_CALLBACK_REGISTRATION {
                Type: CF_CALLBACK_TYPE_NOTIFY_DEHYDRATE,
                Callback: Some(deny_dehydrate),
            },
        ];
        if source.events.is_some() {
            table.extend([
                CF_CALLBACK_REGISTRATION {
                    Type: CF_CALLBACK_TYPE_NOTIFY_DELETE,
                    Callback: Some(events::delete_requested),
                },
                CF_CALLBACK_REGISTRATION {
                    Type: CF_CALLBACK_TYPE_NOTIFY_DELETE_COMPLETION,
                    Callback: Some(events::delete_completed),
                },
                CF_CALLBACK_REGISTRATION {
                    Type: CF_CALLBACK_TYPE_NOTIFY_RENAME,
                    Callback: Some(events::rename_requested),
                },
                CF_CALLBACK_REGISTRATION {
                    Type: CF_CALLBACK_TYPE_NOTIFY_RENAME_COMPLETION,
                    Callback: Some(events::rename_completed),
                },
            ]);
        }
        table.push(CF_CALLBACK_REGISTRATION {
            Type: CF_CALLBACK_TYPE_NONE,
            Callback: None,
        });
        let mut key = 0;
        hr(
            unsafe {
                CfConnectSyncRoot(
                    wide(&guard.root).as_ptr(),
                    table.as_ptr(),
                    (&*source as *const Source).cast(),
                    CF_CONNECT_FLAG_NONE,
                    &mut key,
                )
            },
            "connect owned Development root",
        )?;
        Ok(Self {
            guard,
            receipt,
            source: Some(source),
            connection: Some(key),
        })
    }

    pub fn receipt(&self) -> &RootReceipt {
        &self.receipt
    }
    pub fn root(&self) -> &Path {
        &self.guard.root
    }

    /// Publish verified cloud metadata without downloading content. Never
    /// replaces a file; every existing parent must be an owned cloud directory.
    pub fn publish_placeholder(
        &self,
        relative: &Path,
        identity: &[u8],
        size: u64,
        directory: bool,
        modified_unix: i64,
    ) -> Result<PathBuf> {
        if self.connection.is_none()
            || identity.is_empty()
            || identity.len() > 4096
            || size > i64::MAX as u64
        {
            return Err("invalid native publication".into());
        }
        let (parent, _guards) = self.guard_relative_parent(relative)?;
        let name = wide(Path::new(
            relative.file_name().ok_or("missing native leaf")?,
        ));
        let mut item = CF_PLACEHOLDER_CREATE_INFO {
            RelativeFileName: name.as_ptr(),
            FileIdentity: identity.as_ptr().cast(),
            FileIdentityLength: identity.len() as u32,
            Flags: CF_PLACEHOLDER_CREATE_FLAG_MARK_IN_SYNC
                | if directory {
                    CF_PLACEHOLDER_CREATE_FLAG_DISABLE_ON_DEMAND_POPULATION
                } else {
                    0
                },
            ..Default::default()
        };
        item.FsMetadata.FileSize = if directory { 0 } else { size as i64 };
        item.FsMetadata.BasicInfo.FileAttributes = if directory {
            FILE_ATTRIBUTE_DIRECTORY
        } else {
            FILE_ATTRIBUTE_NORMAL
        };
        item.FsMetadata.BasicInfo.LastWriteTime = modified_unix
            .checked_add(11_644_473_600)
            .and_then(|v| v.checked_mul(10_000_000))
            .unwrap_or(0)
            .max(0);
        let mut done = 0;
        hr(
            unsafe {
                CfCreatePlaceholders(
                    wide(&parent).as_ptr(),
                    &mut item,
                    1,
                    CF_CREATE_FLAG_STOP_ON_ERROR,
                    &mut done,
                )
            },
            "publish native placeholder",
        )?;
        hr(item.Result, "publish native entry")?;
        if done != 1 {
            return Err("native placeholder was not published".into());
        }
        Ok(self.root().join(relative))
    }

    fn guard_relative_parent(&self, relative: &Path) -> Result<(PathBuf, Vec<Handle>)> {
        let parts = relative.components().collect::<Vec<_>>();
        if parts.is_empty()
            || parts.len() > 128
            || parts.iter().any(|p| !matches!(p, Component::Normal(_)))
        {
            return Err("bounded relative native path required".into());
        }
        for part in &parts {
            let name = part.as_os_str().to_str().ok_or("invalid native name")?;
            let stem = name
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            if name.is_empty()
                || name.ends_with(['.', ' '])
                || name.encode_utf16().count() > 255
                || name.chars().any(|c| c < ' ' || "<>:\"/\\|?*".contains(c))
                || ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
                || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                    && stem.len() == 4
                    && matches!(stem.as_bytes()[3], b'1'..=b'9'))
            {
                return Err("unsupported native file name".into());
            }
        }
        let mut parent = self.root().to_path_buf();
        let mut guards = Vec::new();
        for part in &parts[..parts.len() - 1] {
            parent.push(part.as_os_str());
            let handle = directory_handle(&parent)?;
            let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
            if unsafe {
                GetFileInformationByHandleEx(
                    handle.0,
                    FileAttributeTagInfo,
                    (&mut tag as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                    size_of_val(&tag) as u32,
                )
            } == 0
                || tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
                || tag.ReparseTag & 0x20000000 != 0
            {
                return Err("native parent is not an owned directory".into());
            }
            let (root_id, owner) = root_identity(handle.0)?;
            if root_id != self.receipt.file_id || owner != self.receipt.identity {
                return Err("native parent owner changed".into());
            }
            let mut bytes = [0u64; 1024];
            let mut count = 0;
            hr(
                unsafe {
                    CfGetPlaceholderInfo(
                        handle.0,
                        CF_PLACEHOLDER_INFO_STANDARD,
                        bytes.as_mut_ptr().cast(),
                        size_of_val(&bytes) as u32,
                        &mut count,
                    )
                },
                "verify native parent placeholder",
            )?;
            if count < size_of::<CF_PLACEHOLDER_STANDARD_INFO>() as u32 {
                return Err("short parent placeholder information".into());
            }
            guards.push(handle);
        }
        Ok((parent, guards))
    }

    /// Capture a local atomic replacement within this owned root. Conversion
    /// retains all resident bytes and never marks them in sync or dehydrates.
    /// The engine must persist its edit intent and account/version binding first.
    pub fn with_replacement_capture<T>(
        &self,
        relative: &Path,
        identity: &[u8],
        action: impl FnOnce(&NativeCapture<'_>) -> Result<T>,
    ) -> Result<T> {
        if self.connection.is_none() || identity.is_empty() || identity.len() > 4096 {
            return Err("invalid native replacement capture".into());
        }
        let (parent, _guards) = self.guard_relative_parent(relative)?;
        let path = parent.join(relative.file_name().ok_or("missing native leaf")?);
        let mut lease = RetainedPlaceholder::open_node(&path, identity, 0, false)?;
        let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe {
            GetFileInformationByHandleEx(
                lease.handle.0,
                FileAttributeTagInfo,
                (&mut tag as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                size_of_val(&tag) as u32,
            )
        } == 0
            || unsafe { GetFileInformationByHandle(lease.handle.0, &mut info) } == 0
            || tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0
            || tag.ReparseTag & 0x20000000 != 0
            || info.nNumberOfLinks != 1
            || info.dwVolumeSerialNumber != self.receipt.volume_serial
        {
            return Err("replacement is not a unique retained file in the owned volume".into());
        }
        let mut size = 0i64;
        if unsafe { GetFileSizeEx(lease.handle.0, &mut size) } == 0 || size < 0 {
            return Err("replacement size unavailable".into());
        }
        lease.size = usize::try_from(size).map_err(|_| "replacement size overflow")?;
        if tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
            if tag.FileAttributes
                & (FILE_ATTRIBUTE_OFFLINE
                    | FILE_ATTRIBUTE_RECALL_ON_OPEN
                    | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
                    | FILE_ATTRIBUTE_SPARSE_FILE
                    | FILE_ATTRIBUTE_ENCRYPTED)
                != 0
            {
                return Err("replacement is not an ordinary complete resident file".into());
            }
            hr(
                unsafe {
                    CfConvertToPlaceholder(
                        lease.handle.0,
                        identity.as_ptr().cast(),
                        identity.len() as u32,
                        CF_CONVERT_FLAG_NONE,
                        ptr::null_mut(),
                        ptr::null_mut(),
                    )
                },
                "retain replacement as dirty native placeholder",
            )?;
        }
        // Also permits retry after conversion succeeded but journal publication
        // failed. A foreign/unknown reparse point cannot pass both checks.
        let (root_id, owner) = root_identity(lease.handle.0)?;
        if root_id != self.receipt.file_id || owner != self.receipt.identity {
            return Err("replacement root ownership changed".into());
        }
        let state = lease.inspect()?;
        if !state.resident || state.partial || state.logical_size != size as u64 {
            return Err("replacement does not contain all local bytes".into());
        }
        action(&NativeCapture(&lease))
    }

    #[cfg(feature = "native-test-hooks")]
    pub fn retire_empty_office_fixture(&mut self) -> Result<()> {
        if self.receipt.identity != b"twodrive-dev-native-v1:generated-office-only"
            || !self.root().file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .starts_with("twodrive-cfapi-fixture-office-")
            })
            || std::fs::read_dir(self.root())
                .map_err(|e| e.to_string())?
                .next()
                .is_some()
        {
            return Err("only an empty owned Office fixture may be retired".into());
        }
        let (id, identity) =
            root_identity(self.guard._handles.last().ok_or("root lease missing")?.0)?;
        if id != self.receipt.file_id || identity != self.receipt.identity {
            return Err("fixture ownership changed".into());
        }
        self.disconnect()?;
        hr(
            unsafe { CfUnregisterSyncRoot(wide(self.root()).as_ptr()) },
            "retire empty Office fixture",
        )?;
        verify_unregistered(self.root())
    }

    pub fn disconnect(&mut self) -> Result<()> {
        if let Some(source) = &self.source {
            source.stopping.store(true, Ordering::SeqCst);
            if let Ok(active) = source.active.lock() {
                for flag in active.values() {
                    flag.store(true, Ordering::SeqCst);
                }
            }
        }
        if let Some(key) = self.connection {
            hr(
                unsafe { CfDisconnectSyncRoot(key) },
                "disconnect persistent root",
            )?;
            self.connection = None;
        }
        Ok(())
    }
}
impl Drop for PersistentSession {
    fn drop(&mut self) {
        if self.disconnect().is_err() {
            // An OS callback may still retain its context on disconnect failure.
            // Keep that context alive until process exit; never free a live pointer.
            if let Some(source) = self.source.take() {
                let _ = Box::leak(source);
            }
        }
        // Intentionally no CfUnregisterSyncRoot, file conversion or deletion.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct NoCloud;
    impl HydrationSource for NoCloud {
        fn size(&self, _: &[u8]) -> Result<usize> {
            Err("no cloud".into())
        }
        fn read_range(&self, _: &[u8], _: usize, _: usize, _: &AtomicBool) -> Result<Vec<u8>> {
            Err("no cloud".into())
        }
    }

    #[test]
    #[ignore = "explicit NEW generated native roots; interrupted registration and actual connection failure"]
    fn durable_checkpoint_recovers_interrupted_registration_and_first_connection() {
        let base = PathBuf::from(
            std::env::var_os("TWODRIVE_APPROVED_PERSISTENT_FIXTURE_ROOT")
                .expect("explicit root required"),
        );
        assert!(
            base.is_absolute()
                && base
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("twodrive-cfapi-fixture-")
        );
        for registered in [false, true] {
            let root = base.with_extension(if registered { "registered" } else { "prepared" });
            let checkpoint = root.with_extension(if registered {
                "registered.json"
            } else {
                "prepared.json"
            });
            assert!(!root.exists() && !checkpoint.exists());
            let (guard, receipt) = PersistentSession::prepare_new(
                &root,
                b"twodrive-dev-native-v1:interrupted-generated-test",
                &checkpoint,
            )
            .unwrap();
            assert_eq!(
                serde_json::from_slice::<RootReceipt>(&std::fs::read(&checkpoint).unwrap())
                    .unwrap(),
                receipt
            );
            if registered {
                PersistentSession::register_owned(&guard, &receipt.identity).unwrap();
                let blocker =
                    PersistentSession::connect_owned(guard, receipt.clone(), Arc::new(NoCloud))
                        .unwrap();
                // The real OS refuses a second connection. Its failure must not
                // remove the root, registration or already-durable receipt.
                assert!(
                    PersistentSession::resume_from_checkpoint(
                        &root,
                        &checkpoint,
                        Arc::new(NoCloud)
                    )
                    .is_err()
                );
                assert!(root.exists() && checkpoint.exists());
                drop(blocker);
            } else {
                drop(guard); // Process interruption before registration.
            }
            let mut resumed =
                PersistentSession::resume_from_checkpoint(&root, &checkpoint, Arc::new(NoCloud))
                    .unwrap();
            assert_eq!(resumed.receipt(), &receipt);
            resumed.disconnect().unwrap();
            drop(resumed);
            let mut wrong = receipt.clone();
            wrong.file_id = wrong.file_id.wrapping_add(1);
            std::fs::write(&checkpoint, serde_json::to_vec(&wrong).unwrap()).unwrap();
            assert!(
                PersistentSession::resume_from_checkpoint(&root, &checkpoint, Arc::new(NoCloud))
                    .is_err()
            );
            wrong = receipt;
            wrong.identity.extend_from_slice(b":foreign");
            std::fs::write(&checkpoint, serde_json::to_vec(&wrong).unwrap()).unwrap();
            assert!(
                PersistentSession::resume_from_checkpoint(&root, &checkpoint, Arc::new(NoCloud))
                    .is_err()
            );
            // Only these two empty, newly generated roots are retired.
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
            hr(
                unsafe { CfUnregisterSyncRoot(wide(&root).as_ptr()) },
                "retire interrupted fixture",
            )
            .unwrap();
            verify_unregistered(&root).unwrap();
            std::fs::remove_dir(&root).unwrap();
            std::fs::remove_file(&checkpoint).unwrap();
        }
    }
    #[test]
    fn foreign_and_unversioned_root_identities_are_not_accepted() {
        for id in [
            b"".as_slice(),
            b"OneDrive",
            ROOT_IDENTITY_PREFIX,
            b"twodrive-cfapi-generated-fixture-v1",
        ] {
            assert!(valid_identity(id).is_err());
        }
        valid_identity(b"twodrive-dev-native-v1:generated-test-binding").unwrap();
    }

    #[test]
    #[ignore = "explicit NEW generated native root; nested publication and junction refusal"]
    fn publication_accepts_owned_subdirectories_and_refuses_ordinary_or_junction_parents() {
        let root = PathBuf::from(
            std::env::var_os("TWODRIVE_APPROVED_PERSISTENT_FIXTURE_ROOT")
                .expect("explicit root required"),
        );
        assert!(
            root.is_absolute()
                && !root.exists()
                && root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("twodrive-cfapi-fixture-")
        );
        let checkpoint = root.with_extension("receipt.json");
        let outside = root.with_extension("outside");
        std::fs::create_dir(&outside).unwrap();
        let mut session = PersistentSession::create_new(
            &root,
            b"twodrive-dev-native-v1:nested-generated-test",
            &checkpoint,
            Arc::new(NoCloud),
        )
        .unwrap();
        session
            .publish_placeholder(Path::new("owned"), b"directory", 0, true, 1)
            .unwrap();
        let nested = session
            .publish_placeholder(Path::new("owned/empty.bin"), b"empty", 0, false, 1)
            .unwrap();
        std::fs::create_dir(root.join("ordinary")).unwrap();
        assert!(
            session
                .publish_placeholder(Path::new("ordinary/blocked.bin"), b"blocked", 0, false, 1)
                .is_err()
        );
        let linked = root.join("junction");
        let command = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&linked)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(
            command.status.success(),
            "generated junction creation failed"
        );
        assert!(
            session
                .publish_placeholder(Path::new("junction/blocked.bin"), b"blocked", 0, false, 1)
                .is_err()
        );
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(root.join("ordinary")).unwrap().count(), 0);
        assert!(nested.exists());
        // All entries were created in this test and are empty. remove_dir on
        // the junction removes the link itself, not its target.
        std::fs::remove_dir(&linked).unwrap();
        std::fs::remove_file(&nested).unwrap();
        std::fs::remove_dir(root.join("owned")).unwrap();
        std::fs::remove_dir(root.join("ordinary")).unwrap();
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        session.disconnect().unwrap();
        hr(
            unsafe { CfUnregisterSyncRoot(wide(&root).as_ptr()) },
            "retire nested fixture",
        )
        .unwrap();
        drop(session);
        verify_unregistered(&root).unwrap();
        std::fs::remove_dir(&root).unwrap();
        std::fs::remove_dir(&outside).unwrap();
        std::fs::remove_file(&checkpoint).unwrap();
    }

    #[test]
    #[ignore = "explicit NEW generated native root; preserves registration across disconnect/reconnect"]
    fn persistent_registration_retains_cached_and_online_only_files() {
        const BYTES: &[u8] = b"generated persistent native fixture";
        struct Generated;
        impl HydrationSource for Generated {
            fn size(&self, id: &[u8]) -> Result<usize> {
                if id != b"first" && id != b"offline" {
                    return Err("unknown generated identity".into());
                }
                Ok(BYTES.len())
            }
            fn read_range(
                &self,
                id: &[u8],
                offset: usize,
                len: usize,
                cancelled: &AtomicBool,
            ) -> Result<Vec<u8>> {
                self.size(id)?;
                if cancelled.load(Ordering::SeqCst) {
                    return Err("cancelled".into());
                }
                BYTES
                    .get(offset..offset + len)
                    .map(|b| b.to_vec())
                    .ok_or("invalid generated range".into())
            }
        }
        let root = PathBuf::from(
            std::env::var_os("TWODRIVE_APPROVED_PERSISTENT_FIXTURE_ROOT")
                .expect("explicit root required"),
        );
        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("twodrive-cfapi-fixture-")
        );
        assert!(root.is_absolute() && !root.exists());
        let session = PersistentSession::create_new(
            &root,
            b"twodrive-dev-native-v1:generated-test-binding",
            &root.with_extension("receipt.json"),
            Arc::new(Generated),
        )
        .unwrap();
        let receipt = session.receipt().clone();
        for id in ["first", "offline"] {
            session
                .publish_placeholder(
                    Path::new(&format!("{id}.bin")),
                    id.as_bytes(),
                    BYTES.len() as u64,
                    false,
                    1,
                )
                .unwrap();
        }
        for name in [
            "../escape.bin",
            "CON",
            "sub/../escape.bin",
            "file:stream",
            "trailing.",
        ] {
            assert!(
                session
                    .publish_placeholder(Path::new(name), b"invalid", 0, false, 1)
                    .is_err()
            );
        }
        assert!(
            session
                .publish_placeholder(Path::new("first.bin"), b"replacement", 0, false, 1)
                .is_err()
        );
        assert_eq!(
            session
                .source
                .as_ref()
                .unwrap()
                .calls
                .load(Ordering::SeqCst),
            0
        );
        assert_eq!(std::fs::read(root.join("first.bin")).unwrap(), BYTES);
        drop(session);
        assert_eq!(std::fs::read(root.join("first.bin")).unwrap(), BYTES);
        assert!(root.join("offline.bin").exists());
        assert!(std::fs::read(root.join("offline.bin")).is_err());
        let mut wrong = receipt.clone();
        wrong.file_id = wrong.file_id.wrapping_add(1);
        assert!(PersistentSession::resume(&root, &wrong, Arc::new(Generated)).is_err());
        let mut resumed = PersistentSession::resume(&root, &receipt, Arc::new(Generated)).unwrap();
        assert_eq!(std::fs::read(root.join("offline.bin")).unwrap(), BYTES);
        resumed.disconnect().unwrap();
        // Only this explicitly generated two-file fixture is retired. The
        // production type intentionally offers no unregister/delete operation.
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
        let mut retained = Vec::new();
        for id in ["first", "offline"] {
            let lease = RetainedPlaceholder::open_node(
                &root.join(format!("{id}.bin")),
                id.as_bytes(),
                BYTES.len(),
                false,
            )
            .unwrap();
            assert!(lease.inspect().unwrap().resident);
            retained.push(lease);
        }
        for lease in &retained {
            hr(
                unsafe {
                    CfRevertPlaceholder(lease.handle.0, CF_REVERT_FLAG_NONE, ptr::null_mut())
                },
                "retain generated file",
            )
            .unwrap();
        }
        drop(retained);
        hr(
            unsafe { CfUnregisterSyncRoot(wide(&root).as_ptr()) },
            "retire generated root",
        )
        .unwrap();
        verify_unregistered(&root).unwrap();
        for id in ["first", "offline"] {
            assert_eq!(
                std::fs::read(root.join(format!("{id}.bin"))).unwrap(),
                BYTES
            );
        }
    }
}
