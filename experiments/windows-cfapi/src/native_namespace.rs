//! Metadata-only native namespace observation. A failed/incomplete scan is an
//! error, never evidence that missing files should be deleted in the cloud.
use super::*;
use std::os::windows::ffi::OsStringExt;

#[derive(Clone, Debug, Serialize)]
pub struct NativeEntry {
    pub relative_path: PathBuf,
    pub volume_serial: u32,
    pub file_id: u64,
    pub directory: bool,
    pub size: u64,
    pub last_write_time: u64,
    pub attributes: u32,
    pub identity: Option<Vec<u8>>,
    pub cloud: Option<NativeObservation>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InventoryStage {
    BeforeDirectory,
    DirectoryHeld,
}

impl PersistentSession {
    pub fn with_local_directory<T>(
        &self,
        relative: &Path,
        identity: &[u8],
        before_convert: impl FnOnce(u32, u64) -> Result<()>,
        action: impl FnOnce(u32, u64, &dyn Fn() -> Result<()>) -> Result<T>,
    ) -> Result<T> {
        let (_, parents) = self.guard_relative_parent(relative)?;
        let parent = parents
            .last()
            .map(|handle| handle.0)
            .or_else(|| self.guard._handles.last().map(|handle| handle.0))
            .ok_or("directory parent lease missing")?;
        let handle = directory_lease_access(
            parent,
            relative.file_name().ok_or("directory leaf missing")?,
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | WRITE_DAC,
        )?;
        let (handle, tag) = self.namespace_checked_handle(handle)?;
        let (volume, id, _) = file_binding(handle.0)?;
        before_convert(volume, id)?;
        if tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
            hr(
                unsafe {
                    CfConvertToPlaceholder(
                        handle.0,
                        identity.as_ptr().cast(),
                        identity.len() as u32,
                        CF_CONVERT_FLAG_NONE,
                        ptr::null_mut(),
                        ptr::null_mut(),
                    )
                },
                "retain local directory",
            )?;
        }
        directory_observation(handle.0, identity)?;
        let acknowledge = || {
            hr(
                unsafe {
                    CfSetInSyncState(
                        handle.0,
                        CF_IN_SYNC_STATE_IN_SYNC,
                        CF_SET_IN_SYNC_FLAG_NONE,
                        ptr::null_mut(),
                    )
                },
                "acknowledge local directory",
            )
        };
        action(volume, id, &acknowledge)
    }
    pub fn entry_exists(&self, relative: &Path) -> Result<bool> {
        let (_, _parents) = self.guard_relative_parent(relative)?;
        match std::fs::symlink_metadata(self.root().join(relative)) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.to_string()),
        }
    }
    /// Bind a newly published entry while its namespace lease remains held.
    /// This observes metadata only; it is not authority for a content snapshot.
    pub fn with_publication_binding<T>(
        &self,
        relative: &Path,
        identity: &[u8],
        size: u64,
        directory: bool,
        action: impl FnOnce(u32, u64) -> Result<T>,
    ) -> Result<T> {
        let (_, parents) = self.guard_relative_parent(relative)?;
        let parent = parents
            .last()
            .map(|handle| handle.0)
            .or_else(|| self.guard._handles.last().map(|handle| handle.0))
            .ok_or("publication parent lease missing")?;
        let handle = if directory {
            directory_lease(
                parent,
                relative.file_name().ok_or("publication leaf missing")?,
            )?
        } else {
            let raw = unsafe {
                CreateFileW(
                    wide(&self.root().join(relative)).as_ptr(),
                    FILE_READ_ATTRIBUTES | DELETE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OPEN_REPARSE_POINT
                        | FILE_FLAG_OPEN_NO_RECALL
                        | FILE_FLAG_BACKUP_SEMANTICS,
                    ptr::null_mut(),
                )
            };
            if raw == INVALID_HANDLE_VALUE {
                return Err("publication file lease unavailable".into());
            }
            Handle(raw, false)
        };
        let (handle, _) = self.namespace_checked_handle(handle)?;
        let (volume, id, actual_directory) = file_binding(handle.0)?;
        if actual_directory != directory || volume != self.receipt.volume_serial {
            return Err("publication kind or volume changed".into());
        }
        let observation = node_observation(
            handle.0,
            identity,
            usize::try_from(size).map_err(|_| "publication size overflow")?,
            directory,
        )?;
        if !observation.os_in_sync
            || observation.modified_bytes != 0
            || observation.logical_size != size
        {
            return Err("publication metadata already changed".into());
        }
        action(volume, id)
    }

    fn namespace_handle(&self, path: &Path) -> Result<(Handle, FILE_ATTRIBUTE_TAG_INFO)> {
        self.namespace_checked_handle(metadata_handle(path)?)
    }
    fn namespace_checked_handle(
        &self,
        handle: Handle,
    ) -> Result<(Handle, FILE_ATTRIBUTE_TAG_INFO)> {
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
            return Err("cannot inspect namespace entry".into());
        }
        if tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            let state = unsafe {
                CfGetPlaceholderStateFromAttributeTag(tag.FileAttributes, tag.ReparseTag)
            };
            if tag.ReparseTag & 0x20000000 != 0
                || state == CF_PLACEHOLDER_STATE_INVALID
                || state & CF_PLACEHOLDER_STATE_PLACEHOLDER == 0
            {
                return Err("namespace contains an unsupported reparse point".into());
            }
        }
        let (root_id, owner) = root_identity(handle.0)?;
        if root_id != self.receipt.file_id || owner != self.receipt.identity {
            return Err("namespace entry belongs to another root".into());
        }
        Ok((handle, tag))
    }

    /// Inventory ordinary local additions and owned placeholders without reading
    /// contents or following junctions. Ancestors stay retained during each
    /// directory enumeration. The caller must separately prove a missing file's
    /// filesystem identity before issuing any remote deletion.
    pub fn inventory(&self) -> Result<Vec<NativeEntry>> {
        self.inventory_observed(|_, _| Ok(()))
    }

    fn inventory_observed(
        &self,
        mut observe: impl FnMut(InventoryStage, &Path) -> Result<()>,
    ) -> Result<Vec<NativeEntry>> {
        if self.connection.is_none() {
            return Err("native root is disconnected".into());
        }
        let mut directories = vec![(PathBuf::new(), Vec::<(u32, u64)>::new())];
        let mut entries = Vec::new();
        let mut seen = std::collections::HashSet::new();
        while let Some((relative, expected_chain)) = directories.pop() {
            observe(InventoryStage::BeforeDirectory, &relative)?;
            let mut directory = self.root().to_path_buf();
            let mut guards = Vec::new();
            let mut parent_handle = self.guard._handles.last().ok_or("root lease missing")?.0;
            for (index, part) in relative.components().enumerate() {
                if !matches!(part, Component::Normal(_)) {
                    return Err("invalid namespace path".into());
                }
                directory.push(part.as_os_str());
                let (handle, tag) = self
                    .namespace_checked_handle(directory_lease(parent_handle, part.as_os_str())?)?;
                if tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
                    return Err("namespace parent changed kind".into());
                }
                let (volume, id, _) = file_binding(handle.0)?;
                if expected_chain.get(index) != Some(&(volume, id)) {
                    return Err("queued namespace directory identity changed".into());
                }
                parent_handle = handle.0;
                guards.push(handle);
            }
            observe(InventoryStage::DirectoryHeld, &relative)?;
            for child in std::fs::read_dir(&directory).map_err(|e| e.to_string())? {
                let child = child.map_err(|e| e.to_string())?;
                let relative_path = relative.join(child.file_name());
                if relative_path.components().count() > 128 || entries.len() >= 1_000_000 {
                    return Err("native namespace observation limit reached".into());
                }
                let (handle, tag) = self.namespace_handle(&child.path())?;
                let mut information = BY_HANDLE_FILE_INFORMATION::default();
                if unsafe { GetFileInformationByHandle(handle.0, &mut information) } == 0 {
                    return Err("namespace file binding unavailable".into());
                }
                let file_id =
                    ((information.nFileIndexHigh as u64) << 32) | information.nFileIndexLow as u64;
                let directory = tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
                if information.dwVolumeSerialNumber != self.receipt.volume_serial
                    || (!directory && information.nNumberOfLinks != 1)
                    || !seen.insert(file_id)
                {
                    return Err(
                        "namespace identity is ambiguous or belongs to another volume".into(),
                    );
                }
                let size = if directory {
                    0
                } else {
                    ((information.nFileSizeHigh as u64) << 32) | information.nFileSizeLow as u64
                };
                let (identity, cloud) = if tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    let mut buffer = [0u64; 1024];
                    let mut count = 0;
                    hr(
                        unsafe {
                            CfGetPlaceholderInfo(
                                handle.0,
                                CF_PLACEHOLDER_INFO_STANDARD,
                                buffer.as_mut_ptr().cast(),
                                size_of_val(&buffer) as u32,
                                &mut count,
                            )
                        },
                        "inspect namespace placeholder",
                    )?;
                    let offset = offset_of!(CF_PLACEHOLDER_STANDARD_INFO, FileIdentity);
                    if count < offset as u32 {
                        return Err("short namespace placeholder information".into());
                    }
                    let info = unsafe { &*buffer.as_ptr().cast::<CF_PLACEHOLDER_STANDARD_INFO>() };
                    let length = info.FileIdentityLength as usize;
                    if length == 0
                        || length > 4096
                        || offset + length > count as usize
                        || info.SyncRootFileId as u64 != self.receipt.file_id
                    {
                        return Err("invalid namespace placeholder identity".into());
                    }
                    let identity =
                        unsafe { std::slice::from_raw_parts(info.FileIdentity.as_ptr(), length) }
                            .to_vec();
                    let observation = node_observation(
                        handle.0,
                        &identity,
                        usize::try_from(size).map_err(|_| "namespace size overflow")?,
                        directory,
                    )?;
                    (Some(identity), Some(observation))
                } else {
                    (None, None)
                };
                if directory {
                    let mut child_chain = expected_chain.clone();
                    child_chain.push((information.dwVolumeSerialNumber, file_id));
                    directories.push((relative_path.clone(), child_chain));
                }
                entries.push(NativeEntry {
                    relative_path,
                    volume_serial: information.dwVolumeSerialNumber,
                    file_id,
                    directory,
                    size,
                    last_write_time: ((information.ftLastWriteTime.dwHighDateTime as u64) << 32)
                        | information.ftLastWriteTime.dwLowDateTime as u64,
                    attributes: tag.FileAttributes,
                    identity,
                    cloud,
                });
            }
        }
        Ok(entries)
    }

    /// Resolve a previously recorded identity even if it was renamed while a
    /// scan was running. Access denial, unsupported filesystems and transient
    /// errors are NOT absence. A returned path may be outside this root after a
    /// user move; callers must distinguish it from an in-root rename.
    pub fn locate_file_id(&self, volume_serial: u32, file_id: u64) -> Result<Option<PathBuf>> {
        if volume_serial != self.receipt.volume_serial {
            return Err("foreign namespace volume".into());
        }
        let descriptor = FILE_ID_DESCRIPTOR {
            dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32,
            Type: FileIdType,
            Anonymous: FILE_ID_DESCRIPTOR_0 {
                FileId: file_id as i64,
            },
        };
        let raw = unsafe {
            OpenFileById(
                self.guard._handles.last().ok_or("root lease missing")?.0,
                &descriptor,
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                FILE_FLAG_BACKUP_SEMANTICS
                    | FILE_FLAG_OPEN_REPARSE_POINT
                    | FILE_FLAG_OPEN_NO_RECALL,
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            let error = unsafe { GetLastError() };
            return if error == ERROR_FILE_NOT_FOUND {
                Ok(None)
            } else {
                Err(format!("namespace identity lookup: {error}"))
            };
        }
        let handle = Handle(raw, false);
        let (volume, found, _) = file_binding(handle.0)?;
        if volume != volume_serial || found != file_id {
            return Err("namespace lookup identity mismatch".into());
        }
        let mut standard = FILE_STANDARD_INFO::default();
        if unsafe {
            GetFileInformationByHandleEx(
                handle.0,
                FileStandardInfo,
                (&mut standard as *mut FILE_STANDARD_INFO).cast(),
                size_of_val(&standard) as u32,
            )
        } == 0
        {
            return Err("namespace link state unavailable".into());
        }
        if standard.DeletePending || standard.NumberOfLinks == 0 {
            return Ok(None);
        }
        let required = unsafe { GetFinalPathNameByHandleW(handle.0, ptr::null_mut(), 0, 0) };
        if required == 0 || required > 32768 {
            return Err("namespace path unavailable".into());
        }
        let mut path = vec![0u16; required as usize + 1];
        let count =
            unsafe { GetFinalPathNameByHandleW(handle.0, path.as_mut_ptr(), path.len() as u32, 0) };
        if count == 0 || count >= path.len() as u32 {
            return Err("namespace path changed during lookup".into());
        }
        Ok(Some(PathBuf::from(std::ffi::OsString::from_wide(
            &path[..count as usize],
        ))))
    }
}

