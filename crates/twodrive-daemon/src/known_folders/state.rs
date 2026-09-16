use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
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
    pub(super) fn load(data_dir: &Path) -> anyhow::Result<Self> {
        let path = data_dir.join("known-folders-state.json");
        match fs::read_to_string(&path) {
            Ok(data) => Ok(serde_json::from_str(&data)?),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err.into()),
        }
    }

    pub(super) fn save(&self, data_dir: &Path) -> anyhow::Result<()> {
        fs::create_dir_all(data_dir)?;
        let path = data_dir.join("known-folders-state.json");
        let tmp_path = data_dir.join("known-folders-state.json.tmp");
        let mut file = fs::File::create(&tmp_path)?;
        use std::io::Write;
        file.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(tmp_path, path)?;
        fs::File::open(data_dir)?.sync_all()?;
        Ok(())
    }

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
}

impl FileSnapshot {
    pub(super) fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            size: metadata.len(),
            modified_unix: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs() as i64)
                .unwrap_or_else(now_unix),
        }
    }
}
