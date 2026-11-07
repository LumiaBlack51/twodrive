//! Real CldApi adapter for generated fixtures only. Registration is explicitly opt-in.
use crate::fixtures::{BoundEntry, Dataset, RetentionManifest};
use crate::{LocalStatus, transfer_range};
use std::{
    collections::{HashMap, VecDeque},
    ffi::c_void,
    mem::{offset_of, size_of},
    os::windows::ffi::OsStrExt,
    path::{Component, Path, PathBuf, Prefix},
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};
use twodrive_core::FileState;
use windows_sys::Win32::{
    Foundation::*,
    Storage::{CloudFilters::*, FileSystem::*},
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_CREATE, FILE_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT,
            FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
        },
    },
    Win32::System::IO::IO_STATUS_BLOCK,
};

type Result<T> = std::result::Result<T, String>;
#[cfg(feature = "native-sync")]
#[path = "native_persistent.rs"]
pub mod persistent;
const ID: &[u8] = Dataset::StandardV1.root_identity();
const SIZE: usize = 65536;
const OFFLINE: i32 = 0xC000CF11u32 as i32;
const FAILED: i32 = 0xC000CF12u32 as i32;

struct RegisteredPlaceholder {
    path: PathBuf,
    identity: Vec<u8>,
    size: usize,
    reverted: bool,
    directory: bool,
}
#[cfg(feature = "native-test-hooks")]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TeardownStage {
    AllProtected,
    AllReverted,
}
#[cfg(feature = "native-test-hooks")]
type TeardownHook = Box<dyn FnMut(TeardownStage) -> Result<()> + Send>;

fn hr(value: i32, action: &str) -> Result<()> {
    if value < 0 {
        Err(format!("{action}: HRESULT 0x{:08X}", value as u32))
    } else {
        Ok(())
    }
}
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

pub fn payload() -> Vec<u8> {
    (0..SIZE).map(|i| (i % 251) as u8).collect()
}

pub fn platform() -> Result<CF_PLATFORM_INFO> {
    let mut info = CF_PLATFORM_INFO::default();
    hr(
        unsafe { CfGetPlatformInfo(&mut info) },
        "query CFAPI platform",
    )?;
    Ok(info)
}

/// Engine-owned opaque object source. No paths, Provider or database API crosses
/// this boundary. Each requested buffer is at most 64 KiB.
pub trait HydrationSource: Send + Sync {
    fn size(&self, identity: &[u8]) -> Result<usize>;
    fn read_range(
        &self,
        identity: &[u8],
        offset: usize,
        length: usize,
        cancelled: &AtomicBool,
    ) -> Result<Vec<u8>>;
}

#[derive(Default)]
struct Source {
    calls: AtomicUsize,
    failures: AtomicUsize,
    offline: AtomicBool,
    stopping: AtomicBool,
    engine: Option<Arc<dyn HydrationSource>>,
    #[cfg(feature = "native-sync")]
    events: Option<persistent::events::Context>,
    active: Mutex<HashMap<(i64, i64), Arc<AtomicBool>>>,
    requested_bytes: AtomicU64,
    transferred_bytes: AtomicU64,
    recent_requests: Mutex<VecDeque<serde_json::Value>>,
    dataset: Dataset,
}

// The OS owns callback pointers until CfDisconnectSyncRoot returns. Source is
// boxed for a stable address and outlives that call, including error unwinding.
unsafe extern "system" fn fetch(
    info: *const CF_CALLBACK_INFO,
    params: *const CF_CALLBACK_PARAMETERS,
) {
    // SAFETY: CldApi supplies valid pointers for the duration of this callback.
    let (info, params) = unsafe { (&*info, &*params) };
    let source = unsafe { &*(info.CallbackContext as *const Source) };
    source.calls.fetch_add(1, Ordering::SeqCst);
    let requested = unsafe { params.Anonymous.FetchData };
    let mut status = FAILED;
    let mut offset = requested.RequiredFileOffset;
    let mut length = requested.RequiredLength;
    let cancelled = Arc::new(AtomicBool::new(false));
    let key = (info.TransferKey, info.RequestKey);
    let mut claimed = false;
    let mut requested_bytes = 0u64;
    let mut transferred_bytes = 0u64;
    let mut observed_identity = "unknown".to_owned();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        if info.FileIdentity.is_null()
            || info.FileIdentityLength == 0
            || info.FileIdentityLength > 4096
        {
            return Err("invalid opaque identity".into());
        }
        let identity = unsafe {
            std::slice::from_raw_parts(
                info.FileIdentity.cast::<u8>(),
                info.FileIdentityLength as usize,
            )
        };
        let size = match &source.engine {
            Some(engine) => engine.size(identity)?,
            None if identity == ID => SIZE,
            None => return Err("unknown identity".into()),
        };
        let (start, end) = transfer_range(offset, length, size).ok_or("invalid range")?;
        observed_identity = source
            .dataset
            .files()
            .into_iter()
            .find(|entry| entry.id.as_bytes() == identity)
            .map(|entry| entry.id)
            .unwrap_or_else(|| "probe".into());
        requested_bytes = (end - start) as u64;
        source
            .requested_bytes
            .fetch_add((end - start) as u64, Ordering::SeqCst);
        {
            let mut active = source.active.lock().map_err(|_| "request gate poisoned")?;
            if active.len() >= 32 || active.contains_key(&key) {
                return Err("request capacity exceeded".into());
            }
            active.insert(key, cancelled.clone());
            claimed = true;
        }
        offset = start as i64;
        length = (end - start) as i64;
        for start in (start..end).step_by(65536) {
            if source.offline.load(Ordering::SeqCst)
                || source.stopping.load(Ordering::SeqCst)
                || cancelled.load(Ordering::SeqCst)
            {
                return Err("request unavailable or cancelled".into());
            }
            let count = (end - start).min(65536);
            let bytes = match &source.engine {
                Some(engine) => engine.read_range(identity, start, count, &cancelled)?,
                None => payload()[start..start + count].to_vec(),
            };
            if bytes.len() != count
                || cancelled.load(Ordering::SeqCst)
                || source.stopping.load(Ordering::SeqCst)
            {
                return Err("short or cancelled engine read".into());
            }
            execute_transfer(info, 0, start as i64, count as i64, &bytes)?;
            source
                .transferred_bytes
                .fetch_add(count as u64, Ordering::SeqCst);
            transferred_bytes += count as u64;
        }
        Ok(())
    }));
    if claimed && let Ok(mut active) = source.active.lock() {
        active.remove(&key);
    }
    let success = matches!(outcome, Ok(Ok(())));
    let caller_pid = if !info.ProcessInfo.is_null()
        && unsafe { (*info.ProcessInfo).StructSize as usize >= size_of::<CF_PROCESS_INFO>() }
    {
        Some(unsafe { (*info.ProcessInfo).ProcessId })
    } else {
        None
    };
    if let Ok(mut recent) = source.recent_requests.lock() {
        if recent.len() == 64 {
            recent.pop_front();
        }
        recent.push_back(serde_json::json!({"utc_ms":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|t|t.as_millis() as u64),"id":observed_identity,"caller_pid":caller_pid,"requested_bytes":requested_bytes,"transferred_bytes":transferred_bytes,"success":success}));
    }
    if success {
        return;
    }
    if source.offline.load(Ordering::SeqCst) {
        status = OFFLINE;
    }
    let _ = execute_transfer(info, status, offset, length, &[]);
    source.failures.fetch_add(1, Ordering::SeqCst);
}

