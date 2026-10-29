//! Shell presentation for an already guarded generated root. Never enumerates
//! other registrations, installs a COM extension, or accepts account identities.
use std::{
    collections::HashMap,
    marker::PhantomData,
    path::{Path, PathBuf},
    rc::Rc,
};
use windows::{
    Security::Cryptography::CryptographicBuffer,
    Storage::{IStorageItem, Provider::*, StorageFile, StorageFolder},
    Win32::{
        Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree},
        Security::{
            Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_QUERY, TOKEN_USER,
            TokenUser,
        },
        System::{
            Threading::{GetCurrentProcess, OpenProcessToken},
            WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
        },
        UI::Shell::{SHCNE_UPDATEDIR, SHCNE_UPDATEITEM, SHCNF_PATHW, SHChangeNotify},
    },
    core::{GUID, HSTRING, Result},
};

struct Apartment(PhantomData<Rc<()>>);
impl Apartment {
    fn new() -> Result<Self> {
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED)?;
        }
        Ok(Self(PhantomData))
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            RoUninitialize();
        }
    }
}

fn user_sid() -> Result<String> {
    let mut token = HANDLE::default();
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
    }
    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
    let token = Token(token);
    let mut bytes = 0;
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut bytes) };
    if bytes == 0 || bytes > 65536 {
        return Err(windows::core::Error::from_thread());
    }
    let mut buffer = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            bytes,
            &mut bytes,
        )?;
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut sid = windows::core::PWSTR::null();
    unsafe {
        ConvertSidToStringSidW(user.User.Sid, &mut sid)?;
    }
    let result = unsafe { sid.to_string() };
    unsafe {
        LocalFree(Some(HLOCAL(sid.0.cast())));
    }
    result.map_err(Into::into)
}