#[cfg(all(test, feature = "native-test-hooks"))]
mod tests {
    use super::*;
    struct NoData;
    impl HydrationSource for NoData {
        fn size(&self, _: &[u8]) -> Result<usize> {
            Err("inventory must not fetch content".into())
        }
        fn read_range(&self, _: &[u8], _: usize, _: usize, _: &AtomicBool) -> Result<Vec<u8>> {
            Err("inventory must not fetch content".into())
        }
    }
    #[test]
    #[ignore = "explicit NEW generated CFAPI root; file metadata lease rename exclusion without hydration"]
    fn publication_file_lease_blocks_rename_without_fetching_contents() {
        let root = PathBuf::from(
            std::env::var_os("TWODRIVE_APPROVED_PUBLICATION_LEASE_ROOT")
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
        let path = owner
            .publish_placeholder(
                Path::new("generated-online.bin"),
                b"generated-publication-file",
                16,
                false,
                1,
            )
            .unwrap();
        let mut renamed = false;
        let result = owner.with_publication_binding(
            Path::new("generated-online.bin"),
            b"generated-publication-file",
            16,
            false,
            |_, _| {
                let other = owner.root().join("unexpected-rename.bin");
                if std::fs::rename(&path, &other).is_ok() {
                    renamed = true;
                    std::fs::rename(&other, &path).map_err(|e| e.to_string())?;
                }
                Ok(())
            },
        );
        let fetches = owner.source.as_ref().unwrap().calls.load(Ordering::SeqCst);
        std::fs::remove_file(&path).unwrap();
        owner.retire_empty_office_fixture().unwrap();
        drop(owner);
        std::fs::remove_dir(root).unwrap();
        std::fs::remove_file(checkpoint).unwrap();
        result.unwrap();
        assert_eq!(fetches, 0);
        assert!(!renamed, "file attributes alone did not exclude rename");
    }
    fn substitution_case(held: bool, replacement_file: bool) {
        let base = PathBuf::from(
            std::env::var_os("TWODRIVE_APPROVED_NAMESPACE_RACE_ROOT")
                .expect("explicit new generated root"),
        );
        assert!(
            base.is_absolute()
                && base
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("twodrive-cfapi-fixture-office-")
        );
        let root = base.with_file_name(format!(
            "{}-{}",
            base.file_name().unwrap().to_string_lossy(),
            if held {
                "held"
            } else if replacement_file {
                "queued-file"
            } else {
                "queued"
            }
        ));
        assert!(!root.exists());
        let checkpoint = root.with_extension("receipt.json");
        let mut owner = PersistentSession::create_new(
            &root,
            b"twodrive-dev-native-v1:generated-office-only",
            &checkpoint,
            Arc::new(NoData),
        )
        .unwrap();
        let work = root.join("work");
        let moved = root.join("moved");
        std::fs::create_dir(&work).unwrap();
        std::fs::write(work.join("old.txt"), b"old").unwrap();
        let mut attempted = false;
        let mut replaced = false;
        let result = owner.inventory_observed(|stage, path| {
            let expected = if held {
                InventoryStage::DirectoryHeld
            } else {
                InventoryStage::BeforeDirectory
            };
            if !attempted && stage == expected && path == Path::new("work") {
                attempted = true;
                if held {
                    if std::fs::rename(&work, &moved).is_ok() {
                        replaced = true;
                        std::fs::rename(&moved, &work).map_err(|e| e.to_string())?;
                    }
                } else {
                    std::fs::rename(&work, &moved).map_err(|e| e.to_string())?;
                    if replacement_file {
                        owner.publish_placeholder(
                            Path::new("work"),
                            b"substituted-generated-file",
                            65536,
                            false,
                            1,
                        )?;
                    } else {
                        std::fs::create_dir(&work).map_err(|e| e.to_string())?;
                        std::fs::write(work.join("new.txt"), b"new").map_err(|e| e.to_string())?;
                    }
                    replaced = true;
                }
            }
            Ok(())
        });
        let no_fetch = owner.source.as_ref().unwrap().calls.load(Ordering::SeqCst) == 0;
        // Retire only this known ordinary generated tree before asserting the
        // race oracle, so an old-code reproduction does not leave a live root.
        if held {
            assert_eq!(std::fs::read(work.join("old.txt")).unwrap(), b"old");
            std::fs::remove_file(work.join("old.txt")).unwrap();
        } else {
            if replacement_file {
                std::fs::remove_file(&work).unwrap();
            } else {
                assert_eq!(std::fs::read(work.join("new.txt")).unwrap(), b"new");
                std::fs::remove_file(work.join("new.txt")).unwrap();
            }
            assert_eq!(std::fs::read(moved.join("old.txt")).unwrap(), b"old");
            std::fs::remove_file(moved.join("old.txt")).unwrap();
            std::fs::remove_dir(&moved).unwrap();
        }
        if !replacement_file {
            std::fs::remove_dir(&work).unwrap();
        }
        owner.retire_empty_office_fixture().unwrap();
        drop(owner);
        std::fs::remove_dir(&root).unwrap();
        std::fs::remove_file(&checkpoint).unwrap();
        assert!(
            attempted && no_fetch,
            "attempted={attempted} no_fetch={no_fetch} result={:?}",
            result.as_ref().map(|entries| entries.len())
        );
        if held {
            assert!(
                !replaced,
                "a directory was renamed while its traversal lease was held"
            );
            assert!(result.is_ok());
        } else {
            assert!(replaced);
            assert!(
                result.is_err(),
                "queued directory substitution produced a mixed inventory"
            );
        }
    }
    #[test]
    #[ignore = "explicit NEW generated root; replacement while directory is being traversed"]
    fn inventory_rejects_replacement_during_directory_traversal() {
        substitution_case(true, false);
    }
    #[test]
    #[ignore = "explicit NEW generated root; replacement after enqueue before traversal"]
    fn inventory_rejects_replacement_of_a_queued_directory() {
        substitution_case(false, false);
    }
    #[test]
    #[ignore = "explicit NEW generated root; queued directory replaced by an online-only file"]
    fn inventory_rejects_replacement_by_a_file_without_hydration() {
        substitution_case(false, true);
    }
    #[test]
    #[ignore = "explicit NEW generated CFAPI root; metadata-only inventory and file-ID lookup"]
    fn inventory_and_identity_lookup_track_renames_without_inventing_deletion_proof() {
        let root = PathBuf::from(
            std::env::var_os("TWODRIVE_APPROVED_NAMESPACE_ROOT")
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
        let outside = root.with_extension("outside");
        std::fs::create_dir(&outside).unwrap();
        let mut owner = PersistentSession::create_new(
            &root,
            b"twodrive-dev-native-v1:generated-office-only",
            &checkpoint,
            Arc::new(NoData),
        )
        .unwrap();
        owner
            .publish_placeholder(Path::new("cloud"), b"cloud-directory", 0, true, 1)
            .unwrap();
        let placeholder = owner
            .publish_placeholder(
                Path::new("cloud/online.bin"),
                b"cloud-file",
                1024 * 1024,
                false,
                1,
            )
            .unwrap();
        std::fs::create_dir(root.join("local")).unwrap();
        let local = root.join("local/new.txt");
        std::fs::write(&local, b"generated local content").unwrap();
        let entries = owner.inventory().unwrap();
        assert_eq!(entries.len(), 4);
        let native = entries
            .iter()
            .find(|entry| entry.relative_path == Path::new("cloud/online.bin"))
            .unwrap();
        assert_eq!(native.identity.as_deref(), Some(b"cloud-file".as_slice()));
        assert!(!native.cloud.as_ref().unwrap().resident);
        assert_eq!(native.size, 1024 * 1024);
        let ordinary = entries
            .iter()
            .find(|entry| entry.relative_path == Path::new("local/new.txt"))
            .unwrap();
        assert!(ordinary.identity.is_none() && ordinary.cloud.is_none());
        let renamed = root.join("local/renamed.txt");
        std::fs::rename(&local, &renamed).unwrap();
        assert_eq!(
            owner
                .locate_file_id(ordinary.volume_serial, ordinary.file_id)
                .unwrap(),
            Some(renamed.canonicalize().unwrap())
        );
        let moved = outside.join("moved.txt");
        std::fs::rename(&renamed, &moved).unwrap();
        assert_eq!(
            owner
                .locate_file_id(ordinary.volume_serial, ordinary.file_id)
                .unwrap(),
            Some(moved.canonicalize().unwrap())
        );
        assert_eq!(owner.inventory().unwrap().len(), 3);
        std::fs::remove_file(&moved).unwrap();
        match owner.locate_file_id(ordinary.volume_serial, ordinary.file_id) {
            Ok(None) => {}
            // NTFS may reject an expired file reference with INVALID_PARAMETER.
            // Keep this as an error, not permission to delete a cloud object.
            Err(error) if error == "namespace identity lookup: 87" => {}
            other => panic!("unexpected deleted identity observation: {other:?}"),
        }
        assert!(
            owner
                .locate_file_id(ordinary.volume_serial.wrapping_add(1), ordinary.file_id)
                .is_err()
        );
        let junction = root.join("untrusted");
        assert!(
            std::process::Command::new("cmd.exe")
                .args(["/D", "/C", "mklink", "/J"])
                .arg(&junction)
                .arg(&outside)
                .output()
                .unwrap()
                .status
                .success()
        );
        assert!(owner.inventory().is_err());
        assert!(owner.namespace_handle(&junction).is_err());
        assert_eq!(
            owner.source.as_ref().unwrap().calls.load(Ordering::SeqCst),
            0
        );
        std::fs::remove_dir(&junction).unwrap();
        std::fs::remove_file(&placeholder).unwrap();
        std::fs::remove_dir(root.join("cloud")).unwrap();
        std::fs::remove_dir(root.join("local")).unwrap();
        owner.retire_empty_office_fixture().unwrap();
        drop(owner);
        std::fs::remove_dir(&root).unwrap();
        std::fs::remove_dir(&outside).unwrap();
        std::fs::remove_file(&checkpoint).unwrap();
    }
}
