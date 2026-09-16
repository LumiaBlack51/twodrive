use std::{
    env, fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub db_path: PathBuf,
    pub mount_dir: PathBuf,
    pub token_path: PathBuf,
}

impl AppPaths {
    pub fn discover() -> anyhow::Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;

        let config_dir = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("twodrive");
        let data_dir = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("twodrive");
        let cache_dir = data_dir.join("cache");
        let db_path = data_dir.join("twodrive.sqlite3");
        let mount_dir = env::var_os("TWODRIVE_MOUNT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("TwoDrive/OneDrive"));
        let config_path = config_dir.join("config.toml");
        let token_path = config_dir.join("tokens.json");

        Ok(Self {
            config_dir,
            config_path,
            data_dir,
            cache_dir,
            db_path,
            mount_dir,
            token_path,
        })
    }

    pub fn ensure(&self) -> anyhow::Result<()> {
        ensure_dir(&self.config_dir)?;
        ensure_dir(&self.data_dir)?;
        ensure_dir(&self.cache_dir)?;
        ensure_dir(&self.mount_dir)?;
        Ok(())
    }
}

pub fn normalize_cloud_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return "/".to_string();
    }

    let mut normalized = String::from("/");
    normalized.push_str(trimmed.trim_matches('/'));
    normalized
}

pub fn join_cloud_path(parent: &str, name: &str) -> String {
    let parent = normalize_cloud_path(parent);
    if parent == "/" {
        normalize_cloud_path(name)
    } else {
        normalize_cloud_path(&format!("{parent}/{name}"))
    }
}

pub(crate) fn parent_cloud_path(path: &str) -> String {
    let path = normalize_cloud_path(path);
    if path == "/" {
        return "/".to_string();
    }

    match path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(index) => path[..index].to_string(),
    }
}

pub(crate) fn cloud_name(path: &str) -> String {
    let path = normalize_cloud_path(path);
    if path == "/" {
        return String::new();
    }

    path.rsplit('/').next().unwrap_or_default().to_string()
}

fn ensure_dir(path: &Path) -> anyhow::Result<()> {
    match fs::create_dir_all(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err.into()),
    }
}
