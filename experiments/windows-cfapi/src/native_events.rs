//! Persist native intent before allowing an OS operation. Completion events
//! and current file identity still need reconciliation; requests alone are not
//! authority to mutate the cloud.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeEventKind {
    DeleteRequested,
    DeleteCompleted,
    RenameRequested,
    RenameCompleted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeEvent {
    pub schema_version: u32,
    pub kind: NativeEventKind,
    pub volume_serial: u32,
    pub root_file_id: u64,
    pub file_id: u64,
    pub identity: Vec<u8>,
    /// Kernel-supplied volume-relative paths; not cloud paths.
    pub path: String,
    pub related_path: Option<String>,
    pub flags: u32,
    pub connection_key: i64,
    pub request_key: i64,
}

pub trait NativeEventSink: Send + Sync {
    /// Success must mean durable local persistence, not an in-memory send.
    fn record(&self, event: &NativeEvent) -> Result<()>;
}

pub struct DatabaseEventSink {
    db: twodrive_core::Database,
    binding: String,
}
impl DatabaseEventSink {
    pub fn new(db: twodrive_core::Database, binding: String) -> Result<Self> {
        if binding.is_empty() || binding.len() > 16384 {
            return Err("invalid event owner binding".into());
        }
        Ok(Self { db, binding })
    }
}
impl NativeEventSink for DatabaseEventSink {
    fn record(&self, event: &NativeEvent) -> Result<()> {
        let payload = serde_json::to_string(event).map_err(|e| e.to_string())?;
        self.db
            .append_native_event(&self.binding, &payload)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

pub(crate) struct Context {
    receipt: RootReceipt,
    sink: Arc<dyn NativeEventSink>,
}
impl Context {
    pub(crate) fn new(receipt: RootReceipt, sink: Arc<dyn NativeEventSink>) -> Self {
        Self { receipt, sink }
    }
    fn record(
        &self,
        info: &CF_CALLBACK_INFO,
        kind: NativeEventKind,
        flags: u32,
        related: *const u16,
    ) -> Result<()> {
        if info.VolumeSerialNumber != self.receipt.volume_serial
            || info.SyncRootFileId as u64 != self.receipt.file_id
            || info.SyncRootIdentity.is_null()
            || info.SyncRootIdentityLength as usize != self.receipt.identity.len()
        {
            return Err("native event root binding mismatch".into());
        }
        let root = unsafe {
            std::slice::from_raw_parts(
                info.SyncRootIdentity.cast::<u8>(),
                info.SyncRootIdentityLength as usize,
            )
        };
        if root != self.receipt.identity {
            return Err("native event root identity mismatch".into());
        }
        if info.FileId as u64 == self.receipt.file_id {
            return Err("root mutation is not supported".into());
        }
        if info.FileIdentityLength > 4096
            || (info.FileIdentityLength > 0 && info.FileIdentity.is_null())
        {
            return Err("invalid native event identity".into());
        }
        // Incoming ordinary files may not yet have a provider identity. Their
        // physical ID/path is an observation, never an existing cloud object ID.
        let identity = if info.FileIdentityLength == 0 {
            Vec::new()
        } else {
            unsafe {
                std::slice::from_raw_parts(
                    info.FileIdentity.cast::<u8>(),
                    info.FileIdentityLength as usize,
                )
            }
            .to_vec()
        };
        self.sink.record(&NativeEvent {
            schema_version: 1,
            kind,
            volume_serial: info.VolumeSerialNumber,
            root_file_id: info.SyncRootFileId as u64,
            file_id: info.FileId as u64,
            identity,
            path: unsafe { bounded_path(info.NormalizedPath) }?,
            related_path: if related.is_null() {
                None
            } else {
                Some(unsafe { bounded_path(related) }?)
            },
            flags,
            connection_key: info.ConnectionKey,
            request_key: info.RequestKey,
        })
    }
}

/// All pointers originate in the synchronous CldApi callback and are copied
/// before it returns. No borrowed callback pointers enter the durable queue.
unsafe fn bounded_path(value: *const u16) -> Result<String> {
    if value.is_null() {
        return Err("missing native event path".into());
    }
    for size in 0..32768 {
        if unsafe { *value.add(size) } == 0 {
            if size == 0 {
                return Err("empty native event path".into());
            }
            return String::from_utf16(unsafe { std::slice::from_raw_parts(value, size) })
                .map_err(|_| "invalid native event path".into());
        }
    }
    Err("oversized native event path".into())
}

fn record(
    info: &CF_CALLBACK_INFO,
    kind: NativeEventKind,
    flags: u32,
    related: *const u16,
    request: bool,
) -> bool {
    let source = unsafe { &*info.CallbackContext.cast::<Source>() };
    let _callback = source.enter_callback();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        if request && source.stopping.load(Ordering::SeqCst) {
            return Err("native owner is stopping".into());
        }
        source
            .events
            .as_ref()
            .ok_or("native event owner missing")?
            .record(info, kind, flags, related)
    }));
    let succeeded = matches!(result, Ok(Ok(())));
    if !succeeded {
        source.failures.fetch_add(1, Ordering::SeqCst);
    }
    succeeded
}
pub(super) unsafe extern "system" fn delete_requested(
    info: *const CF_CALLBACK_INFO,
    parameters: *const CF_CALLBACK_PARAMETERS,
) {
    let info = unsafe { &*info };
    let flags = unsafe { (*parameters).Anonymous.Delete.Flags } as u32;
    let status = if record(
        info,
        NativeEventKind::DeleteRequested,
        flags,
        ptr::null(),
        true,
    ) {
        0
    } else {
        0xC0000022u32 as i32
    };
    let _ = acknowledge_mutation(info, CF_OPERATION_TYPE_ACK_DELETE, status);
}
pub(super) unsafe extern "system" fn delete_completed(
    info: *const CF_CALLBACK_INFO,
    parameters: *const CF_CALLBACK_PARAMETERS,
) {
    let info = unsafe { &*info };
    let flags = unsafe { (*parameters).Anonymous.DeleteCompletion.Flags } as u32;
    record(
        info,
        NativeEventKind::DeleteCompleted,
        flags,
        ptr::null(),
        false,
    );
}
pub(super) unsafe extern "system" fn rename_requested(
    info: *const CF_CALLBACK_INFO,
    parameters: *const CF_CALLBACK_PARAMETERS,
) {
    let info = unsafe { &*info };
    let rename = unsafe { (*parameters).Anonymous.Rename };
    let status = if !rename.TargetPath.is_null()
        && record(
            info,
            NativeEventKind::RenameRequested,
            rename.Flags as u32,
            rename.TargetPath,
            true,
        ) {
        0
    } else {
        0xC0000022u32 as i32
    };
    let _ = acknowledge_mutation(info, CF_OPERATION_TYPE_ACK_RENAME, status);
}
pub(super) unsafe extern "system" fn rename_completed(
    info: *const CF_CALLBACK_INFO,
    parameters: *const CF_CALLBACK_PARAMETERS,
) {
    let info = unsafe { &*info };
    let rename = unsafe { (*parameters).Anonymous.RenameCompletion };
    record(
        info,
        NativeEventKind::RenameCompleted,
        rename.Flags as u32,
        rename.SourcePath,
        false,
    );
}

