//! Read-only provenance and complete-data export from an existing registered
//! root. No registration, connection, pin, conversion or deletion API is used.
use super::*;

pub fn personal_source_location() -> Result<(String, PathBuf)> {
    use windows_sys::Win32::System::Registry::*;
    fn read(name: &str) -> Result<String> {
        let key = wide(Path::new(
            "Software\\Microsoft\\OneDrive\\Accounts\\Personal",
        ));
        let name = wide(Path::new(name));
        let mut bytes = 0u32;
        let first = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut bytes,
            )
        };
        if first != ERROR_SUCCESS || !(2..=65536).contains(&bytes) || !bytes.is_multiple_of(2) {
            return Err("personal source registry value unavailable".into());
        }
        let mut buffer = vec![0u16; bytes as usize / 2];
        if unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut bytes,
            )
        } != ERROR_SUCCESS
        {
            return Err("personal source registry read failed".into());
        }
        buffer.truncate(bytes as usize / 2);
        if buffer.pop() != Some(0) || buffer.contains(&0) {
            return Err("personal source registry string invalid".into());
        }
        String::from_utf16(&buffer).map_err(|_| "personal source registry encoding invalid".into())
    }
    let owner = read("CID")?;
    if owner.len() != 16 || !owner.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("personal source owner invalid".into());
    }
    Ok((owner, PathBuf::from(read("UserFolder")?)))
}

