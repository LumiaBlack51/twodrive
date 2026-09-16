use crate::paths::{cloud_name, normalize_cloud_path, parent_cloud_path};
use std::{fmt, path::PathBuf, str::FromStr};
use thiserror::Error;
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("unknown file state: {0}")]
    UnknownState(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    OnlineOnly,
    Hydrating,
    Cached,
    Pinned,
    Writing,
    Dirty,
    Uploading,
    Conflict,
    Error,
}

impl FileState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OnlineOnly => "online_only",
            Self::Hydrating => "hydrating",
            Self::Cached => "cached",
            Self::Pinned => "pinned",
            Self::Writing => "writing",
            Self::Dirty => "dirty",
            Self::Uploading => "uploading",
            Self::Conflict => "conflict",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for FileState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FileState {
    type Err = CoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "online_only" => Ok(Self::OnlineOnly),
            "hydrating" => Ok(Self::Hydrating),
            "cached" => Ok(Self::Cached),
            "pinned" => Ok(Self::Pinned),
            "writing" => Ok(Self::Writing),
            "dirty" => Ok(Self::Dirty),
            "uploading" => Ok(Self::Uploading),
            "conflict" => Ok(Self::Conflict),
            "error" => Ok(Self::Error),
            other => Err(CoreError::UnknownState(other.to_string())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MetadataEntry {
    pub remote_id: String,
    pub path: String,
    pub parent_path: String,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified_unix: i64,
    pub etag: String,
}

impl MetadataEntry {
    pub fn new_file(
        remote_id: impl Into<String>,
        path: impl Into<String>,
        size: u64,
        modified_unix: i64,
        etag: impl Into<String>,
    ) -> Self {
        Self::new(remote_id, path, false, size, modified_unix, etag)
    }

    pub fn new_dir(
        remote_id: impl Into<String>,
        path: impl Into<String>,
        modified_unix: i64,
        etag: impl Into<String>,
    ) -> Self {
        Self::new(remote_id, path, true, 0, modified_unix, etag)
    }

    fn new(
        remote_id: impl Into<String>,
        path: impl Into<String>,
        is_dir: bool,
        size: u64,
        modified_unix: i64,
        etag: impl Into<String>,
    ) -> Self {
        let path = normalize_cloud_path(&path.into());
        let parent_path = parent_cloud_path(&path);
        let name = cloud_name(&path);

        Self {
            remote_id: remote_id.into(),
            path,
            parent_path,
            name,
            is_dir,
            size,
            modified_unix,
            etag: etag.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileRecord {
    pub metadata: MetadataEntry,
    /// Stable remote identity assigned by OneDrive. `metadata.remote_id` is the
    /// local identity used by the cache/database and never changes after create.
    pub cloud_remote_id: Option<String>,
    /// Local rwx permissions, never imported from or exported to the cloud.
    pub local_mode: Option<u16>,
    pub state: FileState,
    pub cache_path: Option<PathBuf>,
    pub cache_accessed_unix: Option<i64>,
    pub pin_explicit: bool,
    pub pin_origin_remote_id: Option<String>,
    pub pin_inheritance_blocked: bool,
}

#[derive(Debug, Clone)]
pub struct PendingDelete {
    pub remote_id: String,
    pub path: String,
    pub cache_path: Option<PathBuf>,
    pub queued_unix: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingMetadataKind {
    CreateFolder,
    Move,
}

impl PendingMetadataKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CreateFolder => "create_folder",
            Self::Move => "move",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PendingMetadataOperation {
    pub local_id: String,
    pub kind: PendingMetadataKind,
    pub path: String,
    pub queued_unix: i64,
}

impl FileRecord {
    pub fn effective_pinned(&self) -> bool {
        self.pin_explicit || self.pin_origin_remote_id.is_some()
    }
}