pub struct ShellRoot {
    id: HSTRING,
    root: PathBuf,
    context: Vec<u8>,
    registered: bool,
    published: HashMap<PathBuf, String>,
    // Destroy WinRT objects and registration before leaving this apartment.
    _apartment: Apartment,
}
impl ShellRoot {
    #[cfg(feature = "native-test-hooks")]
    pub fn retained_generated(root: &Path, context: &[u8]) -> Result<Self> {
        let apartment = Apartment::new()?;
        let folder = StorageFolder::GetFolderFromPathAsync(&storage_path(root))?.join()?;
        let info = StorageProviderSyncRootManager::GetSyncRootInformationForFolder(&folder)?;
        let id = info.Id()?;
        let prefix = format!("TwoDrive.Generated!{}!", user_sid()?);
        let mut actual = windows::core::Array::<u8>::new();
        CryptographicBuffer::CopyToByteArray(&info.Context()?, &mut actual)?;
        if !id.to_string().starts_with(&prefix) || &actual[..] != context {
            return Err(windows::core::Error::new(
                windows::core::HRESULT(0x8007000Du32 as i32),
                "retained root is not this user's generated fixture",
            ));
        }
        let result = Self {
            id,
            root: root.into(),
            context: context.to_vec(),
            registered: true,
            published: HashMap::new(),
            _apartment: apartment,
        };
        result.verify()?;
        Ok(result)
    }
    /// Only call after Session has atomically created and guarded a NEW root.
    pub fn register_generated(root: &Path, context: &[u8]) -> Result<Self> {
        let apartment = Apartment::new()?;
        let id = HSTRING::from(format!(
            "TwoDrive.Generated!{}!{:?}",
            user_sid()?,
            GUID::new()?
        ));
        let info = StorageProviderSyncRootInfo::new()?;
        info.SetId(&id)?;
        info.SetPath(&StorageFolder::GetFolderFromPathAsync(&storage_path(root))?.join()?)?;
        info.SetDisplayNameResource(&HSTRING::from("TwoDrive Development (generated files)"))?;
        info.SetIconResource(&HSTRING::from("%SystemRoot%\\system32\\shell32.dll,0"))?;
        info.SetHydrationPolicy(StorageProviderHydrationPolicy::Full)?;
        info.SetHydrationPolicyModifier(StorageProviderHydrationPolicyModifier::None)?;
        info.SetPopulationPolicy(StorageProviderPopulationPolicy::AlwaysFull)?;
        info.SetInSyncPolicy(StorageProviderInSyncPolicy(0x3ff))?;
        info.SetHardlinkPolicy(StorageProviderHardlinkPolicy::None)?;
        info.SetShowSiblingsAsGroup(false)?;
        info.SetVersion(&HSTRING::from(env!("CARGO_PKG_VERSION")))?;
        info.SetContext(&CryptographicBuffer::CreateFromByteArray(context)?)?;
        let definition = StorageProviderItemPropertyDefinition::new()?;
        definition.SetId(1)?;
        definition.SetDisplayNameResource(&HSTRING::from("TwoDrive synchronization"))?;
        info.StorageProviderItemPropertyDefinitions()?
            .Append(&definition)?;
        StorageProviderSyncRootManager::Register(&info)?;
        let mut result = Self {
            id,
            root: root.into(),
            context: context.to_vec(),
            registered: true,
            published: HashMap::new(),
            _apartment: apartment,
        };
        // Failure after registration must still undo this exact registration.
        if let Err(error) = result.verify() {
            let _ = result.finish();
            return Err(error);
        }
        notify(root, true);
        Ok(result)
    }
    pub fn verify(&self) -> Result<()> {
        let info = StorageProviderSyncRootManager::GetSyncRootInformationForId(&self.id)?;
        let folder = StorageFolder::GetFolderFromPathAsync(&storage_path(&self.root))?.join()?;
        let by_folder = StorageProviderSyncRootManager::GetSyncRootInformationForFolder(&folder)?;
        let expected = CryptographicBuffer::CreateFromByteArray(&self.context)?;
        if PathBuf::from(info.Path()?.Path()?.to_string())
            .canonicalize()
            .ok()
            .as_ref()
            != Some(&self.root)
            || by_folder.Id()? != self.id
            || !CryptographicBuffer::Compare(&info.Context()?, &expected)?
            || !CryptographicBuffer::Compare(&by_folder.Context()?, &expected)?
        {
            return Err(windows::core::Error::new(
                windows::core::HRESULT(0x8007000Du32 as i32),
                "generated Shell binding differs",
            ));
        }
        Ok(())
    }
    /// Writes metadata only. Availability/pin icons remain OS-derived. Unknown
    /// sync states are never translated into success, progress or a conflict.
    pub fn publish(&mut self, path: &Path, state: &str, directory: bool) -> Result<()> {
        use windows::core::Interface;
        if self.published.get(path).is_some_and(|old| old == state) {
            return Ok(());
        }
        let property = StorageProviderItemProperty::new()?;
        property.SetId(1)?;
        property.SetValue(&HSTRING::from(state))?;
        let values =
            windows_collections::IIterable::<StorageProviderItemProperty>::from(vec![Some(
                property,
            )]);
        let item: IStorageItem = if directory {
            storage_folder(&self.root, path)?.cast()?
        } else {
            storage_file(&self.root, path)?.cast()?
        };
        StorageProviderItemProperties::SetAsync(&item, &values)?.join()?;
        self.published.insert(path.into(), state.into());
        notify(path, false);
        notify(&self.root, true);
        Ok(())
    }
    pub fn finish(&mut self) -> Result<()> {
        if self.registered {
            // Never unregister a foreign replacement if our binding changed.
            self.verify()?;
            StorageProviderSyncRootManager::Unregister(&self.id)?;
            self.registered = false;
            notify(&self.root, true);
        }
        Ok(())
    }
}
fn storage_file(root: &Path, path: &Path) -> Result<StorageFile> {
    use std::os::windows::ffi::OsStrExt;
    let parent = path.parent().ok_or_else(|| {
        windows::core::Error::new(
            windows::core::HRESULT(0x80070057u32 as i32),
            "missing generated parent",
        )
    })?;
    if path.strip_prefix(root).ok().is_none_or(|p| {
        p.components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
            || p.components().count() > 2
    }) {
        return Err(windows::core::Error::new(
            windows::core::HRESULT(0x80070057u32 as i32),
            "metadata item must be a bounded generated-root leaf",
        ));
    }
    let name = path.file_name().ok_or_else(|| {
        windows::core::Error::new(
            windows::core::HRESULT(0x80070057u32 as i32),
            "missing generated leaf",
        )
    })?;
    let name = HSTRING::from_wide(&name.encode_wide().collect::<Vec<_>>());
    storage_folder(root, parent)?.GetFileAsync(&name)?.join()
}
fn storage_folder(root: &Path, path: &Path) -> Result<StorageFolder> {
    use std::os::windows::ffi::OsStrExt;
    let relative = path.strip_prefix(root).map_err(|_| {
        windows::core::Error::new(
            windows::core::HRESULT(0x80070057u32 as i32),
            "folder outside generated root",
        )
    })?;
    if relative.components().count() > 1
        || relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(windows::core::Error::new(
            windows::core::HRESULT(0x80070057u32 as i32),
            "unbounded generated directory",
        ));
    }
    let mut folder = StorageFolder::GetFolderFromPathAsync(&storage_path(root))?.join()?;
    for part in relative.components() {
        folder = folder
            .GetFolderAsync(&HSTRING::from_wide(
                &part.as_os_str().encode_wide().collect::<Vec<_>>(),
            ))?
            .join()?;
    }
    Ok(folder)
}