unsafe extern "system" fn cancel(info: *const CF_CALLBACK_INFO, _: *const CF_CALLBACK_PARAMETERS) {
    let info = unsafe { &*info };
    let source = unsafe { &*(info.CallbackContext as *const Source) };
    if let Ok(active) = source.active.lock() {
        // A partial cancellation conservatively cancels this entire request.
        if let Some(flag) = active.get(&(info.TransferKey, info.RequestKey)) {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

fn execute_transfer(
    info: &CF_CALLBACK_INFO,
    status: i32,
    offset: i64,
    length: i64,
    bytes: &[u8],
) -> Result<()> {
    let operation = CF_OPERATION_INFO {
        StructSize: size_of::<CF_OPERATION_INFO>() as u32,
        Type: CF_OPERATION_TYPE_TRANSFER_DATA,
        ConnectionKey: info.ConnectionKey,
        TransferKey: info.TransferKey,
        RequestKey: info.RequestKey,
        CorrelationVector: info.CorrelationVector,
        ..Default::default()
    };
    let mut transfer = CF_OPERATION_PARAMETERS {
        ParamSize: (offset_of!(CF_OPERATION_PARAMETERS, Anonymous)
            + size_of::<CF_OPERATION_PARAMETERS_0_0>()) as u32,
        ..Default::default()
    };
    transfer.Anonymous.TransferData = CF_OPERATION_PARAMETERS_0_0 {
        Flags: CF_OPERATION_TRANSFER_DATA_FLAG_NONE,
        CompletionStatus: status,
        Buffer: if bytes.is_empty() {
            ptr::null()
        } else {
            bytes.as_ptr().cast()
        },
        Offset: offset,
        Length: length,
    };
    // CfExecute consumes this buffer synchronously; it cannot outlive bytes.
    hr(
        unsafe { CfExecute(&operation, &mut transfer) },
        "transfer engine data",
    )
}

unsafe extern "system" fn deny_delete(
    info: *const CF_CALLBACK_INFO,
    _: *const CF_CALLBACK_PARAMETERS,
) {
    deny_mutation(unsafe { &*info }, CF_OPERATION_TYPE_ACK_DELETE);
}
unsafe extern "system" fn deny_rename(
    info: *const CF_CALLBACK_INFO,
    _: *const CF_CALLBACK_PARAMETERS,
) {
    deny_mutation(unsafe { &*info }, CF_OPERATION_TYPE_ACK_RENAME);
}
unsafe extern "system" fn deny_dehydrate(
    info: *const CF_CALLBACK_INFO,
    _: *const CF_CALLBACK_PARAMETERS,
) {
    deny_mutation(unsafe { &*info }, CF_OPERATION_TYPE_ACK_DEHYDRATE);
}
fn deny_mutation(info: &CF_CALLBACK_INFO, kind: CF_OPERATION_TYPE) {
    let _ = acknowledge_mutation(info, kind, 0xC0000022u32 as i32);
}
fn acknowledge_mutation(
    info: &CF_CALLBACK_INFO,
    kind: CF_OPERATION_TYPE,
    status: i32,
) -> Result<()> {
    let operation = CF_OPERATION_INFO {
        StructSize: size_of::<CF_OPERATION_INFO>() as u32,
        Type: kind,
        ConnectionKey: info.ConnectionKey,
        TransferKey: info.TransferKey,
        RequestKey: info.RequestKey,
        CorrelationVector: info.CorrelationVector,
        ..Default::default()
    };
    let mut parameters = CF_OPERATION_PARAMETERS::default();
    let size = match kind {
        CF_OPERATION_TYPE_ACK_DELETE => {
            parameters.Anonymous.AckDelete = CF_OPERATION_PARAMETERS_0_7 {
                Flags: 0,
                CompletionStatus: status,
            };
            size_of::<CF_OPERATION_PARAMETERS_0_7>()
        }
        CF_OPERATION_TYPE_ACK_RENAME => {
            parameters.Anonymous.AckRename = CF_OPERATION_PARAMETERS_0_6 {
                Flags: 0,
                CompletionStatus: status,
            };
            size_of::<CF_OPERATION_PARAMETERS_0_6>()
        }
        _ => {
            parameters.Anonymous.AckDehydrate = CF_OPERATION_PARAMETERS_0_5 {
                CompletionStatus: status,
                ..Default::default()
            };
            size_of::<CF_OPERATION_PARAMETERS_0_5>()
        }
    };
    parameters.ParamSize = (offset_of!(CF_OPERATION_PARAMETERS, Anonymous) + size) as u32;
    hr(
        unsafe { CfExecute(&operation, &mut parameters) },
        "acknowledge native mutation",
    )
}

pub struct Session {
    root: PathBuf,
    registered: bool,
    connection: Option<CF_CONNECTION_KEY>,
    source: Box<Source>,
    _guard: RootGuard,
    placeholders: Mutex<Vec<RegisteredPlaceholder>>,
    teardown_started: bool,
    dataset: Dataset,
    directory_guards: Mutex<Vec<Handle>>,
    retention_manifest: Option<RetentionManifest>,
    manifest_file: Option<std::fs::File>,
    #[cfg(feature = "native-test-hooks")]
    teardown_hook: Option<TeardownHook>,
    #[cfg(feature = "native-local-fixture")]
    shell: Option<crate::shell::ShellRoot>,
}

// Handles deny DELETE sharing, preventing directory substitution while path-
// based CFAPI calls run. Check original components before canonicalization.
struct RootGuard {
    root: PathBuf,
    _handles: Vec<Handle>,
}
/// Newly created ordinary directory with its original path chain held against
/// replacement; no registration or existing-data adoption.
pub struct GeneratedDirectory(RootGuard);
impl GeneratedDirectory {
    pub fn create(path: &Path) -> Result<Self> {
        let mut guard = RootGuard::lock_parent(path)?;
        guard.require_unmanaged_parent()?;
        guard.create_leaf()?;
        Ok(Self(guard))
    }
    pub fn path(&self) -> &Path {
        &self.0.root
    }
}
impl RootGuard {
    fn lock_parent(root: &Path) -> Result<Self> {
        let disk = matches!(root.components().next(), Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)));
        if !disk
            || !root.is_absolute()
            || root.components().any(|c| matches!(c, Component::ParentDir))
            || root.file_name().is_none()
        {
            return Err(
                "fixture root requires an absolute local disk path without parent traversal".into(),
            );
        }
        let parent = root.parent().ok_or("missing parent")?;
        let mut handles = Vec::new();
        for ancestor in parent.ancestors().collect::<Vec<_>>().into_iter().rev() {
            let handle = unsafe {
                CreateFileW(
                    wide(ancestor).as_ptr(),
                    FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                    ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(format!("lock original ancestor: {}", unsafe {
                    GetLastError()
                }));
            }
            let handle = Handle(handle, false);
            let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
            if unsafe {
                GetFileInformationByHandleEx(
                    handle.0,
                    FileAttributeTagInfo,
                    (&mut tag as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                    size_of_val(&tag) as u32,
                )
            } == 0
            {
                return Err("cannot inspect original ancestor".into());
            }
            if tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err("fixture ancestors must not be reparse points".into());
            }
            handles.push(handle);
        }
        Ok(Self {
            root: root.to_path_buf(),
            _handles: handles,
        })
    }
    fn require_unmanaged_parent(&self) -> Result<()> {
        let parent = self.root.parent().ok_or("missing parent")?;
        let mut existing = CF_SYNC_ROOT_BASIC_INFO::default();
        let query = unsafe {
            CfGetSyncRootInfoByPath(
                wide(parent).as_ptr(),
                CF_SYNC_ROOT_INFO_BASIC,
                (&mut existing as *mut CF_SYNC_ROOT_BASIC_INFO).cast(),
                size_of_val(&existing) as u32,
                ptr::null_mut(),
            )
        };
        require_unmanaged(query)
    }
    fn create_leaf(&mut self) -> Result<()> {
        let root = &self.root;
        let handles = &mut self._handles;
        // Atomically create a new leaf relative to the locked parent and keep
        // its no-delete-sharing handle. No create/open replacement race.
        let mut name = wide(Path::new(root.file_name().unwrap()));
        let length = ((name.len() - 1) * 2)
            .try_into()
            .map_err(|_| "fixture name too long")?;
        let unicode = UNICODE_STRING {
            Length: length,
            MaximumLength: length,
            Buffer: name.as_mut_ptr(),
        };
        let attributes = OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: handles.last().ok_or("no parent handle")?.0,
            ObjectName: &unicode,
            Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
            ..Default::default()
        };
        let mut io = IO_STATUS_BLOCK::default();
        let mut handle = ptr::null_mut();
        let status = unsafe {
            NtCreateFile(
                &mut handle,
                FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY | SYNCHRONIZE,
                &attributes,
                &mut io,
                ptr::null(),
                FILE_ATTRIBUTE_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                FILE_CREATE,
                FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
                ptr::null(),
                0,
            )
        };
        if status < 0 {
            return Err(format!(
                "create new fixture directory: NTSTATUS 0x{:08X}",
                status as u32
            ));
        }
        handles.push(Handle(handle, false));
        self.root = root.canonicalize().map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn require_unmanaged(result: i32) -> Result<()> {
    // Observed and defined ERROR_CLOUD_FILE_NOT_UNDER_SYNC_ROOT (390).
    if result == (0x80070000u32 | ERROR_CLOUD_FILE_NOT_UNDER_SYNC_ROOT) as i32 {
        return Ok(());
    }
    if result >= 0 {
        return Err("fixture parent belongs to a sync root".into());
    }
    Err(format!(
        "cannot establish unmanaged parent: HRESULT 0x{:08X}",
        result as u32
    ))
}
pub fn verify_unregistered(path: &Path) -> Result<()> {
    let mut info = CF_SYNC_ROOT_BASIC_INFO::default();
    require_unmanaged(unsafe {
        CfGetSyncRootInfoByPath(
            wide(path).as_ptr(),
            CF_SYNC_ROOT_INFO_BASIC,
            (&mut info as *mut CF_SYNC_ROOT_BASIC_INFO).cast(),
            size_of_val(&info) as u32,
            ptr::null_mut(),
        )
    })
}

impl Session {
    /// Only a NEW empty root beneath locked non-reparse ancestors is accepted. Never
    /// updates an existing registration or adopts another client's placeholder.
    pub fn create(root: &Path) -> Result<Self> {
        Self::create_inner(root, None, Dataset::StandardV1)
    }
    /// Same new-root guard as the probe; explicit fixture host only.
    pub fn create_with_source(root: &Path, engine: Arc<dyn HydrationSource>) -> Result<Self> {
        Self::create_inner(root, Some(engine), Dataset::StandardV1)
    }
    pub fn create_with_source_dataset(
        root: &Path,
        engine: Arc<dyn HydrationSource>,
        dataset: Dataset,
    ) -> Result<Self> {
        Self::create_inner(root, Some(engine), dataset)
    }
    fn create_inner(
        root: &Path,
        engine: Option<Arc<dyn HydrationSource>>,
        dataset: Dataset,
    ) -> Result<Self> {
        if !root.is_absolute() || root.file_name().is_none() {
            return Err("fixture root must be a new absolute directory".into());
        }
        let mut guard = RootGuard::lock_parent(root)?;
        guard.require_unmanaged_parent()?;
        guard.create_leaf()?;
        let mut session = Self {
            root: guard.root.clone(),
            registered: false,
            connection: None,
            source: Box::new(Source {
                engine,
                dataset,
                ..Default::default()
            }),
            _guard: guard,
            placeholders: Mutex::new(Vec::new()),
            teardown_started: false,
            dataset,
            directory_guards: Mutex::new(Vec::new()),
            retention_manifest: None,
            manifest_file: None,
            #[cfg(feature = "native-test-hooks")]
            teardown_hook: None,
            #[cfg(feature = "native-local-fixture")]
            shell: None,
        };
        let name = wide(Path::new("TwoDrive isolated CFAPI experiment"));
        let version = wide(Path::new(env!("CARGO_PKG_VERSION")));
        let registration = CF_SYNC_REGISTRATION {
            StructSize: size_of::<CF_SYNC_REGISTRATION>() as u32,
            ProviderName: name.as_ptr(),
            ProviderVersion: version.as_ptr(),
            SyncRootIdentity: dataset.root_identity().as_ptr().cast(),
            SyncRootIdentityLength: dataset.root_identity().len() as u32,
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
                    wide(&session.root).as_ptr(),
                    &registration,
                    &policies,
                    CF_REGISTER_FLAG_NONE,
                )
            },
            "register generated root",
        )?;
        session.registered = true;
        session.connect()?;
        Ok(session)
    }

    pub fn connect(&mut self) -> Result<()> {
        if self.connection.is_some() {
            return Err("already connected".into());
        }
        self.source.stopping.store(false, Ordering::SeqCst);
        let table = [
            CF_CALLBACK_REGISTRATION {
                Type: CF_CALLBACK_TYPE_FETCH_DATA,
                Callback: Some(fetch),
            },
            CF_CALLBACK_REGISTRATION {
                Type: CF_CALLBACK_TYPE_CANCEL_FETCH_DATA,
                Callback: Some(cancel),
            },
            CF_CALLBACK_REGISTRATION {
                Type: CF_CALLBACK_TYPE_NOTIFY_DELETE,
                Callback: Some(deny_delete),
            },
            CF_CALLBACK_REGISTRATION {
                Type: CF_CALLBACK_TYPE_NOTIFY_RENAME,
                Callback: Some(deny_rename),
            },
            CF_CALLBACK_REGISTRATION {
                Type: CF_CALLBACK_TYPE_NOTIFY_DEHYDRATE,
                Callback: Some(deny_dehydrate),
            },
            CF_CALLBACK_REGISTRATION {
                Type: CF_CALLBACK_TYPE_NONE,
                Callback: None,
            },
        ];
        let mut key = 0;
        hr(
            unsafe {
                CfConnectSyncRoot(
                    wide(&self.root).as_ptr(),
                    table.as_ptr(),
                    (&*self.source as *const Source).cast::<c_void>(),
                    if self.dataset == Dataset::VisualV1 {
                        CF_CONNECT_FLAG_REQUIRE_PROCESS_INFO
                    } else {
                        CF_CONNECT_FLAG_NONE
                    },
                    &mut key,
                )
            },
            "connect generated root",
        )?;
        self.connection = Some(key);
        Ok(())
    }
    pub fn disconnect(&mut self) -> Result<()> {
        self.source.stopping.store(true, Ordering::SeqCst);
        if let Ok(active) = self.source.active.lock() {
            for flag in active.values() {
                flag.store(true, Ordering::SeqCst);
            }
        }
        if let Some(key) = self.connection {
            hr(
                unsafe { CfDisconnectSyncRoot(key) },
                "disconnect generated root",
            )?;
            self.connection = None;
        }
        Ok(())
    }
    pub fn finish(&mut self) -> Result<()> {
        // CfUnregisterSyncRoot PERMANENTLY DELETES non-hydrated placeholders.
        // Never use unregistration as cache cleanup. The owner must first retain
        // every generated file, including failures and dirty bytes.
        if self.registered {
            let mut leaves = self
                .placeholders
                .lock()
                .map_err(|_| "placeholder registry poisoned")?;
            let mut protected = Vec::new();
            require_exact_tree(&self.root, &leaves)?;
            let mut order = leaves
                .iter()
                .enumerate()
                .filter(|(_, leaf)| !leaf.reverted)
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            order.sort_by_key(|index| leaves[*index].directory);
            for index in order {
                let leaf = &leaves[index];
                // Attribute-only rejection avoids opening an incomplete file
                // with write access. This is not the authorization: the same
                // evidence is checked again under the protected reference.
                let before = if leaf.directory {
                    inspect_directory(&leaf.path, &leaf.identity)?
                } else {
                    inspect_item(&leaf.path, &leaf.identity, leaf.size)?
                };
                if !before.resident || before.logical_size != leaf.size as u64 {
                    return Err(format!(
                        "refusing unregister before lease: {} resident={} length={}",
                        String::from_utf8_lossy(&leaf.identity),
                        before.resident,
                        before.logical_size
                    ));
                }
                let handle = RetainedPlaceholder::open_node(
                    &leaf.path,
                    &leaf.identity,
                    leaf.size,
                    leaf.directory,
                )?;
                let os = handle.inspect()?;
                if let Some(manifest) = &self.retention_manifest {
                    let (volume, file_id, directory) = file_binding(handle.handle.0)?;
                    if !manifest.authorizes(
                        &relative_name(&self.root, &leaf.path)?,
                        volume,
                        file_id,
                        directory,
                    ) {
                        return Err("retention filesystem identity changed".into());
                    }
                }
                if !os.resident || os.logical_size != leaf.size as u64 {
                    return Err(format!(
                        "refusing unregister under lease: {} resident={} length={}",
                        String::from_utf8_lossy(&leaf.identity),
                        os.resident,
                        os.logical_size
                    ));
                }
                protected.push((index, handle));
            }
            #[cfg(feature = "native-test-hooks")]
            if let Some(hook) = &mut self.teardown_hook {
                hook(TeardownStage::AllProtected)?;
            }
            // All share-none leases remain held through conversion. Reverting a fully
            // retained placeholder makes it an ordinary file before unregister,
            // which cannot delete later writes to that file. No network runs
            // while these three bounded generated-file leases are held.
            self.teardown_started = true;
            for (index, handle) in &protected {
                hr(
                    unsafe {
                        CfRevertPlaceholder(handle.handle.0, CF_REVERT_FLAG_NONE, ptr::null_mut())
                    },
                    "revert retained generated placeholder",
                )?;
                leaves[*index].reverted = true;
            }
            drop(protected);
            require_exact_tree(&self.root, &leaves)?;
            drop(leaves);
            #[cfg(feature = "native-test-hooks")]
            if let Some(hook) = &mut self.teardown_hook {
                hook(TeardownStage::AllReverted)?;
            }
        }
        self.disconnect()?;
        #[cfg(feature = "native-local-fixture")]
        if let Some(shell) = &mut self.shell {
            shell
                .finish()
                .map_err(|e| format!("unregister generated Shell root: {e}"))?;
        }
        if self.registered {
            // Shell unregistration can also remove the Cloud Filter root.
            if verify_unregistered(&self.root).is_err() {
                hr(
                    unsafe { CfUnregisterSyncRoot(wide(&self.root).as_ptr()) },
                    "unregister generated root",
                )?;
            }
            self.registered = false;
        }
        Ok(())
    }
    pub fn serving(&self) -> bool {
        self.connection.is_some() && !self.teardown_started
    }
    pub fn teardown_started(&self) -> bool {
        self.teardown_started
    }
    #[cfg(feature = "native-test-hooks")]
    pub fn set_teardown_hook(
        &mut self,
        hook: impl FnMut(TeardownStage) -> Result<()> + Send + 'static,
    ) {
        self.teardown_hook = Some(Box::new(hook));
    }
    #[cfg(feature = "native-local-fixture")]
    pub fn attach_shell(&mut self) -> Result<()> {
        if self.shell.is_some() {
            return Err("Shell already attached".into());
        }
        self.shell = Some(
            crate::shell::ShellRoot::register_generated(&self.root, self.dataset.root_identity())
                .map_err(|e| format!("register generated Shell root: {e}"))?,
        );
        Ok(())
    }
    #[cfg(feature = "native-local-fixture")]
    pub fn publish_shell(&mut self, path: &Path, state: &str) -> Result<()> {
        self.shell
            .as_mut()
            .ok_or("Shell unavailable")?
            .publish(
                path,
                state,
                path == self.root
                    || self
                        .placeholders
                        .lock()
                        .map_err(|_| "placeholder registry poisoned")?
                        .iter()
                        .any(|p| p.path == path && p.directory),
            )
            .map_err(|e| format!("publish generated Shell state: {e}"))
    }
    #[cfg(feature = "native-local-fixture")]
    pub fn verify_shell(&self) -> Result<()> {
        self.shell
            .as_ref()
            .ok_or("Shell unavailable")?
            .verify()
            .map_err(|e| format!("verify generated Shell root: {e}"))
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn calls(&self) -> usize {
        self.source.calls.load(Ordering::SeqCst)
    }
    pub fn failures(&self) -> usize {
        self.source.failures.load(Ordering::SeqCst)
    }
    pub fn transfer_counters(&self) -> serde_json::Value {
        let recent = self
            .source
            .recent_requests
            .lock()
            .ok()
            .map(|r| r.iter().cloned().collect::<Vec<_>>());
        let inflight = self.source.active.lock().ok().map(|r| r.len());
        serde_json::json!({"native_requested_bytes":self.source.requested_bytes.load(Ordering::SeqCst),"native_transferred_bytes":self.source.transferred_bytes.load(Ordering::SeqCst),"native_in_flight":inflight,"recent_requests":recent,"fetch_callbacks":self.calls(),"fetch_failures":self.failures()})
    }
    pub fn offline(&self, value: bool) {
        self.source.offline.store(value, Ordering::SeqCst);
    }

    pub fn placeholder(&self, relative: &Path) -> Result<PathBuf> {
        self.placeholder_for(relative, ID, SIZE)
    }
    pub fn placeholder_for(
        &self,
        relative: &Path,
        identity: &[u8],
        size: usize,
    ) -> Result<PathBuf> {
        self.create_entry(relative, identity, size, false)
    }
    pub fn directory_for(&self, relative: &Path, identity: &[u8]) -> Result<PathBuf> {
        self.create_entry(relative, identity, 0, true)
    }
    fn create_entry(
        &self,
        relative: &Path,
        identity: &[u8],
        size: usize,
        directory: bool,
    ) -> Result<PathBuf> {
        if identity.is_empty() || identity.len() > 4096 || size > i64::MAX as usize {
            return Err("invalid placeholder identity/size".into());
        }
        let components = relative.components().collect::<Vec<_>>();
        if components.is_empty()
            || components.len() > 2
            || components
                .iter()
                .any(|c| !matches!(c, Component::Normal(_)))
            || (directory && components.len() != 1)
            || relative.file_name().is_none()
            || relative.is_absolute()
        {
            return Err("only bounded generated relative names are accepted".into());
        }
        let parent = relative.parent().filter(|p| !p.as_os_str().is_empty());
        let base = if let Some(parent) = parent {
            let path = self.root.join(parent);
            let nodes = self
                .placeholders
                .lock()
                .map_err(|_| "placeholder registry poisoned")?;
            let node = nodes
                .iter()
                .find(|p| p.path == path && p.directory)
                .ok_or("unowned generated parent")?;
            directory_observation(metadata_handle(&path)?.0, &node.identity)?;
            path
        } else {
            self.root.clone()
        };
        let name = wide(Path::new(
            relative.file_name().ok_or("missing generated leaf")?,
        ));
        let mut entry = CF_PLACEHOLDER_CREATE_INFO {
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
        entry.FsMetadata.FileSize = size as i64;
        entry.FsMetadata.BasicInfo.FileAttributes = if directory {
            FILE_ATTRIBUTE_DIRECTORY
        } else {
            FILE_ATTRIBUTE_NORMAL
        };
        let mut done = 0;
        hr(
            unsafe {
                CfCreatePlaceholders(
                    wide(&base).as_ptr(),
                    &mut entry,
                    1,
                    CF_CREATE_FLAG_STOP_ON_ERROR,
                    &mut done,
                )
            },
            "create generated placeholder",
        )?;
        hr(entry.Result, "placeholder entry")?;
        if done != 1 {
            return Err("placeholder was not created".into());
        }
        let path = self.root.join(relative);
        self.placeholders
            .lock()
            .map_err(|_| "placeholder registry poisoned")?
            .push(RegisteredPlaceholder {
                path: path.clone(),
                identity: identity.to_vec(),
                size,
                reverted: false,
                directory,
            });
        if directory {
            let guard = directory_handle(&path)?;
            directory_observation(guard.0, identity)?;
            self.directory_guards
                .lock()
                .map_err(|_| "directory guard poisoned")?
                .push(guard);
        }
        Ok(path)
    }
    pub fn seal_manifest(&mut self, path: &Path) -> Result<()> {
        use std::{io::Write, os::windows::fs::OpenOptionsExt};
        if self.retention_manifest.is_some() {
            return Err("retention manifest already sealed".into());
        }
        let (volume, root_id, directory) = file_binding(metadata_handle(&self.root)?.0)?;
        if !directory {
            return Err("root is not a directory".into());
        }
        let nodes = self
            .placeholders
            .lock()
            .map_err(|_| "placeholder registry poisoned")?;
        require_exact_tree(&self.root, &nodes)?;
        let mut entries = Vec::new();
        for node in nodes.iter() {
            let (node_volume, file_id, node_directory) =
                file_binding(metadata_handle(&node.path)?.0)?;
            if node_volume != volume || node_directory != node.directory {
                return Err("retention filesystem identity differs".into());
            }
            entries.push(BoundEntry {
                relative_path: relative_name(&self.root, &node.path)?,
                file_id,
                directory: node.directory,
            });
        }
        let manifest = RetentionManifest {
            schema_version: 1,
            dataset: self.dataset,
            volume_serial: volume,
            root_file_id: root_id,
            entries,
        };
        manifest.validate()?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)
            .map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec(&manifest).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        self.retention_manifest = Some(manifest);
        self.manifest_file = Some(file);
        Ok(())
    }
    pub fn binding_summary(&self) -> serde_json::Value {
        self.retention_manifest.as_ref().map_or(serde_json::Value::Null,|m|serde_json::json!({"dataset":m.dataset,"volume_serial":m.volume_serial,"root_file_id":m.root_file_id}))
    }
    pub fn mark_directories_initially_in_sync(&self) -> Result<()> {
        let nodes = self
            .placeholders
            .lock()
            .map_err(|_| "placeholder registry poisoned")?;
        require_exact_tree(&self.root, &nodes)?;
        for node in nodes.iter().filter(|n| n.directory) {
            let raw = unsafe {
                CreateFileW(
                    wide(&node.path).as_ptr(),
                    FILE_READ_ATTRIBUTES | WRITE_DAC,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OPEN_REPARSE_POINT
                        | FILE_FLAG_BACKUP_SEMANTICS
                        | FILE_FLAG_OPEN_NO_RECALL,
                    ptr::null_mut(),
                )
            };
            if raw == INVALID_HANDLE_VALUE {
                return Err("cannot open initial directory metadata".into());
            }
            let handle = Handle(raw, false);
            if !directory_observation(handle.0, &node.identity)?.resident {
                return Err("generated directory population is incomplete".into());
            }
            hr(
                unsafe {
                    CfSetInSyncState(
                        handle.0,
                        CF_IN_SYNC_STATE_IN_SYNC,
                        CF_SET_IN_SYNC_FLAG_NONE,
                        ptr::null_mut(),
                    )
                },
                "acknowledge initial generated directory listing",
            )?;
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            // Do not claim cleanup succeeded. Leaking the context is safer than
            // dangling callback memory if the OS cannot disconnect.
            eprintln!("cleanup incomplete: {error}");
            if self.connection.is_some() {
                let _ = Box::leak(std::mem::take(&mut self.source));
            }
        }
    }
}

struct Handle(HANDLE, bool);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            if self.1 {
                CfCloseHandle(self.0);
            } else {
                CloseHandle(self.0);
            }
        }
    }
}

