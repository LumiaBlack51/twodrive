use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use twodrive_core::now_unix;

#[derive(Debug, Clone)]
pub(super) struct KnownFolderRoot {
    pub(super) local: PathBuf,
    pub(super) remote: String,
}

#[derive(Debug, Clone)]
pub(super) struct KnownFolderUploadJob {
    pub(super) local_path: PathBuf,
    pub(super) remote_path: String,
    pub(super) snapshot: FileSnapshot,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct KnownFolderState {
    #[serde(skip)]
    pub(super) rate_deferred: bool,
    #[serde(skip)]
    pub(super) rate: std::sync::Arc<std::sync::Mutex<super::rate::UploadRate>>,
    #[serde(skip)]
    pub(super) storage: Option<rusqlite::Connection>,
    #[serde(default)]
    pub(super) baseline_initialized: bool,
    #[serde(default)]
    pub(super) files: HashMap<String, FileSnapshot>,
    #[serde(default)]
    pub(super) pending: HashMap<String, PendingKnownFolderUpload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PendingKnownFolderUpload {
    pub(super) remote_path: String,
    pub(super) snapshot: FileSnapshot,
}

impl KnownFolderState {
    pub(super) fn remove_path(&mut self, path: &Path) {
        let key = path.to_string_lossy();
        self.files.retain(|local_path, _| {
            local_path != key.as_ref() && !local_path.starts_with(&format!("{key}/"))
        });
        self.pending.retain(|local_path, _| {
            local_path != key.as_ref() && !local_path.starts_with(&format!("{key}/"))
        });
    }

    pub(super) fn prune_missing_pending(&mut self) -> usize {
        let missing = self
            .pending
            .keys()
            .filter(|local_path| !Path::new(local_path).is_file())
            .cloned()
            .collect::<Vec<_>>();
        for local_path in &missing {
            eprintln!(
                "twodrive: dropping queued known folder upload because the local file is gone: {local_path}"
            );
            self.pending.remove(local_path);
            self.files.remove(local_path);
        }
        missing.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct FileSnapshot {
    pub(super) size: u64,
    pub(super) modified_unix: i64,
    #[serde(default)]
    pub(super) modified_nanos: u32,
}

impl FileSnapshot {
    pub(super) fn from_metadata(metadata: &fs::Metadata) -> Self {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok());
        Self {
            size: metadata.len(),
            modified_unix: modified
                .map(|duration| duration.as_secs() as i64)
                .unwrap_or_else(now_unix),
            modified_nanos: modified
                .map(|duration| duration.subsec_nanos())
                .unwrap_or(0),
        }
    }
}
