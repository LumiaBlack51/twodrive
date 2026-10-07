//! Read-only diagnostics for explicit upload sources. Never infer a replacement.
use crate::{AppPaths, Config, normalize_cloud_path};
use serde::Serialize;
use std::{
    env, fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KnownFolderSourceState {
    Ready,
    Missing,
    BrokenSymlink,
    Symlink,
    NotDirectory,
    Unsafe,
    Unavailable,
}

impl KnownFolderSourceState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Missing => "missing",
            Self::BrokenSymlink => "broken_symlink",
            Self::Symlink => "symlink",
            Self::NotDirectory => "not_directory",
            Self::Unsafe => "unsafe",
            Self::Unavailable => "unavailable",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Ready => "Source directory is available.",
            Self::Missing => {
                "Source directory does not exist. Choose its current location in TwoDrive Settings."
            }
            Self::BrokenSymlink => {
                "Source is a broken symbolic link. Choose a local directory in TwoDrive Settings."
            }
            Self::Symlink => {
                "Symbolic links are skipped by uploads. Choose a local directory in TwoDrive Settings."
            }
            Self::NotDirectory => {
                "Source is not a directory. Choose a local directory in TwoDrive Settings."
            }
            Self::Unsafe => {
                "Choose an absolute local directory outside the TwoDrive mount, rather than the home directory or filesystem root."
            }
            Self::Unavailable => {
                "Source cannot be read. Check access permissions or choose another directory in TwoDrive Settings."
            }
        }
    }
}

#[derive(Debug, Serialize)]
pub struct KnownFolderDiagnostic {
    pub index: usize,
    pub configured_local: String,
    pub local: PathBuf,
    pub remote: String,
    pub state: KnownFolderSourceState,
    pub message: &'static str,
    pub system_directory: Option<PathBuf>,
    pub warnings: Vec<String>,
}

pub fn expand_known_folder_home(value: &str) -> anyhow::Result<PathBuf> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
    Ok(expand_home(value, &home))
}

fn expand_home(value: &str, home: &Path) -> PathBuf {
    if value == "~" {
        home.to_path_buf()
    } else if let Some(relative) = value.strip_prefix("~/") {
        home.join(relative)
    } else {
        PathBuf::from(value)
    }
}

pub fn inspect_known_folder_source(
    local: &Path,
    home: &Path,
    mount: &Path,
) -> KnownFolderSourceState {
    use KnownFolderSourceState::*;
    // Check lexical mount paths before touching FUSE or resolving symlinks.
    if !local.is_absolute()
        || local == Path::new("/")
        || local == home
        || local.starts_with(mount)
        || mount.starts_with(local)
    {
        return Unsafe;
    }
    let metadata = match fs::symlink_metadata(local) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == ErrorKind::NotFound => return Missing,
        Err(_) => return Unavailable,
    };
    if metadata.file_type().is_symlink() {
        return match fs::metadata(local) {
            Ok(_) => Symlink,
            Err(err) if err.kind() == ErrorKind::NotFound => BrokenSymlink,
            Err(_) => Unavailable,
        };
    }
    if !metadata.is_dir() {
        return NotDirectory;
    }
    let Ok(resolved) = fs::canonicalize(local) else {
        return Unavailable;
    };
    let home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let mount = fs::canonicalize(mount).unwrap_or_else(|_| mount.to_path_buf());
    if resolved == Path::new("/")
        || resolved == home
        || resolved.starts_with(&mount)
        || mount.starts_with(&resolved)
    {
        return Unsafe;
    }
    if fs::read_dir(local).is_err() {
        return Unavailable;
    }
    Ready
}

pub fn known_folder_diagnostics(
    config: &Config,
    paths: &AppPaths,
) -> anyhow::Result<Vec<KnownFolderDiagnostic>> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
    // user-dirs.dirs belongs to XDG_CONFIG_HOME, not its twodrive subdirectory.
    let user_dirs = paths
        .config_dir
        .parent()
        .map(|root| root.join("user-dirs.dirs"));
    let user_dirs = user_dirs
        .and_then(|path| fs::read_to_string(path).ok())
        .unwrap_or_default();
    Ok(config.known_folders.folders.iter().enumerate().map(|(index, folder)| {
        let local = expand_home(&folder.local, &home);
        let state = inspect_known_folder_source(&local, &home, &paths.mount_dir);
        let remote = normalize_cloud_path(&folder.remote);
        let system_directory = system_directory_key(&remote)
            .and_then(|key| parse_system_directory(&user_dirs, key, &home));
        let mut warnings = Vec::new();
        if folder.remote.trim().is_empty() {
            warnings.push("Cloud destination is empty; this mapping is skipped.".to_string());
        }
        if let Some(system) = &system_directory {
            if system == &home {
                warnings.push("The system special directory points to your home directory. Repair its XDG setting separately; TwoDrive keeps the explicit source.".to_string());
            } else if system != &local {
                warnings.push("The system special directory differs from this upload source. If you renamed the source, choose its current location; custom mappings may be intentional.".to_string());
            }
        }
        KnownFolderDiagnostic { index, configured_local: folder.local.clone(), local, remote,
            state, message: state.message(), system_directory, warnings }
    }).collect())
}