struct RetainedPlaceholder {
    handle: Handle,
    identity: Vec<u8>,
    size: usize,
    directory: bool,
}
impl RetainedPlaceholder {
    #[cfg(test)]
    fn open(path: &Path, identity: &[u8], size: usize) -> Result<Self> {
        Self::open_node(path, identity, size, false)
    }
    fn open_node(path: &Path, identity: &[u8], size: usize, directory: bool) -> Result<Self> {
        // WRITE_DAC authorizes CfRevertPlaceholder without requesting data
        // writes. A normal share-none lease also gives deterministic refusal to
        // concurrent writers; no oplock can be acknowledged behind this owner.
        let raw = unsafe {
            CreateFileW(
                wide(path).as_ptr(),
                FILE_READ_DATA | FILE_READ_ATTRIBUTES | WRITE_DAC,
                0,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT
                    | FILE_FLAG_OPEN_NO_RECALL
                    | FILE_FLAG_BACKUP_SEMANTICS,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(format!("exclusive retained-file lease: {}", unsafe {
                GetLastError()
            }));
        }
        let handle = Handle(raw, false);
        Ok(Self {
            handle,
            identity: identity.to_vec(),
            size,
            directory,
        })
    }
    fn inspect(&self) -> Result<NativeObservation> {
        if self.directory {
            directory_observation(self.handle.0, &self.identity)
        } else {
            observation(self.handle.0, &self.identity, self.size)
        }
    }
}

/// Recovery of THIS generated fixture only, without registration or connection.
/// All bytes are retained under leases before any conversion or unregister.
#[cfg(feature = "native-test-hooks")]
pub fn recover_retained_generated(
    root: &Path,
    backup: &Path,
    mut record_dirty: impl FnMut(Dataset, &str) -> Result<()>,
) -> Result<()> {
    use std::{
        fs,
        io::{Read, Write},
        os::windows::{fs::OpenOptionsExt, io::FromRawHandle},
    };
    let mut guard = RootGuard::lock_parent(root)?;
    guard.require_unmanaged_parent()?;
    let root_handle = directory_handle(root)?;
    let (volume, root_file_id, is_directory) = file_binding(root_handle.0)?;
    if !is_directory {
        return Err("retained root is not a directory".into());
    }
    guard._handles.push(root_handle);
    guard.root = root.canonicalize().map_err(|e| e.to_string())?;
    let mut buffer = [0u64; 1024];
    let mut length = 0;
    hr(
        unsafe {
            CfGetSyncRootInfoByPath(
                wide(&guard.root).as_ptr(),
                CF_SYNC_ROOT_INFO_STANDARD,
                buffer.as_mut_ptr().cast(),
                size_of_val(&buffer) as u32,
                &mut length,
            )
        },
        "inspect retained generated root",
    )?;
    let info = unsafe { &*buffer.as_ptr().cast::<CF_SYNC_ROOT_STANDARD_INFO>() };
    let start = offset_of!(CF_SYNC_ROOT_STANDARD_INFO, SyncRootIdentity);
    let end = start
        .checked_add(info.SyncRootIdentityLength as usize)
        .ok_or("invalid root identity length")?;
    if length < size_of::<CF_SYNC_ROOT_STANDARD_INFO>() as u32
        || end > length as usize
        || end > size_of_val(&buffer)
        || info.SyncRootFileId as u64 != root_file_id
    {
        return Err("retained root identity/binding differs".into());
    }
    let identity = unsafe {
        std::slice::from_raw_parts(
            info.SyncRootIdentity.as_ptr(),
            info.SyncRootIdentityLength as usize,
        )
    };
    let dataset = Dataset::from_identity(identity).ok_or("not a known generated dataset")?;
    let mut shell = crate::shell::ShellRoot::retained_generated(&guard.root, identity)
        .map_err(|e| format!("verify retained Shell: {e}"))?;
    let manifest_path = root
        .with_extension("engine-state")
        .join("native-retention.json");
    let manifest = if manifest_path.exists() {
        let _manifest_guard = RootGuard::lock_parent(&manifest_path)?;
        let mut file = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL)
            .open(&manifest_path)
            .map_err(|e| e.to_string())?;
        let mut attr = BY_HANDLE_FILE_INFORMATION::default();
        use std::os::windows::io::AsRawHandle;
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut attr) } == 0
            || attr.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || file.metadata().map_err(|e| e.to_string())?.len() > 16384
        {
            return Err("unsafe retention manifest".into());
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        let manifest: RetentionManifest =
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        manifest.validate()?;
        if manifest.dataset != dataset
            || manifest.root_file_id != root_file_id
            || manifest.volume_serial != volume
        {
            return Err("retention root manifest differs".into());
        }
        Some(manifest)
    } else {
        if dataset == Dataset::VisualV1 {
            return Err("visual retention manifest required".into());
        }
        None
    };
    let nodes = dataset
        .entries()
        .into_iter()
        .map(|entry| RegisteredPlaceholder {
            path: guard.root.join(&entry.relative_path),
            identity: entry.id.into_bytes(),
            size: entry.size,
            reverted: false,
            directory: entry.directory,
        })
        .collect::<Vec<_>>();
    for node in nodes.iter().filter(|n| n.directory) {
        let handle = directory_handle(&node.path)?;
        authorize_retained_node(handle.0, node, &guard.root, manifest.as_ref())?;
        guard._handles.push(handle);
    }
    require_exact_tree(&guard.root, &nodes)?;
    let mut retained = Vec::new();
    let mut order = (0..nodes.len()).collect::<Vec<_>>();
    order.sort_by_key(|&i| nodes[i].directory);
    for index in order {
        let node = &nodes[index];
        let handle =
            RetainedPlaceholder::open_node(&node.path, &node.identity, node.size, node.directory)?;
        let (os, placeholder) =
            authorize_retained_node(handle.handle.0, node, &guard.root, manifest.as_ref())?;
        if !os.resident || os.logical_size != node.size as u64 {
            return Err("retained node incomplete; no conversion performed".into());
        }
        retained.push((index, handle, os, placeholder));
    }
    let backup_guard = GeneratedDirectory::create(backup)?;
    let backup = backup_guard.path();
    let mut backup_guards = Vec::new();
    for entry in dataset.directories() {
        backup_guards.push(GeneratedDirectory::create(
            &backup.join(entry.relative_path),
        )?);
    }
    for (index, handle, os, placeholder) in &retained {
        let node = &nodes[*index];
        if node.directory {
            continue;
        }
        let id = std::str::from_utf8(&node.identity).map_err(|_| "invalid generated identity")?;
        let file_index = dataset
            .files()
            .iter()
            .position(|e| e.id == id)
            .ok_or("unknown generated file")?;
        let mut bytes = vec![0; node.size];
        let mut file =
            std::mem::ManuallyDrop::new(unsafe { fs::File::from_raw_handle(handle.handle.0) });
        file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        let output_path = backup.join(relative_name(&guard.root, &node.path)?);
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output_path)
            .map_err(|e| e.to_string())?;
        output.write_all(&bytes).map_err(|e| e.to_string())?;
        output.sync_all().map_err(|e| e.to_string())?;
        if fs::read(&output_path).map_err(|e| e.to_string())? != bytes {
            return Err("backup bytes differ; no conversion performed".into());
        }
        if (*placeholder && (os.modified_bytes != 0 || !os.os_in_sync))
            || bytes != dataset.bytes(file_index)
        {
            record_dirty(dataset, id)?;
        }
    }
    for (_, handle, _, placeholder) in &retained {
        if *placeholder {
            hr(
                unsafe {
                    CfRevertPlaceholder(handle.handle.0, CF_REVERT_FLAG_NONE, ptr::null_mut())
                },
                "revert retained generated node",
            )?;
        }
    }
    drop(retained);
    require_exact_tree(&guard.root, &nodes)?;
    shell
        .finish()
        .map_err(|e| format!("unregister retained Shell: {e}"))?;
    if verify_unregistered(&guard.root).is_err() {
        hr(
            unsafe { CfUnregisterSyncRoot(wide(&guard.root).as_ptr()) },
            "unregister retained generated root",
        )?;
    }
    verify_unregistered(&guard.root)
}

