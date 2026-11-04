//! Durable local acknowledgement for small Office advisory locks. The cloud
//! mutation still uses the normal version/recovery path on a background worker.
use crate::{
    model::{CloudBackend, MetadataEntry},
    mount_write::Writer,
    state::State,
};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::Path,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

pub(crate) const LIMIT: u64 = 16 * 1024;
pub(crate) type Deleted = Arc<dyn Fn(&str, &str, &str) -> anyhow::Result<()> + Send + Sync>;
#[derive(Clone, Serialize, Deserialize)]
struct RemoteVersion {
    id: String,
    etag: String,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
enum Phase {
    Pending,
    Uploading,
    Uploaded,
    Deleting,
    Failed,
}
#[derive(Clone, Serialize, Deserialize)]
struct Job {
    id: String,
    path: String,
    size: u64,
    modified: i64,
    visible: bool,
    phase: Phase,
    remote: Option<RemoteVersion>,
}
impl Job {
    fn metadata(&self) -> MetadataEntry {
        MetadataEntry::new_file(
            format!("local-office-lock:{}", self.id),
            &self.path,
            self.size,
            self.modified,
            &self.id,
        )
    }
}
pub(crate) struct EditorLocks {
    state: State,
    jobs: Mutex<Vec<Job>>,
}
pub(crate) fn matches(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or_default();
    name.starts_with(".~lock.") && name.ends_with('#') && name.len() > 8
}
impl EditorLocks {
    pub(crate) fn new(directory: &Path) -> anyhow::Result<Arc<Self>> {
        let state = State::new(directory.to_owned())?;
        let mut jobs: Vec<Job> = if state.dir.join("queue.json").exists() {
            ensure!(
                std::fs::metadata(state.dir.join("queue.json"))?.len() <= 2 * 1024 * 1024,
                "临时锁队列过大"
            );
            state.read("queue.json")?
        } else {
            Vec::new()
        };
        ensure!(jobs.len() <= 128, "临时锁队列过大");
        let mut ids = HashSet::new();
        for job in &mut jobs {
            crate::provider::valid_path(&job.path)?;
            ensure!(
                matches(&job.path)
                    && job.size <= LIMIT
                    && !job.id.is_empty()
                    && job
                        .id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                    && ids.insert(job.id.clone()),
                "临时锁记录无效"
            );
            // A lost cloud reply is not permission to repeat a mutation. Keep
            // the content and report recovery instead of overwriting a lock.
            if matches!(job.phase, Phase::Uploading | Phase::Deleting) {
                job.phase = Phase::Failed;
            }
        }
        state.save("queue.json", &jobs)?;
        Ok(Arc::new(Self {
            state,
            jobs: Mutex::new(jobs),
        }))
    }
    pub(crate) fn put(
        &self,
        writer: &Writer,
        id: &str,
        path: &str,
        content: &Path,
    ) -> anyhow::Result<MetadataEntry> {
        let bytes = std::fs::read(content)?;
        ensure!(bytes.len() as u64 <= LIMIT, "Office临时锁过大");
        let needs_probe = !self
            .jobs
            .lock()
            .map_err(|_| anyhow::anyhow!("临时锁队列不可用"))?
            .iter()
            .any(|j| j.path == path);
        let remote_exists = needs_probe && writer.backend.get_metadata_by_path(path)?.is_some();
        let mut jobs = self
            .jobs
            .lock()
            .map_err(|_| anyhow::anyhow!("临时锁队列不可用"))?;
        ensure!(jobs.len() < 128, "临时锁队列已满，请先处理恢复内容");
        ensure!(
            !remote_exists || jobs.iter().any(|j| j.path == path),
            "云端已有锁文件，未覆盖"
        );
        // Content is private and synced before publishing the local namespace.
        self.state.write_bytes(&format!("{id}.data"), &bytes)?;
        let mut next = jobs.clone();
        for old in next.iter_mut().filter(|j| j.path == path) {
            old.visible = false;
        }
        let job = Job {
            id: id.into(),
            path: path.into(),
            size: bytes.len() as u64,
            modified: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs() as i64,
            visible: true,
            phase: Phase::Pending,
            remote: None,
        };
        let metadata = job.metadata();
        next.push(job);
        self.state.save("queue.json", &next)?;
        *jobs = next;
        Ok(metadata)
    }
    pub(crate) fn file(&self, path: &str) -> anyhow::Result<Option<(MetadataEntry, Vec<u8>)>> {
        let jobs = self
            .jobs
            .lock()
            .map_err(|_| anyhow::anyhow!("临时锁队列不可用"))?;
        let Some(job) = jobs.iter().find(|j| j.path == path && j.visible) else {
            return Ok(None);
        };
        let data = std::fs::read(self.state.dir.join(format!("{}.data", job.id)))?;
        ensure!(data.len() as u64 == job.size, "临时锁内容不完整");
        Ok(Some((job.metadata(), data)))
    }
    pub(crate) fn entries(&self, parent: &str) -> anyhow::Result<Vec<MetadataEntry>> {
        let jobs = self
            .jobs
            .lock()
            .map_err(|_| anyhow::anyhow!("临时锁队列不可用"))?;
        Ok(jobs
            .iter()
            .filter(|j| j.visible)
            .map(Job::metadata)
            .filter(|m| m.parent_path == parent)
            .collect())
    }
    pub(crate) fn hidden(&self) -> anyhow::Result<HashSet<String>> {
        let jobs = self
            .jobs
            .lock()
            .map_err(|_| anyhow::anyhow!("临时锁队列不可用"))?;
        let visible: HashSet<_> = jobs.iter().filter(|j| j.visible).map(|j| &j.path).collect();
        Ok(jobs
            .iter()
            .filter(|j| !visible.contains(&j.path))
            .map(|j| j.path.clone())
            .collect())
    }
    pub(crate) fn delete(&self, path: &str) -> anyhow::Result<bool> {
        let mut jobs = self
            .jobs
            .lock()
            .map_err(|_| anyhow::anyhow!("临时锁队列不可用"))?;
        let mut next = jobs.clone();
        let Some(job) = next.iter_mut().find(|j| j.path == path && j.visible) else {
            return Ok(false);
        };
        job.visible = false;
        self.state.save("queue.json", &next)?;
        *jobs = next;
        Ok(true)
    }
    fn step(&self, writer: &Writer, deleted: &Deleted) -> anyhow::Result<bool> {
        let (job, upload) = {
            let mut jobs = self
                .jobs
                .lock()
                .map_err(|_| anyhow::anyhow!("临时锁队列不可用"))?;
            let mut seen = HashSet::new();
            let index = jobs.iter().position(|j| {
                seen.insert(j.path.clone())
                    && (j.phase == Phase::Pending || j.phase == Phase::Uploaded && !j.visible)
            });
            let Some(index) = index else { return Ok(false) };
            if jobs[index].phase == Phase::Pending && !jobs[index].visible {
                let mut next = jobs.clone();
                let done = next.remove(index);
                self.state.save("queue.json", &next)?;
                *jobs = next;
                let _ = std::fs::remove_file(self.state.dir.join(format!("{}.data", done.id)));
                return Ok(true);
            }
            let upload = jobs[index].phase == Phase::Pending;
            let mut next = jobs.clone();
            next[index].phase = if upload {
                Phase::Uploading
            } else {
                Phase::Deleting
            };
            self.state.save("queue.json", &next)?;
            *jobs = next;
            (jobs[index].clone(), upload)
        };
        let result = if upload {
            writer
                .put_content(&job.path, &self.state.dir.join(format!("{}.data", job.id)))
                .map(|m| {
                    Some(RemoteVersion {
                        id: m.remote_id,
                        etag: m.etag,
                    })
                })
        } else {
            let remote = job.remote.as_ref().context("临时锁缺少已确认版本")?;
            writer
                .delete_editor_version(&job.path, &remote.id, &remote.etag)
                .map(|()| None)
        };
        let mut jobs = self
            .jobs
            .lock()
            .map_err(|_| anyhow::anyhow!("临时锁队列不可用"))?;
        let mut next = jobs.clone();
        let index = next
            .iter()
            .position(|j| j.id == job.id)
            .context("临时锁队列记录丢失")?;
        let remove = matches!(&result, Ok(None));
        match result {
            Ok(Some(remote)) => {
                next[index].phase = Phase::Uploaded;
                next[index].remote = Some(remote);
            }
            Ok(None) => {
                next.remove(index);
            }
            Err(_) => {
                next[index].phase = Phase::Failed;
            }
        }
        if remove {
            // Keep the tombstone visible until the directory cache has
            // forgotten this exact cloud version. A later lock at the same
            // path belongs to a different editor/generation and must survive.
            let remote = job.remote.as_ref().context("临时锁缺少已确认版本")?;
            deleted(&job.path, &remote.id, &remote.etag)?;
        }
        self.state.save("queue.json", &next)?;
        *jobs = next;
        if remove {
            let _ = std::fs::remove_file(self.state.dir.join(format!("{}.data", job.id)));
        }
        Ok(true)
    }
    pub(crate) fn start(
        self: &Arc<Self>,
        writer: Weak<Writer>,
        stop: Arc<AtomicBool>,
        deleted: Deleted,
    ) {
        let queue = self.clone();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let Some(writer) = writer.upgrade() else {
                    break;
                };
                let worked = queue.step(&writer, &deleted).unwrap_or(false);
                drop(writer);
                if !worked {
                    thread::sleep(Duration::from_millis(100));
                }
            }
        });
    }
}
pub(crate) fn pending(directory: &Path) -> anyhow::Result<(u64, u64, u64)> {
    let file = directory.join("editor-locks/queue.json");
    if !file.exists() {
        return Ok((0, 0, 0));
    }
    let jobs: Vec<Job> = serde_json::from_slice(&std::fs::read(file)?)?;
    let pending = jobs
        .iter()
        .filter(|j| j.phase != Phase::Uploaded || !j.visible);
    Ok(pending.fold((0, 0, 0), |(n, size, failed), j| {
        (
            n + 1,
            size + j.size,
            failed + u64::from(j.phase == Phase::Failed),
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupted_cloud_replies_keep_bytes_and_require_recovery() {
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path().join("editor-locks")).unwrap();
        let job = Job {
            id: "synthetic1".into(),
            path: "/.~lock.book.xlsx#".into(),
            size: 4,
            modified: 1,
            visible: false,
            phase: Phase::Uploading,
            remote: None,
        };
        state.write_bytes("synthetic1.data", b"lock").unwrap();
        state.save("queue.json", &vec![job]).unwrap();
        let queue = EditorLocks::new(&state.dir).unwrap();
        assert!(queue.jobs.lock().unwrap()[0].phase == Phase::Failed);
        assert_eq!(pending(root.path()).unwrap(), (1, 4, 1));
        assert_eq!(
            std::fs::read(state.dir.join("synthetic1.data")).unwrap(),
            b"lock"
        );
        assert!(queue.hidden().unwrap().contains("/.~lock.book.xlsx#"));
    }
    #[test]
    fn queue_rejects_paths_outside_the_narrow_office_lock_namespace() {
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path().to_owned()).unwrap();
        let job = Job {
            id: "../escape".into(),
            path: "/ordinary.xlsx".into(),
            size: 0,
            modified: 1,
            visible: true,
            phase: Phase::Pending,
            remote: None,
        };
        state.save("queue.json", &vec![job]).unwrap();
        assert!(EditorLocks::new(&state.dir).is_err());
    }
}
