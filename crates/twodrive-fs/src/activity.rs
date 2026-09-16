use std::fs::{self};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};
use twodrive_core::now_unix;

use crate::cache_io::unique_suffix;

#[derive(Debug)]
pub(crate) struct ActivityGuard {
    pub(crate) path: PathBuf,
    pub(crate) id: String,
    pub(crate) last_bytes_done: u64,
    pub(crate) last_update: SystemTime,
    pub(crate) finished: bool,
}

impl ActivityGuard {
    pub(crate) fn start(
        cache_dir: &Path,
        kind: &str,
        cloud_path: &str,
        name: &str,
        bytes_total: Option<u64>,
    ) -> Self {
        let path = activity_file_path(cache_dir);
        let id = format!("{kind}-{}", unique_suffix());
        let mut guard = Self {
            path,
            id,
            last_bytes_done: 0,
            last_update: SystemTime::now(),
            finished: false,
        };
        let _ = guard.write_entry(kind, cloud_path, name, 0, bytes_total);
        guard
    }

    pub(crate) fn set_progress(&mut self, bytes_done: u64, bytes_total: Option<u64>) {
        let now_time = SystemTime::now();
        let enough_bytes = bytes_done >= self.last_bytes_done.saturating_add(1024 * 1024);
        let enough_time = now_time
            .duration_since(self.last_update)
            .unwrap_or_default()
            >= Duration::from_millis(750);
        let complete = bytes_total.is_some_and(|total| bytes_done >= total);
        if !enough_bytes && !enough_time && !complete {
            return;
        }
        self.last_bytes_done = bytes_done;
        self.last_update = now_time;
        let _ = update_activity_file(&self.path, |active| {
            let now = now_unix();
            if let Some(item) = active
                .iter_mut()
                .find(|item| item.get("id").and_then(serde_json::Value::as_str) == Some(&self.id))
            {
                item["bytes_done"] = serde_json::json!(bytes_done);
                if let Some(total) = bytes_total {
                    item["bytes_total"] = serde_json::json!(total);
                }
                item["updated_unix"] = serde_json::json!(now);
            }
        });
    }

    pub(crate) fn write_entry(
        &mut self,
        kind: &str,
        cloud_path: &str,
        name: &str,
        bytes_done: u64,
        bytes_total: Option<u64>,
    ) -> anyhow::Result<()> {
        let id = self.id.clone();
        let item = serde_json::json!({
            "id": id,
            "kind": kind,
            "path": cloud_path,
            "name": name,
            "bytes_done": bytes_done,
            "bytes_total": bytes_total,
            "started_unix": now_unix(),
            "updated_unix": now_unix(),
        });
        update_activity_file(&self.path, |active| {
            active.retain(|entry| {
                entry.get("id").and_then(serde_json::Value::as_str) != Some(&self.id)
            });
            active.push(item);
        })
    }

    pub(crate) fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let _ = update_activity_file(&self.path, |active| {
            active.retain(|entry| {
                entry.get("id").and_then(serde_json::Value::as_str) != Some(&self.id)
            });
        });
    }
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        self.finish();
    }
}

pub(crate) fn activity_file_path(cache_dir: &Path) -> PathBuf {
    cache_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cache_dir.to_path_buf())
        .join("activity.json")
}

pub(crate) fn activity_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub(crate) fn update_activity_file(
    path: &Path,
    update: impl FnOnce(&mut Vec<serde_json::Value>),
) -> anyhow::Result<()> {
    let _guard = activity_lock()
        .lock()
        .map_err(|_| anyhow::anyhow!("activity lock is poisoned"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let now = now_unix();
    let existing = fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .unwrap_or_else(|| serde_json::json!({"updated_unix": now, "active": []}));
    let mut active = existing
        .get("active")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();

    active.retain(|entry| {
        entry
            .get("updated_unix")
            .and_then(serde_json::Value::as_i64)
            .map(|updated| now.saturating_sub(updated) < 6 * 60 * 60)
            .unwrap_or(false)
    });
    update(&mut active);

    let snapshot = serde_json::json!({
        "updated_unix": now,
        "active": active,
    });
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, serde_json::to_vec_pretty(&snapshot)?)?;
    fs::rename(tmp_path, path)?;
    Ok(())
}

pub(crate) fn clear_activity_file(cache_dir: &Path) -> anyhow::Result<()> {
    let path = activity_file_path(cache_dir);
    update_activity_file(&path, |active| active.clear())
}
