//! Persistent CFAPI registration. This platform layer owns no cloud credentials
//! and performs no adoption: new roots must not exist; resume requires the exact
//! registration identity, volume and file ID saved by the engine.
use super::*;
use serde::{Deserialize, Serialize};

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
    /// The caller persists the account-bound identity and creation intent before
    /// calling this method, then stores receipt() before publishing any files.
    pub fn create_new(
        root: &Path,
        identity: &[u8],
        engine: Arc<dyn HydrationSource>,
    ) -> Result<Self> {
        valid_identity(identity)?;
        let mut guard = RootGuard::lock_parent(root)?;
        guard.require_unmanaged_parent()?;
        guard.create_leaf()?;
        let handle = guard._handles.last().ok_or("missing root handle")?.0;
        let (volume_serial, file_id, directory) = file_binding(handle)?;
        if !directory {
            return Err("native root must be a directory".into());
        }
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
        // Never unregister on startup failure: a concurrent user can already
        // have written data. The durable creation intent permits explicit resume.
        let receipt = RootReceipt {
            schema_version: 1,
            volume_serial,
            file_id,
            identity: identity.to_vec(),
        };
        Self::connect_owned(guard, receipt, engine)
    }

    pub fn resume(
        root: &Path,
        receipt: &RootReceipt,
        engine: Arc<dyn HydrationSource>,
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
        let (registered_id, identity) = root_identity(handle.0)?;
        if !directory
            || volume != receipt.volume_serial
            || id != receipt.file_id
            || id != registered_id
            || identity != receipt.identity
        {
            return Err("native root registration or filesystem identity changed".into());
        }
        guard._handles.push(handle);
        guard.root = root.canonicalize().map_err(|e| e.to_string())?;
        Self::connect_owned(guard, receipt.clone(), engine)
    }

    fn connect_owned(
        guard: RootGuard,
        receipt: RootReceipt,
        engine: Arc<dyn HydrationSource>,
    ) -> Result<Self> {
        let source = Box::new(Source {
            engine: Some(engine),
            ..Default::default()
        });
        let table = [
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
            CF_CALLBACK_REGISTRATION {
                Type: CF_CALLBACK_TYPE_NONE,
                Callback: None,
            },
        ];
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
            Arc::new(Generated),
        )
        .unwrap();
        let receipt = session.receipt().clone();
        for id in ["first", "offline"] {
            let name = wide(Path::new(&format!("{id}.bin")));
            let mut item = CF_PLACEHOLDER_CREATE_INFO {
                RelativeFileName: name.as_ptr(),
                FileIdentity: id.as_ptr().cast(),
                FileIdentityLength: id.len() as u32,
                Flags: CF_PLACEHOLDER_CREATE_FLAG_MARK_IN_SYNC,
                ..Default::default()
            };
            item.FsMetadata.FileSize = BYTES.len() as i64;
            item.FsMetadata.BasicInfo.FileAttributes = FILE_ATTRIBUTE_NORMAL;
            let mut done = 0;
            hr(
                unsafe {
                    CfCreatePlaceholders(
                        wide(&root).as_ptr(),
                        &mut item,
                        1,
                        CF_CREATE_FLAG_STOP_ON_ERROR,
                        &mut done,
                    )
                },
                "generated persistent placeholder",
            )
            .unwrap();
            hr(item.Result, "generated entry").unwrap();
            assert_eq!(done, 1);
        }
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