#[cfg(any(test, feature = "native-test-hooks"))]
fn authorize_retained_node(
    handle: HANDLE,
    node: &RegisteredPlaceholder,
    root: &Path,
    manifest: Option<&RetentionManifest>,
) -> Result<(NativeObservation, bool)> {
    let (volume, file_id, directory) = file_binding(handle)?;
    if directory != node.directory {
        return Err("retained node kind differs".into());
    }
    if let Some(manifest) = manifest
        && !manifest.authorizes(
            &relative_name(root, &node.path)?,
            volume,
            file_id,
            directory,
        )
    {
        return Err("retained node filesystem identity changed".into());
    }
    let observed = if directory {
        directory_observation(handle, &node.identity)
    } else {
        observation(handle, &node.identity, node.size)
    };
    match observed {
        Ok(os) => Ok((os, true)),
        Err(error) => {
            // Ordinary-file CfGetPlaceholderInfo errors differ across builds
            // (NOT_A_CLOUD_FILE locally, INVALID_FUNCTION on Server 26100).
            // An error code is not positive proof of conversion. Require the
            // bound filesystem ID above plus independent, ordinary tag state.
            if manifest.is_none() {
                return Err(error);
            }
            let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
            if unsafe {
                GetFileInformationByHandleEx(
                    handle,
                    FileAttributeTagInfo,
                    (&mut tag as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                    size_of_val(&tag) as u32,
                )
            } == 0
                || tag.FileAttributes
                    & (FILE_ATTRIBUTE_REPARSE_POINT
                        | FILE_ATTRIBUTE_OFFLINE
                        | FILE_ATTRIBUTE_RECALL_ON_OPEN
                        | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
                    != 0
                || tag.ReparseTag != 0
                || unsafe {
                    CfGetPlaceholderStateFromAttributeTag(tag.FileAttributes, tag.ReparseTag)
                } != CF_PLACEHOLDER_STATE_NO_STATES
            {
                return Err(error);
            }
            let mut length = 0;
            if !directory && unsafe { GetFileSizeEx(handle, &mut length) } == 0 {
                return Err("ordinary retained size unknown".into());
            }
            Ok((
                NativeObservation {
                    logical_size: length as u64,
                    resident: directory || length == node.size as i64,
                    partial: false,
                    pinned: false,
                    pin_known_unpinned: true,
                    release_requested: false,
                    os_in_sync: false,
                    modified_bytes: 0,
                },
                false,
            ))
        }
    }
}

fn metadata_handle(path: &Path) -> Result<Handle> {
    // OPEN_REPARSE_POINT + attributes-only does not request file content.
    let handle = unsafe {
        CreateFileW(
            wide(path).as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(format!("open attributes: {}", unsafe { GetLastError() }));
    }
    Ok(Handle(handle, false))
}

#[cfg(test)]
mod retained_lease_tests {
    use super::*;
    #[test]
    fn ordinary_converted_directory_and_files_require_manifest_ids_and_retain_bytes() {
        use std::fs;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let dataset = Dataset::VisualV1;
        for entry in dataset.directories() {
            fs::create_dir(root.join(entry.relative_path)).unwrap();
        }
        for (index, entry) in dataset.files().into_iter().enumerate() {
            fs::write(root.join(entry.relative_path), dataset.bytes(index)).unwrap();
        }
        let nodes = dataset
            .entries()
            .into_iter()
            .map(|entry| RegisteredPlaceholder {
                path: root.join(entry.relative_path),
                identity: entry.id.into_bytes(),
                size: entry.size,
                directory: entry.directory,
                reverted: true,
            })
            .collect::<Vec<_>>();
        require_exact_tree(&root, &nodes).unwrap();
        let (volume, root_file_id, _) = file_binding(metadata_handle(&root).unwrap().0).unwrap();
        let manifest = RetentionManifest {
            schema_version: 1,
            dataset,
            volume_serial: volume,
            root_file_id,
            entries: nodes
                .iter()
                .map(|node| {
                    let (_, file_id, directory) =
                        file_binding(metadata_handle(&node.path).unwrap().0).unwrap();
                    BoundEntry {
                        relative_path: relative_name(&root, &node.path).unwrap(),
                        file_id,
                        directory,
                    }
                })
                .collect(),
        };
        manifest.validate().unwrap();
        let directory_guard = directory_handle(&nodes[0].path).unwrap();
        let mut leases = Vec::new();
        for node in nodes
            .iter()
            .filter(|n| !n.directory)
            .chain(nodes.iter().filter(|n| n.directory))
        {
            let lease = RetainedPlaceholder::open_node(
                &node.path,
                &node.identity,
                node.size,
                node.directory,
            )
            .unwrap();
            let (observed, placeholder) =
                authorize_retained_node(lease.handle.0, node, &root, Some(&manifest)).unwrap();
            assert!(authorize_retained_node(lease.handle.0, node, &root, None).is_err());
            assert!(observed.resident);
            assert!(!placeholder);
            leases.push(lease);
        }
        assert_eq!(
            fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&nodes[1].path)
                .unwrap_err()
                .raw_os_error(),
            Some(ERROR_SHARING_VIOLATION as i32)
        );
        drop(leases);
        drop(directory_guard);
        assert_eq!(fs::read(&nodes[2].path).unwrap(), dataset.bytes(1));
        let replacement = root.join("replacement.tmp");
        fs::write(&replacement, dataset.bytes(0)).unwrap();
        fs::rename(&replacement, &nodes[1].path).unwrap();
        let lease = RetainedPlaceholder::open_node(
            &nodes[1].path,
            &nodes[1].identity,
            nodes[1].size,
            false,
        )
        .unwrap();
        assert!(
            authorize_retained_node(lease.handle.0, &nodes[1], &root, Some(&manifest)).is_err()
        );
        drop(lease);
        fs::write(root.join("unexpected.bin"), b"preserve").unwrap();
        assert!(require_exact_tree(&root, &nodes).is_err());
    }
    #[test]
    fn retained_lease_refuses_truncation_until_conversion_owner_releases_it() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ordinary-generated.bin");
        std::fs::write(&path, b"complete retained data").unwrap();
        let lease = RetainedPlaceholder::open(&path, b"unused-ordinary-identity", 22).unwrap();
        let denied = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap_err();
        assert_eq!(denied.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32));
        drop(lease);
        assert_eq!(std::fs::read(&path).unwrap(), b"complete retained data");
        std::fs::write(&path, b"ordinary write after conversion").unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"ordinary write after conversion"
        );
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct NativeObservation {
    pub logical_size: u64,
    pub resident: bool,
    pub partial: bool,
    pub pinned: bool,
    pub pin_known_unpinned: bool,
    pub release_requested: bool,
    pub os_in_sync: bool,
    pub modified_bytes: i64,
}
impl NativeObservation {
    pub fn clean(&self) -> bool {
        self.resident && self.pin_known_unpinned && self.os_in_sync && self.modified_bytes == 0
    }
}
fn observation(handle: HANDLE, expected_identity: &[u8], size: usize) -> Result<NativeObservation> {
    node_observation(handle, expected_identity, size, false)
}
fn directory_observation(handle: HANDLE, expected_identity: &[u8]) -> Result<NativeObservation> {
    node_observation(handle, expected_identity, 0, true)
}
fn node_observation(
    handle: HANDLE,
    expected_identity: &[u8],
    size: usize,
    directory: bool,
) -> Result<NativeObservation> {
    // u64 buffer provides alignment for the variable-length standard-info blob.
    let mut buffer = [0u64; 1024];
    let mut returned = 0;
    hr(
        unsafe {
            CfGetPlaceholderInfo(
                handle,
                CF_PLACEHOLDER_INFO_STANDARD,
                buffer.as_mut_ptr().cast(),
                size_of_val(&buffer) as u32,
                &mut returned,
            )
        },
        "inspect placeholder",
    )?;
    if returned < size_of::<CF_PLACEHOLDER_STANDARD_INFO>() as u32 {
        return Err("short placeholder info".into());
    }
    let info = unsafe { &*buffer.as_ptr().cast::<CF_PLACEHOLDER_STANDARD_INFO>() };
    let identity_end =
        offset_of!(CF_PLACEHOLDER_STANDARD_INFO, FileIdentity) + expected_identity.len();
    if info.FileIdentityLength as usize != expected_identity.len()
        || identity_end > returned as usize
    {
        return Err("foreign placeholder identity".into());
    }
    let identity =
        unsafe { std::slice::from_raw_parts(info.FileIdentity.as_ptr(), expected_identity.len()) };
    if identity != expected_identity {
        return Err("foreign placeholder identity".into());
    }
    let mut logical_size = 0i64;
    if !directory && (unsafe { GetFileSizeEx(handle, &mut logical_size) } == 0 || logical_size < 0)
    {
        return Err("cannot inspect logical size".into());
    }
    let (resident, partial) = if directory {
        let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
        if unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileAttributeTagInfo,
                (&mut tag as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                size_of_val(&tag) as u32,
            )
        } == 0
            || tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        {
            return Err("not a generated directory".into());
        }
        let flags =
            unsafe { CfGetPlaceholderStateFromAttributeTag(tag.FileAttributes, tag.ReparseTag) };
        let partial = flags & CF_PLACEHOLDER_STATE_PARTIAL != 0;
        (!partial, partial)
    } else {
        (
            info.OnDiskDataSize >= size as i64,
            info.OnDiskDataSize > 0 && info.OnDiskDataSize < size as i64,
        )
    };
    Ok(NativeObservation {
        logical_size: logical_size as u64,
        resident,
        partial,
        pinned: info.PinState == CF_PIN_STATE_PINNED,
        pin_known_unpinned: info.PinState == CF_PIN_STATE_UNPINNED
            || info.PinState == CF_PIN_STATE_UNSPECIFIED,
        release_requested: info.PinState == CF_PIN_STATE_UNPINNED,
        os_in_sync: info.InSyncState == CF_IN_SYNC_STATE_IN_SYNC,
        modified_bytes: info.ModifiedDataSize,
    })
}
fn status(handle: HANDLE, engine_state: FileState) -> Result<LocalStatus> {
    let info = observation(handle, ID, SIZE)?;
    Ok(LocalStatus {
        resident: info.resident,
        partial: info.partial,
        pinned: info.pinned,
        os_in_sync: info.os_in_sync,
        modified_bytes: info.modified_bytes,
        engine_state,
        pending_operations: false,
        open_handles: false,
    })
}