impl Drop for ShellRoot {
    fn drop(&mut self) {
        // Session owns the retention gate. A destructor must never bypass it:
        // Shell unregister can itself delete unhydrated placeholders.
        if self.registered {
            eprintln!("generated Shell registration retained; safe teardown unconfirmed");
        }
    }
}
fn storage_path(path: &Path) -> HSTRING {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    // The guarded fixture accepts local disks only. WinRT uses a regular DOS
    // path, while native file/CFAPI calls keep the canonical extended path.
    let start = if wide.starts_with(&[92, 92, 63, 92]) {
        4
    } else {
        0
    };
    HSTRING::from_wide(&wide[start..])
}
pub fn notify(path: &Path, directory: bool) {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        SHChangeNotify(
            if directory {
                SHCNE_UPDATEDIR
            } else {
                SHCNE_UPDATEITEM
            },
            SHCNF_PATHW,
            Some(wide.as_ptr().cast()),
            None,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires an explicit new generated root; never register automatically"]
    fn changed_context_is_not_owned_even_when_id_and_folder_match() {
        let root = PathBuf::from(
            std::env::var_os("TWODRIVE_APPROVED_SHELL_FIXTURE_ROOT")
                .expect("explicit new Shell root required"),
        );
        assert!(root.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .starts_with("twodrive-cfapi-fixture-")
        }));
        let mut session = crate::native::Session::create(&root).unwrap();
        session.attach_shell().unwrap();
        let folder = StorageFolder::GetFolderFromPathAsync(&storage_path(session.root()))
            .unwrap()
            .join()
            .unwrap();
        let info =
            StorageProviderSyncRootManager::GetSyncRootInformationForFolder(&folder).unwrap();
        let mut actual = windows::core::Array::<u8>::new();
        CryptographicBuffer::CopyToByteArray(&info.Context().unwrap(), &mut actual).unwrap();
        // A non-owning view queries our fresh fixture. It must never unregister
        // it or publish metadata, including while the assertion is failing.
        let mut view = ShellRoot {
            id: info.Id().unwrap(),
            root: session.root().into(),
            context: b"different-generated-owner".to_vec(),
            registered: false,
            published: HashMap::new(),
            _apartment: Apartment::new().unwrap(),
        };
        let mismatch_rejected = view.verify().is_err();
        view.context = actual.to_vec();
        let original_accepted = view.verify().is_ok();
        drop(view);
        drop(info);
        drop(folder);
        let canonical_root = session.root().to_owned();
        session.finish().unwrap();
        crate::native::verify_unregistered(&canonical_root).unwrap();
        assert!(
            original_accepted,
            "the original generated binding must match"
        );
        assert!(mismatch_rejected, "changed context must veto ownership");
    }
    #[test]
    fn metadata_lookup_accepts_chinese_file_beyond_max_path() {
        let _apartment = Apartment::new().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("generated-parent-".repeat(5));
        std::fs::create_dir(&root).unwrap();
        let path = root.join(format!("生成-{}.bin", "long-".repeat(35)));
        std::fs::write(&path, b"generated ordinary data").unwrap();
        assert!(path.as_os_str().len() > 260);
        let file = storage_file(&root, &path).unwrap();
        assert_eq!(
            file.Name().unwrap().to_string(),
            path.file_name().unwrap().to_str().unwrap()
        );
        let child = root.join("generated-目录");
        std::fs::create_dir(&child).unwrap();
        let nested = child.join(path.file_name().unwrap());
        std::fs::write(&nested, b"generated nested ordinary data").unwrap();
        assert_eq!(
            storage_file(&root, &nested)
                .unwrap()
                .Name()
                .unwrap()
                .to_string(),
            nested.file_name().unwrap().to_str().unwrap()
        );
        assert!(storage_file(&root, &root.join("../outside.bin")).is_err());
        assert!(storage_folder(&root, &child.join("unbounded")).is_err());
    }
}