pub struct ReadOnlyRegisteredRoot {
    guard: RootGuard,
    receipt: RootReceipt,
}
impl ReadOnlyRegisteredRoot {
    pub fn open(root: &Path) -> Result<Self> {
        let mut guard = RootGuard::lock_parent(root)?;
        let raw = unsafe {
            CreateFileW(
                wide(root).as_ptr(),
                FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS
                    | FILE_FLAG_OPEN_REPARSE_POINT
                    | FILE_FLAG_OPEN_NO_RECALL,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err("registered source root lease unavailable".into());
        }
        let handle = Handle(raw, false);
        let (volume_serial, file_id, directory) = file_binding(handle.0)?;
        let (registered_id, identity) = root_identity(handle.0)?;
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
            || registered_id != file_id
            || tag.ReparseTag & 0x20000000 != 0
        {
            return Err(
                "source must be the exact registered directory, not a link or cache".into(),
            );
        }
        guard.root = root.canonicalize().map_err(|e| e.to_string())?;
        guard._handles.push(handle);
        Ok(Self {
            guard,
            receipt: RootReceipt {
                schema_version: 1,
                volume_serial,
                file_id,
                identity,
            },
        })
    }
    pub fn root(&self) -> &Path {
        &self.guard.root
    }
    pub fn receipt(&self) -> &RootReceipt {
        &self.receipt
    }
    pub fn inventory(&self) -> Result<Vec<NativeEntry>> {
        namespace::inventory_under_root(
            self.root(),
            self.guard
                ._handles
                .last()
                .ok_or("source root lease missing")?
                .0,
            &self.receipt,
            namespace::NamespaceIdentityPolicy::RegisteredSource,
            |_, _| Ok(()),
        )
    }
    /// Deny recalls and concurrent mutation. The caller still needs an
    /// independently bound service checksum/version before importing this copy.
    pub fn copy_complete_clean(
        &self,
        entry: &NativeEntry,
        output: &mut impl std::io::Write,
    ) -> Result<u64> {
        if entry.directory || entry.volume_serial != self.receipt.volume_serial {
            return Err("source file identity invalid".into());
        }
        if entry.attributes & FILE_ATTRIBUTE_ENCRYPTED != 0
            || entry.cloud.as_ref().is_none_or(|os| {
                !os.resident || os.partial || !os.os_in_sync || os.modified_bytes != 0
            })
        {
            return Err("source inventory is not a complete clean export candidate".into());
        }
        let identity = entry
            .identity
            .as_deref()
            .ok_or("ordinary source file requires separate recovery")?;
        let (_, _parents) = guard_registered_parent(
            self.root(),
            self.guard
                ._handles
                .last()
                .ok_or("source root lease missing")?
                .0,
            &self.receipt,
            &entry.relative_path,
        )?;
        // DELETE access is only a sharing/mapped-file veto. This type never
        // requests delete-on-close or issues a disposition operation.
        let raw = unsafe {
            CreateFileW(
                wide(&self.root().join(&entry.relative_path)).as_ptr(),
                FILE_READ_ATTRIBUTES | DELETE,
                FILE_SHARE_READ,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT
                    | FILE_FLAG_OPEN_NO_RECALL
                    | FILE_FLAG_BACKUP_SEMANTICS,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err("source content is busy; export deferred".into());
        }
        let (handle, _) = namespace::checked_namespace_handle(Handle(raw, false), &self.receipt)?;
        let (volume, id, directory) = file_binding(handle.0)?;
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(handle.0, &mut information) } == 0 {
            return Err("source information unavailable".into());
        }
        let stamp = ((information.ftLastWriteTime.dwHighDateTime as u64) << 32)
            | information.ftLastWriteTime.dwLowDateTime as u64;
        if directory
            || id != entry.file_id
            || volume != entry.volume_serial
            || information.nNumberOfLinks != 1
            || stamp != entry.last_write_time
        {
            return Err("source file changed since inventory".into());
        }
        let metadata_lease = handle;
        let current = node_observation(
            metadata_lease.0,
            identity,
            usize::try_from(entry.size).map_err(|_| "source size overflow")?,
            false,
        )?;
        if !current.resident
            || current.partial
            || !current.os_in_sync
            || current.modified_bytes != 0
        {
            return Err("current source metadata is no longer a complete clean export".into());
        }
        // The metadata lease denies writes and deletion while opening data.
        // Requesting READ_DATA before this fresh check can recall a stale file,
        // even with OPEN_NO_RECALL on this host.
        let data = unsafe {
            CreateFileW(
                wide(&self.root().join(&entry.relative_path)).as_ptr(),
                FILE_READ_DATA | FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_DELETE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT
                    | FILE_FLAG_OPEN_NO_RECALL
                    | FILE_FLAG_BACKUP_SEMANTICS,
                ptr::null_mut(),
            )
        };
        if data == INVALID_HANDLE_VALUE {
            return Err("source data lease unavailable".into());
        }
        let (handle, _) = namespace::checked_namespace_handle(Handle(data, false), &self.receipt)?;
        let data_binding = file_binding(handle.0)?;
        if data_binding != (entry.volume_serial, entry.file_id, false) {
            return Err("source data identity changed".into());
        }
        let lease = RetainedPlaceholder {
            handle,
            identity: identity.to_vec(),
            size: usize::try_from(entry.size).map_err(|_| "source size overflow")?,
            directory: false,
        };
        let before = lease.inspect()?;
        if !before.resident || before.partial || !before.os_in_sync || before.modified_bytes != 0 {
            return Err("source must be complete and clean; no hydration is attempted".into());
        }
        let capture = NativeCapture(&lease);
        let modified = capture.modified_time()?;
        let count = capture.copy_resident_to(output)?;
        let after = lease.inspect()?;
        if count != entry.size
            || modified != capture.modified_time()?
            || !after.os_in_sync
            || after.modified_bytes != 0
            || !after.resident
            || after.partial
        {
            return Err("source export changed or became incomplete".into());
        }
        if file_binding(metadata_lease.0)? != (entry.volume_serial, entry.file_id, false) {
            return Err("source metadata lease changed".into());
        }
        Ok(count)
    }
}

#[cfg(all(test, feature = "native-test-hooks"))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    const BYTES: &[u8] = b"generated source file retained in full";
    struct Bytes(Arc<AtomicUsize>);
    impl HydrationSource for Bytes {
        fn size(&self, _: &[u8]) -> Result<usize> {
            Ok(BYTES.len())
        }
        fn read_range(
            &self,
            _: &[u8],
            offset: usize,
            count: usize,
            _: &AtomicBool,
        ) -> Result<Vec<u8>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(BYTES[offset..offset + count].to_vec())
        }
    }
    #[test]
    #[ignore = "explicit NEW generated registered source root; metadata/stale-residency no-recall and complete clean export"]
    fn registered_source_observation_never_recalls_and_export_refuses_dirty_or_busy_data() {
        let root = PathBuf::from(
            std::env::var_os("TWODRIVE_APPROVED_READONLY_SOURCE_ROOT")
                .expect("generated source root required"),
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
        let calls = Arc::new(AtomicUsize::new(0));
        let mut owner = PersistentSession::create_new(
            &root,
            b"twodrive-dev-native-v1:generated-office-only",
            &checkpoint,
            Arc::new(Bytes(calls.clone())),
        )
        .unwrap();
        let tag = b"generated-readonly-source-file-tag";
        let file = owner
            .publish_placeholder(
                Path::new("generated.txt"),
                tag,
                BYTES.len() as u64,
                false,
                1,
            )
            .unwrap();
        let source = ReadOnlyRegisteredRoot::open(&root).unwrap();
        let cold = source.inventory().unwrap().remove(0);
        let mut output = Vec::new();
        let cold_refused = source.copy_complete_clean(&cold, &mut output).is_err();
        let no_cold_read = calls.load(Ordering::SeqCst) == 0 && output.is_empty();
        assert_eq!(std::fs::read(&file).unwrap(), BYTES);
        let resident = source.inventory().unwrap().remove(0);
        super::super::super::with_exclusive_placeholder(&file, tag, BYTES.len(), |locked| {
            locked.dehydrate()
        })
        .unwrap();
        let before = calls.load(Ordering::SeqCst);
        let stale_refused = source.copy_complete_clean(&resident, &mut output).is_err();
        let no_stale_read = calls.load(Ordering::SeqCst) == before && output.is_empty();
        assert_eq!(std::fs::read(&file).unwrap(), BYTES);
        let resident = source.inventory().unwrap().remove(0);
        let count = source.copy_complete_clean(&resident, &mut output).unwrap();
        let writer = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
        let mut blocked = Vec::new();
        let busy_refused = source.copy_complete_clean(&resident, &mut blocked).is_err();
        drop(writer);
        std::fs::write(&file, b"generated source edit must be retained").unwrap();
        let dirty = source.inventory().unwrap().remove(0);
        let dirty_refused = source.copy_complete_clean(&dirty, &mut blocked).is_err();
        let dirty_bytes = std::fs::read(&file).unwrap();
        let ordinary = tempfile::tempdir().unwrap();
        let cache_refused = ReadOnlyRegisteredRoot::open(ordinary.path()).is_err();
        drop(source);
        std::fs::remove_file(file).unwrap();
        owner.retire_empty_office_fixture().unwrap();
        drop(owner);
        std::fs::remove_dir(root).unwrap();
        std::fs::remove_file(checkpoint).unwrap();
        ordinary.close().unwrap();
        assert!(
            cold_refused && no_cold_read && stale_refused && no_stale_read,
            "cold_refused={cold_refused}, no_cold_read={no_cold_read}, stale_refused={stale_refused}, no_stale_read={no_stale_read}"
        );
        assert_eq!(count, BYTES.len() as u64);
        assert_eq!(output, BYTES);
        assert!(busy_refused && dirty_refused && cache_refused && blocked.is_empty());
        assert_eq!(dirty_bytes, b"generated source edit must be retained");
    }
}