pub fn inspect_item(path: &Path, identity: &[u8], size: usize) -> Result<NativeObservation> {
    observation(metadata_handle(path)?.0, identity, size)
}
pub fn inspect_directory(path: &Path, identity: &[u8]) -> Result<NativeObservation> {
    directory_observation(metadata_handle(path)?.0, identity)
}

fn directory_handle(path: &Path) -> Result<Handle> {
    let handle = unsafe {
        CreateFileW(
            wide(path).as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(format!("lock generated directory: {}", unsafe {
            GetLastError()
        }));
    }
    Ok(Handle(handle, false))
}
/// Effective directory traversal lease. Attribute-only handles do not prevent
/// rename on Windows. A directory-only relative NT open prevents a substituted
/// file from being opened for data/implicit hydration. Keep this distinct from
/// metadata observations and from
/// the fixture teardown's independent exclusive directory handles.
#[cfg(feature = "native-sync")]
fn directory_lease(parent: HANDLE, component: &std::ffi::OsStr) -> Result<Handle> {
    use windows_sys::Wdk::Storage::FileSystem::FILE_OPEN;
    let mut name = wide(Path::new(component));
    let length = ((name.len() - 1) * 2)
        .try_into()
        .map_err(|_| "directory component too long")?;
    let unicode = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: name.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent,
        ObjectName: &unicode,
        Attributes: OBJ_CASE_INSENSITIVE,
        ..Default::default()
    };
    let mut io = IO_STATUS_BLOCK::default();
    let mut raw = ptr::null_mut();
    let status = unsafe {
        NtCreateFile(
            &mut raw,
            FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY | SYNCHRONIZE,
            &attributes,
            &mut io,
            ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            FILE_OPEN,
            FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    };
    if status < 0 {
        return Err(format!(
            "retain traversal directory: NTSTATUS 0x{:08X}",
            status as u32
        ));
    }
    let handle = Handle(raw, false);
    if !file_binding(handle.0)?.2 {
        return Err("traversal target is not a directory".into());
    }
    Ok(handle)
}

fn relative_name(root: &Path, path: &Path) -> Result<String> {
    path.strip_prefix(root)
        .map_err(|_| "path outside fixture")?
        .components()
        .map(|c| match c {
            Component::Normal(name) => name
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| "invalid generated name".into()),
            _ => Err("invalid relative component".into()),
        })
        .collect::<Result<Vec<String>>>()
        .map(|c| c.join("/"))
}
fn file_binding(handle: HANDLE) -> Result<(u32, u64, bool)> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err("cannot inspect filesystem identity".into());
    }
    Ok((
        info.dwVolumeSerialNumber,
        ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0,
    ))
}
fn require_exact_tree(root: &Path, nodes: &[RegisteredPlaceholder]) -> Result<()> {
    for parent in std::iter::once(root.to_path_buf())
        .chain(nodes.iter().filter(|n| n.directory).map(|n| n.path.clone()))
    {
        let expected = nodes
            .iter()
            .filter(|n| n.path.parent() == Some(parent.as_path()))
            .map(|n| n.path.file_name().unwrap().to_owned())
            .collect::<Vec<_>>();
        let actual = std::fs::read_dir(&parent)
            .map_err(|e| e.to_string())?
            .map(|e| e.map(|d| d.file_name()))
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        if expected.len() != actual.len() || expected.iter().any(|n| !actual.contains(n)) {
            return Err("unexpected or missing fixture tree entry; root retained".into());
        }
    }
    Ok(())
}