fn system_directory_key(remote: &str) -> Option<&'static str> {
    match remote {
        "/Pictures" => Some("XDG_PICTURES_DIR"),
        "/Downloads" => Some("XDG_DOWNLOAD_DIR"),
        "/Documents" => Some("XDG_DOCUMENTS_DIR"),
        "/Videos" => Some("XDG_VIDEOS_DIR"),
        "/Templates" => Some("XDG_TEMPLATES_DIR"),
        "/Music" => Some("XDG_MUSIC_DIR"),
        "/Desktop" => Some("XDG_DESKTOP_DIR"),
        _ => None,
    }
}

// Parse the documented quoted path subset without executing shell expressions.
fn parse_system_directory(data: &str, key: &str, home: &Path) -> Option<PathBuf> {
    let value = data
        .lines()
        .filter_map(|line| line.trim().split_once('='))
        .rfind(|(name, _)| name.trim() == key)?
        .1
        .trim();
    let value = value.strip_prefix('"')?.strip_suffix('"')?;
    let mut decoded = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let next = chars.next()?;
            if !matches!(next, '\\' | '"' | '$' | '`') {
                return None;
            }
            decoded.push(next);
        } else {
            decoded.push(ch);
        }
    }
    if decoded == "$HOME" || decoded.starts_with("$HOME/") {
        let relative = decoded
            .strip_prefix("$HOME")
            .unwrap()
            .trim_start_matches('/');
        if relative.contains(['$', '`']) {
            return None;
        }
        return Some(home.join(relative));
    }
    if decoded.contains(['$', '`']) {
        return None;
    }
    let path = PathBuf::from(decoded);
    path.is_absolute().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    // This fixture checks Unix/XDG roots and links; Windows cannot use its APIs.
    #[cfg(unix)]
    #[test]
    fn sources_distinguish_missing_links_files_and_unsafe_fallbacks() {
        let root = env::temp_dir().join(format!(
            "twodrive-source-check-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = root.join("home");
        let mount = home.join("TwoDrive/OneDrive");
        let pictures = home.join("图片");
        fs::create_dir_all(&pictures).unwrap();
        fs::create_dir_all(&mount).unwrap();
        symlink(home.join("old-drive/Image"), home.join("Pictures")).unwrap();
        symlink(&pictures, home.join("linked-pictures")).unwrap();
        symlink(&home, root.join("home-alias")).unwrap();
        fs::write(home.join("file"), b"contents").unwrap();
        let inspect = |path: &Path| inspect_known_folder_source(path, &home, &mount);
        assert_eq!(inspect(&pictures), KnownFolderSourceState::Ready);
        assert_eq!(inspect(&home.join("下载")), KnownFolderSourceState::Missing);
        assert_eq!(
            inspect(&home.join("Pictures")),
            KnownFolderSourceState::BrokenSymlink
        );
        assert_eq!(
            inspect(&home.join("linked-pictures")),
            KnownFolderSourceState::Symlink
        );
        assert_eq!(
            inspect(&home.join("file")),
            KnownFolderSourceState::NotDirectory
        );
        for path in [
            home.clone(),
            home.join("."),
            pictures.join(".."),
            PathBuf::from("/"),
            mount.join("Pictures"),
            mount.parent().unwrap().to_path_buf(),
            root.join("home-alias/TwoDrive/OneDrive"),
            PathBuf::from("Pictures"),
        ] {
            assert_eq!(
                inspect(&path),
                KnownFolderSourceState::Unsafe,
                "{}",
                path.display()
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn xdg_paths_are_read_without_running_shell_or_guessing_new_sources() {
        let home = Path::new("/test/home");
        let data = "XDG_PICTURES_DIR=\"$HOME/\"\nXDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\n";
        assert_eq!(
            parse_system_directory(data, "XDG_PICTURES_DIR", home),
            Some(home.to_path_buf())
        );
        assert_eq!(
            parse_system_directory(data, "XDG_DOWNLOAD_DIR", home),
            Some(home.join("Downloads"))
        );
        assert_eq!(expand_home("~/下载", home), home.join("下载"));
        for value in [
            "$(touch /tmp/never)",
            "`id`",
            "$OTHER/Pictures",
            "relative/path",
        ] {
            assert!(
                parse_system_directory(
                    &format!("XDG_PICTURES_DIR=\"{value}\""),
                    "XDG_PICTURES_DIR",
                    home
                )
                .is_none()
            );
        }
    }
}
