//! Local secrets: compatible 0600 JSON on Unix, per-user DPAPI on Windows.
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

pub fn read_secret(path: &Path) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 1024 * 1024, "secret file too large");
    #[cfg(windows)]
    {
        crypt(&bytes, false)
    }
    #[cfg(not(windows))]
    {
        Ok(bytes)
    }
}

pub fn write_secret(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    #[cfg(windows)]
    let protected = crypt(bytes, true)?;
    #[cfg(windows)]
    let bytes = protected.as_slice();
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(windows)]
fn crypt(bytes: &[u8], protect: bool) -> anyhow::Result<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len().try_into()?,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // DPAPI allocates output; no UI and no machine-wide flag: current user only.
    unsafe {
        let ok = if protect {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        anyhow::ensure!(ok != 0, "Windows DPAPI operation failed");
        let result = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as *mut _);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn secret_roundtrip_and_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        super::write_secret(&path, b"synthetic one").unwrap();
        super::write_secret(&path, b"synthetic two").unwrap();
        assert_eq!(super::read_secret(&path).unwrap(), b"synthetic two");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        #[cfg(windows)]
        assert_ne!(std::fs::read(path).unwrap(), b"synthetic two");
    }
}