#[cfg(all(test, feature = "native-test-hooks"))]
mod tests {
    use super::*;
    struct NoData;
    impl HydrationSource for NoData {
        fn size(&self, _: &[u8]) -> Result<usize> {
            Err("no content in mutation fixture".into())
        }
        fn read_range(&self, _: &[u8], _: usize, _: usize, _: &AtomicBool) -> Result<Vec<u8>> {
            Err("no content".into())
        }
    }
    struct Switch {
        fail: AtomicBool,
        storage: DatabaseEventSink,
    }
    impl NativeEventSink for Switch {
        fn record(&self, event: &NativeEvent) -> Result<()> {
            if self.fail.load(Ordering::SeqCst) {
                return Err("injected durable append failure".into());
            }
            self.storage.record(event)
        }
    }
    fn queued(db: &twodrive_core::Database) -> Vec<(i64, NativeEvent)> {
        db.pending_native_events("generated-owner", 100)
            .unwrap()
            .into_iter()
            .map(|(seq, payload)| (seq, serde_json::from_str(&payload).unwrap()))
            .collect()
    }
    #[test]
    #[ignore = "child helper for the explicit generated mutation test"]
    fn native_mutation_child() {
        let root =
            PathBuf::from(std::env::var_os("TWODRIVE_APPROVED_EVENT_ROOT").expect("explicit root"));
        assert!(
            root.is_absolute()
                && root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("twodrive-cfapi-fixture-office-events-")
        );
        let handle = directory_handle(&root).unwrap();
        assert_eq!(
            root_identity(handle.0).unwrap().1,
            b"twodrive-dev-native-v1:generated-office-only"
        );
        let original = root.join("original.txt");
        let renamed = root.join("renamed.txt");
        match std::env::var("TWODRIVE_GENERATED_MUTATION")
            .unwrap()
            .as_str()
        {
            "deny_rename" => assert!(std::fs::rename(original, renamed).is_err()),
            "rename" => std::fs::rename(original, renamed).unwrap(),
            "delete" => std::fs::remove_file(renamed).unwrap(),
            _ => panic!("unknown generated mutation"),
        }
    }
    fn external_mutation(action: &str) {
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native::persistent::events::tests::native_mutation_child",
                "--ignored",
                "--nocapture",
            ])
            .env("TWODRIVE_GENERATED_MUTATION", action)
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "generated child failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[test]
    #[ignore = "explicit NEW generated CFAPI root; real rename/delete callbacks and durable queue"]
    fn native_mutations_require_durable_intent_and_keep_completion_after_restart() {
        let root = PathBuf::from(
            std::env::var_os("TWODRIVE_APPROVED_EVENT_ROOT").expect("explicit new fixture"),
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
        let state = tempfile::tempdir().unwrap();
        let db = twodrive_core::Database::new(state.path().join("events.sqlite3"));
        db.init().unwrap();
        let sink = Arc::new(Switch {
            fail: AtomicBool::new(true),
            storage: DatabaseEventSink::new(db.clone(), "generated-owner".into()).unwrap(),
        });
        let mut owner = PersistentSession::create_new_recording(
            &root,
            b"twodrive-dev-native-v1:generated-office-only",
            &checkpoint,
            Arc::new(NoData),
            sink.clone(),
        )
        .unwrap();
        let original = owner
            .publish_placeholder(
                Path::new("original.txt"),
                b"generated-mutation-object",
                4096,
                false,
                1,
            )
            .unwrap();
        let renamed = root.join("renamed.txt");
        // The provider's own filesystem operations are not application events.
        // Exercise a separate process, as Office/Explorer would do.
        external_mutation("deny_rename");
        assert!(original.exists() && !renamed.exists());
        assert!(queued(&db).is_empty());
        sink.fail.store(false, Ordering::SeqCst);
        external_mutation("rename");
        external_mutation("delete");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !queued(&db)
            .iter()
            .any(|(_, e)| e.kind == NativeEventKind::DeleteCompleted)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let events = queued(&db);
        for kind in [
            NativeEventKind::RenameRequested,
            NativeEventKind::RenameCompleted,
            NativeEventKind::DeleteRequested,
            NativeEventKind::DeleteCompleted,
        ] {
            assert!(
                events.iter().any(|(_, event)| event.kind == kind),
                "missing event {kind:?}"
            );
        }
        let first_id = events[0].1.file_id;
        assert!(events.iter().all(|(_, event)| event.schema_version == 1
            && event.root_file_id == owner.receipt.file_id
            && event.volume_serial == owner.receipt.volume_serial
            && event.file_id == first_id
            && event.identity == b"generated-mutation-object"));
        assert!(events.iter().any(|(_, event)| {
            event.kind == NativeEventKind::RenameRequested
                && event
                    .related_path
                    .as_ref()
                    .is_some_and(|path| path.ends_with("renamed.txt"))
        }));
        assert_eq!(
            owner.source.as_ref().unwrap().calls.load(Ordering::SeqCst),
            0
        );
        // Ordinary local files have not yet acquired a provider identity.
        // Observe their callback coverage instead of assuming it matches CFAPI
        // placeholders; creation still needs an independent watcher/inventory.
        std::fs::write(&original, b"ordinary generated content").unwrap();
        external_mutation("rename");
        external_mutation("delete");
        let all_events = queued(&db);
        println!(
            "ordinary generated rename/delete events: {}",
            all_events.len() - events.len()
        );
        owner.disconnect().unwrap();
        drop(owner);
        let reopened = twodrive_core::Database::new(db.path());
        reopened.init().unwrap();
        assert_eq!(queued(&reopened).len(), all_events.len());
        assert!(
            reopened
                .pending_native_events("another-owner", 100)
                .unwrap()
                .is_empty()
        );
        let mut resumed = PersistentSession::resume_from_checkpoint_recording(
            &root,
            &checkpoint,
            Arc::new(NoData),
            sink,
        )
        .unwrap();
        reopened
            .acknowledge_native_events("generated-owner", all_events.last().unwrap().0)
            .unwrap();
        assert!(queued(&reopened).is_empty());
        resumed.retire_empty_office_fixture().unwrap();
        drop(resumed);
        std::fs::remove_dir(&root).unwrap();
        std::fs::remove_file(&checkpoint).unwrap();
    }
}
