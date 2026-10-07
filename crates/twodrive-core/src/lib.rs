//! Domain state and compatible local persistence for TwoDrive.
mod config;
mod credentials;
mod database;
mod domain;
mod known_folders;
mod paths;
mod time;

pub use config::{
    CacheConfig, Config, DEFAULT_GRAPH_CLIENT_ID, GraphConfig, KnownFolderConfig,
    KnownFoldersConfig, PowerConfig,
};
pub use credentials::{TokenData, TokenStore};
pub use database::Database;
pub use database::cloud::{CloudIdentity, DownloadTask, IndexedItem};
pub use domain::{
    CoreError, FileRecord, FileState, MetadataEntry, PendingDelete, PendingMetadataKind,
    PendingMetadataOperation,
};
pub use known_folders::{
    KnownFolderDiagnostic, KnownFolderSourceState, expand_known_folder_home,
    inspect_known_folder_source, known_folder_diagnostics,
};
pub use paths::{AppPaths, join_cloud_path, normalize_cloud_path};
pub use time::{now_unix, parse_duration_seconds};

#[cfg(test)]
mod tests;

pub mod private_file;