/// Scope-bound OS guard. It cannot be detached into a release-ready boolean.
pub struct LockedPlaceholder<'a> {
    handle: HANDLE,
    identity: &'a [u8],
    size: usize,
}
impl LockedPlaceholder<'_> {
    pub fn inspect(&self) -> Result<NativeObservation> {
        observation(
            unsafe { CfGetWin32HandleFromProtectedHandle(self.handle) },
            self.identity,
            self.size,
        )
    }
    pub fn dehydrate(&self) -> Result<()> {
        let state = self.inspect()?;
        if !state.clean() || state.logical_size != self.size as u64 {
            return Err("native state vetoes dehydration".into());
        }
        hr(
            unsafe {
                CfDehydratePlaceholder(self.handle, 0, -1, CF_DEHYDRATE_FLAG_NONE, ptr::null_mut())
            },
            "dehydrate journal-verified placeholder",
        )
    }
}
pub fn with_exclusive_placeholder<T>(
    path: &Path,
    identity: &[u8],
    size: usize,
    action: impl FnOnce(&LockedPlaceholder<'_>) -> Result<T>,
) -> Result<T> {
    let mut raw = ptr::null_mut();
    hr(
        unsafe {
            CfOpenFileWithOplock(
                wide(path).as_ptr(),
                CF_OPEN_FILE_FLAG_EXCLUSIVE | CF_OPEN_FILE_FLAG_WRITE_ACCESS,
                &mut raw,
            )
        },
        "exclusive engine oplock",
    )?;
    let handle = Handle(raw, true);
    if !unsafe { CfReferenceProtectedHandle(handle.0) } {
        return Err("oplock already broken".into());
    }
    struct Reference(HANDLE);
    impl Drop for Reference {
        fn drop(&mut self) {
            unsafe { CfReleaseProtectedHandle(self.0) };
        }
    }
    let _reference = Reference(handle.0);
    let locked = LockedPlaceholder {
        handle: handle.0,
        identity,
        size,
    };
    locked.inspect()?;
    action(&locked)
}

/// A synchronous no-recall, no-follow, share-none lease for copying native
/// edits. Busy files are deferred instead of reading a writer's partial save.
pub struct NativeCapture<'a>(&'a RetainedPlaceholder);
impl NativeCapture<'_> {
    /// Call only while the engine's acknowledged generation is held and its
    /// full digest matches this retained native file. The share-none lease
    /// prevents a write/rename between verification and CfSetInSyncState.
    pub fn set_in_sync_after_verification(&self) -> Result<()> {
        hr(
            unsafe {
                CfSetInSyncState(
                    self.0.handle.0,
                    CF_IN_SYNC_STATE_IN_SYNC,
                    CF_SET_IN_SYNC_FLAG_NONE,
                    ptr::null_mut(),
                )
            },
            "acknowledge verified native bytes",
        )?;
        if !self.0.inspect()?.os_in_sync {
            return Err("native sync acknowledgement not observed".into());
        }
        Ok(())
    }

    pub fn modified_time(&self) -> Result<std::time::SystemTime> {
        use std::os::windows::io::BorrowedHandle;
        let borrowed = unsafe { BorrowedHandle::borrow_raw(self.0.handle.0) };
        let file = std::fs::File::from(borrowed.try_clone_to_owned().map_err(|e| e.to_string())?);
        file.metadata()
            .and_then(|metadata| metadata.modified())
            .map_err(|e| e.to_string())
    }
    pub fn copy_resident_to(&self, output: &mut impl std::io::Write) -> Result<u64> {
        use std::io::{Read, Seek, SeekFrom};
        use std::os::windows::io::BorrowedHandle;
        let state = self.0.inspect()?;
        if !state.resident || state.partial {
            return Err("native capture requires resident bytes".into());
        }
        let borrowed = unsafe { BorrowedHandle::borrow_raw(self.0.handle.0) };
        let mut file =
            std::fs::File::from(borrowed.try_clone_to_owned().map_err(|e| e.to_string())?);
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut buffer = [0u8; 65536];
        let mut total = 0u64;
        loop {
            let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            output
                .write_all(&buffer[..count])
                .map_err(|e| e.to_string())?;
            total = total
                .checked_add(count as u64)
                .ok_or("capture size overflow")?;
        }
        if total != state.logical_size || self.0.inspect()?.logical_size != state.logical_size {
            return Err("native capture size changed".into());
        }
        Ok(total)
    }
}
pub fn with_native_capture<T>(
    path: &Path,
    identity: &[u8],
    action: impl FnOnce(&NativeCapture<'_>) -> Result<T>,
) -> Result<T> {
    let mut lease = RetainedPlaceholder::open_node(path, identity, 0, false)?;
    // The committed cloud version can have a different size. Residency must
    // describe the current local generation, under the SAME exclusive lease.
    let mut current_size = 0i64;
    if unsafe { GetFileSizeEx(lease.handle.0, &mut current_size) } == 0 || current_size < 0 {
        return Err("cannot inspect native capture length".into());
    }
    lease.size = usize::try_from(current_size).map_err(|_| "native capture length overflow")?;
    lease.inspect()?;
    action(&NativeCapture(&lease))
}

pub fn inspect(path: &Path, engine_state: FileState) -> Result<LocalStatus> {
    status(metadata_handle(path)?.0, engine_state)
}

pub fn pin(path: &Path, pinned: bool) -> Result<()> {
    set_pin_state(
        path,
        if pinned {
            CF_PIN_STATE_PINNED
        } else {
            CF_PIN_STATE_UNSPECIFIED
        },
    )
}
#[cfg(feature = "native-test-hooks")]
pub fn test_release_intent(path: &Path) -> Result<()> {
    set_pin_state(path, CF_PIN_STATE_UNPINNED)
}
fn set_pin_state(path: &Path, state: CF_PIN_STATE) -> Result<()> {
    let mut handle = ptr::null_mut();
    hr(
        unsafe {
            CfOpenFileWithOplock(
                wide(path).as_ptr(),
                CF_OPEN_FILE_FLAG_WRITE_ACCESS,
                &mut handle,
            )
        },
        "open pin oplock",
    )?;
    let handle = Handle(handle, true);
    hr(
        unsafe { CfSetPinState(handle.0, state, CF_SET_PIN_FLAG_NONE, ptr::null_mut()) },
        "set pin intent",
    )
}

/// Prototype only: engine_state is the harness's immutable fixture state. A
/// production caller needs a durable generation/journal and item-lock adapter.
pub fn release(path: &Path, engine_state: FileState) -> Result<()> {
    with_exclusive_placeholder(path, ID, SIZE, |locked| {
        if engine_state != FileState::Cached {
            return Err("fixture engine state veto".into());
        }
        locked.dehydrate()
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn only_known_unmanaged_hresult_allows_creation() {
        assert!(require_unmanaged(0x80070186u32 as i32).is_ok());
        for code in [
            0,
            1,
            0x80070005u32 as i32,
            0x80070057u32 as i32,
            0x80070001u32 as i32,
            -1,
        ] {
            assert!(require_unmanaged(code).is_err());
        }
    }
    #[test]
    fn new_leaf_and_ancestors_stay_locked_against_substitution() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("new-fixture");
        // File-lock behavior is independent of Cloud Filter support on a CI volume.
        let mut guard = RootGuard::lock_parent(&root).unwrap();
        guard.create_leaf().unwrap();
        let mut second = RootGuard::lock_parent(&root).unwrap();
        assert!(second.create_leaf().is_err());
        drop(second);
        assert!(fs::rename(&root, temp.path().join("replacement")).is_err());
        assert!(fs::rename(temp.path(), temp.path().with_extension("replacement")).is_err());
        drop(guard);
        fs::rename(&root, temp.path().join("replacement")).unwrap();
    }
    #[test]
    fn original_junction_is_rejected_before_normalizing_or_creating_leaf() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        let link = temp.path().join("junction");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("marker"), b"generated marker").unwrap();
        let status = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:TD_GENERATED_LINK -Target $env:TD_GENERATED_TARGET | Out-Null"])
            .env("TD_GENERATED_LINK", &link).env("TD_GENERATED_TARGET", &real).status().unwrap();
        assert!(status.success());
        assert!(RootGuard::lock_parent(&link.join("new-fixture")).is_err());
        assert!(!real.join("new-fixture").exists());
        fs::remove_dir(&link).unwrap();
        assert_eq!(fs::read(real.join("marker")).unwrap(), b"generated marker");
    }
}
