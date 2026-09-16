mod onedrive;
pub use onedrive::{DEFAULT_GRAPH_CLIENT_ID, GraphConfig};

use crate::{AppPaths, parse_duration_seconds};
use serde::{Deserialize, Serialize};
use std::fs;
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub graph: GraphConfig,
    pub cache: CacheConfig,
    pub power: PowerConfig,
    pub known_folders: KnownFoldersConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheConfig {
    pub retain_for: String,
    pub max_size: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PowerConfig {
    pub ac_sync_interval: String,
    pub battery_sync_interval: String,
    pub ac_download_concurrency: u8,
    pub battery_download_concurrency: u8,
    pub ac_upload_concurrency: u8,
    pub battery_upload_concurrency: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KnownFoldersConfig {
    pub enabled: bool,
    pub mode: String,
    pub debounce: String,
    pub rescan_interval: String,
    pub startup_scan: bool,
    pub upload_deletes: bool,
    pub exclude_suffixes: Vec<String>,
    pub folders: Vec<KnownFolderConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KnownFolderConfig {
    pub local: String,
    pub remote: String,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            retain_for: "30d".to_string(),
            max_size: "20GiB".to_string(),
        }
    }
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self {
            ac_sync_interval: "15m".to_string(),
            battery_sync_interval: "60m".to_string(),
            ac_download_concurrency: 4,
            battery_download_concurrency: 1,
            ac_upload_concurrency: 4,
            battery_upload_concurrency: 2,
        }
    }
}

impl Default for KnownFoldersConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: "upload_only".to_string(),
            debounce: "5s".to_string(),
            rescan_interval: "15m".to_string(),
            startup_scan: true,
            upload_deletes: false,
            exclude_suffixes: vec![
                ".crdownload".to_string(),
                ".part".to_string(),
                ".tmp".to_string(),
                ".download".to_string(),
                ".temp".to_string(),
                ".partial".to_string(),
                ".filepart".to_string(),
                ".opdownload".to_string(),
                ".aria2".to_string(),
                ".!qB".to_string(),
                ".swp".to_string(),
                ".swo".to_string(),
                ".swx".to_string(),
                ".bak".to_string(),
            ],
            folders: vec![
                KnownFolderConfig {
                    local: "~/Pictures".to_string(),
                    remote: "/Pictures".to_string(),
                },
                KnownFolderConfig {
                    local: "~/Downloads".to_string(),
                    remote: "/Downloads".to_string(),
                },
            ],
        }
    }
}

impl Default for KnownFolderConfig {
    fn default() -> Self {
        Self {
            local: String::new(),
            remote: "/".to_string(),
        }
    }
}

impl Config {
    pub fn load_or_create(paths: &AppPaths) -> anyhow::Result<Self> {
        paths.ensure()?;
        if !paths.config_path.exists() {
            let config = Self::default();
            config.save(paths)?;
            return Ok(config);
        }

        let data = fs::read_to_string(&paths.config_path)?;
        Ok(toml::from_str(&data)?)
    }

    pub fn save(&self, paths: &AppPaths) -> anyhow::Result<()> {
        fs::create_dir_all(&paths.config_dir)?;
        fs::write(&paths.config_path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn cache_retain_seconds(&self) -> anyhow::Result<i64> {
        parse_duration_seconds(&self.cache.retain_for)
    }

    pub fn known_folder_debounce_seconds(&self) -> anyhow::Result<u64> {
        parse_duration_seconds(&self.known_folders.debounce).map(|value| value.max(1) as u64)
    }

    pub fn known_folder_rescan_seconds(&self) -> anyhow::Result<u64> {
        parse_duration_seconds(&self.known_folders.rescan_interval)
            .map(|value| if value <= 0 { 0 } else { value.max(30) as u64 })
    }
}
