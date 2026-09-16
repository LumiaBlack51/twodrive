use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use twodrive_core::now_unix;

#[derive(Debug)]
pub(super) struct ActivityEntry {
    pub(super) path: PathBuf,
    pub(super) id: String,
    pub(super) finished: bool,
}

impl ActivityEntry {
    pub(super) fn start(
        data_dir: &Path,
        kind: &str,
        cloud_path: &str,
        name: &str,
        bytes_total: Option<u64>,
    ) -> Self {
        let path = data_dir.join("activity.json");
        let id = format!("known-{kind}-{}", unique_suffix());
        let entry = Self {
            path,
            id,
            finished: false,
        };
        let item = serde_json::json!({
            "id": entry.id,
            "kind": kind,
            "path": cloud_path,
            "name": name,
            "bytes_done": 0,
            "bytes_total": bytes_total,
            "started_unix": now_unix(),
            "updated_unix": now_unix(),
        });
        let _ = update_activity(&entry.path, |active| active.push(item));
        entry
    }

    pub(super) fn set_progress(&mut self, bytes_done: u64, bytes_total: Option<u64>) {
        let id = self.id.clone();
        let _ = update_activity(&self.path, |active| {
            if let Some(item) = active
                .iter_mut()
                .find(|item| item.get("id").and_then(serde_json::Value::as_str) == Some(&id))
            {
                item["bytes_done"] = serde_json::json!(bytes_done);
                if let Some(total) = bytes_total {
                    item["bytes_total"] = serde_json::json!(total);
                }
                item["updated_unix"] = serde_json::json!(now_unix());
            }
        });
    }

    pub(super) fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let id = self.id.clone();
        let _ = update_activity(&self.path, |active| {
            active.retain(|item| item.get("id").and_then(serde_json::Value::as_str) != Some(&id));
        });
    }
}

impl Drop for ActivityEntry {
    fn drop(&mut self) {
        self.finish();
    }
}

pub(super) fn update_activity<F>(path: &Path, update: F) -> anyhow::Result<()>
where
    F: FnOnce(&mut Vec<serde_json::Value>),
{
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| anyhow::anyhow!("known-folder activity lock is poisoned"))?;
    let mut active = match fs::read_to_string(path) {
        Ok(data) => serde_json::from_str::<serde_json::Value>(&data)
            .ok()
            .and_then(|value| {
                value
                    .get("active")
                    .and_then(|active| active.as_array())
                    .cloned()
            })
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    update(&mut active);
    let snapshot = serde_json::json!({
        "active": active,
        "updated_unix": now_unix(),
    });
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension(format!("json.{}.tmp", unique_suffix()));
    fs::write(&tmp_path, serde_json::to_string_pretty(&snapshot)?)?;
    fs::rename(tmp_path, path)?;
    Ok(())
}

pub(super) fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos}-{}", std::process::id())
}
