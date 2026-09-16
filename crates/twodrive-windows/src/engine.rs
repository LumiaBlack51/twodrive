use crate::protocol::*;
use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};
use twodrive_backend::{CloudBackend, MockBackend};
use twodrive_core::{Database, FileState, MetadataEntry};

#[derive(Clone)]
pub struct Engine(Arc<Inner>);
struct Inner {
    db: Database,
    cache: PathBuf,
    backend: Option<Arc<MockBackend>>,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    paused: bool,
    revision: u64,
    queue: VecDeque<Job>,
    active: Option<Transfer>,
    recent: VecDeque<Transfer>,
    replies: VecDeque<(String, String, Reply)>,
}
#[derive(Clone)]
struct Job {
    id: String,
    upload: bool,
}

impl Engine {
    pub fn open(root: &Path, mock: bool) -> anyhow::Result<Self> {
        fs::create_dir_all(root)?;
        let marker = root.join("mode");
        let mode = if mock {
            "isolated_mock_v1"
        } else {
            "unconfigured_v1"
        };
        if marker.exists() {
            anyhow::ensure!(
                fs::read_to_string(&marker)? == mode,
                "state mode mismatch; use a new isolated directory"
            );
        } else {
            anyhow::ensure!(
                fs::read_dir(root)?.all(|e| e
                    .is_ok_and(|e| e.file_name() == "engine.lock" || e.file_name() == "tray.lock")),
                "refusing nonempty unowned state directory"
            );
            fs::write(marker, mode)?;
        }
        let cache = root.join("cache");
        fs::create_dir_all(&cache)?;
        let db = Database::new(root.join("engine.sqlite3"));
        db.init()?;
        let backend = mock.then(|| Arc::new(MockBackend::new()));
        if let Some(backend) = &backend {
            for entry in backend.list_all()? {
                db.upsert_metadata(&entry)?;
            }
        }
        let paused = db.get_state_value("windows_paused")?.as_deref() == Some("true");
        Ok(Self(Arc::new(Inner {
            db,
            cache,
            backend,
            state: Mutex::new(State {
                paused,
                ..State::default()
            }),
        })))
    }

    pub fn start(&self) {
        let engine = self.clone();
        thread::spawn(move || {
            loop {
                if engine.run_one().is_err() {
                    // Preserve dirty content; a failed operation is never advertised as complete.
                }
                thread::sleep(Duration::from_millis(50));
            }
        });
    }

    pub fn snapshot(&self) -> anyhow::Result<Snapshot> {
        let state = self.0.state.lock().unwrap();
        self.snapshot_locked(&state)
    }

    fn snapshot_locked(&self, state: &State) -> anyhow::Result<Snapshot> {
        let records = self.0.db.all_records()?;
        let file_count = records.iter().filter(|r| !r.metadata.is_dir).count();
        let mock = self.0.backend.is_some();
        Ok(Snapshot {
            engine_version: env!("CARGO_PKG_VERSION").into(),
            engine_pid: std::process::id(),
            revision: state.revision,
            mode: if mock {
                "isolated_mock"
            } else {
                "unconfigured"
            }
            .into(),
            status: if state.paused && state.active.is_some() {
                "draining"
            } else if state.paused {
                "paused"
            } else if state.active.is_some() {
                "transferring"
            } else if !state.queue.is_empty() {
                "queued"
            } else if mock {
                "mock_idle"
            } else {
                "signed_out"
            }
            .into(),
            paused: state.paused,
            queued: state.queue.len(),
            active: state.active.clone(),
            recent: state.recent.iter().cloned().collect(),
            files: records
                .into_iter()
                .filter(|r| !r.metadata.is_dir)
                .take(200)
                .map(|r| Item {
                    id: r.metadata.remote_id,
                    name: r.metadata.name,
                    size: r.metadata.size,
                    state: r.state.to_string(),
                })
                .collect(),
            file_count,
            capabilities: if mock {
                vec!["pause", "download", "release", "mock_import"]
            } else {
                vec!["pause"]
            }
            .into_iter()
            .map(str::to_string)
            .collect(),
            unsupported: [
                "cfapi",
                "browser_login",
                "account_switch",
                "peer_file_sync",
                "open_sync_root",
                "pin",
                "bandwidth",
                "autostart",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        })
    }

    pub fn handle(&self, request: Request) -> Reply {
        let mut state = self.0.state.lock().unwrap();
        let fingerprint = serde_json::to_string(&request).unwrap();
        if let Some((_, previous, reply)) =
            state.replies.iter().find(|(id, _, _)| id == &request.id)
        {
            if previous == &fingerprint {
                return reply.clone();
            }
            return Reply {
                version: VERSION,
                id: request.id,
                ok: false,
                error: Some("request_id_reused".into()),
                snapshot: self.snapshot_locked(&state).unwrap(),
            };
        }
        let result = (|| -> anyhow::Result<()> {
            anyhow::ensure!(request.version == VERSION, "unsupported_protocol");
            anyhow::ensure!(
                !request.id.is_empty() && request.id.len() <= 128,
                "invalid_request_id"
            );
            match &request.command {
                Command::Snapshot => (),
                Command::SetPaused { paused } => {
                    self.0.db.set_state_value(
                        "windows_paused",
                        if *paused { "true" } else { "false" },
                    )?;
                    state.paused = *paused;
                    state.revision += 1;
                }
                Command::Download { id } => {
                    anyhow::ensure!(self.0.backend.is_some(), "capability_unavailable");
                    anyhow::ensure!(state.queue.len() < 128, "queue_full");
                    let record = self
                        .0
                        .db
                        .get_by_remote_id(id)?
                        .ok_or_else(|| anyhow::anyhow!("unknown_item"))?;
                    anyhow::ensure!(!record.metadata.is_dir, "not_a_file");
                    anyhow::ensure!(
                        record.state == FileState::OnlineOnly,
                        "download_requires_online_only_file"
                    );
                    anyhow::ensure!(
                        state.active.as_ref().is_none_or(|a| a.id != *id)
                            && !state.queue.iter().any(|j| j.id == *id),
                        "already_queued"
                    );
                    state.queue.push_back(Job {
                        id: id.clone(),
                        upload: false,
                    });
                    state.revision += 1;
                }
                Command::Release { id } => {
                    anyhow::ensure!(self.0.backend.is_some(), "capability_unavailable");
                    anyhow::ensure!(
                        state.active.as_ref().is_none_or(|a| a.id != *id)
                            && !state.queue.iter().any(|j| j.id == *id),
                        "file_busy"
                    );
                    let record = self
                        .0
                        .db
                        .get_by_remote_id(id)?
                        .ok_or_else(|| anyhow::anyhow!("unknown_item"))?;
                    anyhow::ensure!(
                        self.0.db.release_record(&record)?,
                        "release_refused_dirty_pinned_busy_or_online"
                    );
                    state.revision += 1;
                }
                Command::MockImport { name, content } => {
                    anyhow::ensure!(self.0.backend.is_some(), "capability_unavailable");
                    anyhow::ensure!(
                        !name.is_empty()
                            && name.len() < 128
                            && name
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
                            && name != "."
                            && name != "..",
                        "invalid_mock_name"
                    );
                    anyhow::ensure!(
                        content.len() <= 256 * 1024 && state.queue.len() < 128,
                        "mock_input_limit"
                    );
                    let id = format!(
                        "local-{}-{}",
                        std::process::id(),
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)?
                            .as_nanos()
                    );
                    let path = self.0.cache.join(&id);
                    // create_new prevents overwriting an earlier generation after restart.
                    use std::io::Write;
                    let mut file = fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)?;
                    file.write_all(content.as_bytes())?;
                    file.sync_all()?;
                    self.0
                        .db
                        .create_local_file(&id, &format!("/{name}"), &path)?;
                    self.0.db.mark_dirty_with_size(&id, content.len() as u64)?;
                    state.queue.push_back(Job { id, upload: true });
                    state.revision += 1;
                }
            }
            Ok(())
        })();
        let snapshot = self.snapshot_locked(&state).unwrap_or_else(|_| Snapshot {
            engine_version: env!("CARGO_PKG_VERSION").into(),
            engine_pid: std::process::id(),
            revision: state.revision,
            mode: "error".into(),
            status: "error".into(),
            paused: state.paused,
            queued: state.queue.len(),
            active: None,
            recent: vec![],
            files: vec![],
            file_count: 0,
            capabilities: vec![],
            unsupported: vec!["all".into()],
        });
        let reply = Reply {
            version: VERSION,
            id: request.id.clone(),
            ok: result.is_ok(),
            error: result.err().map(|e| e.to_string()),
            snapshot,
        };
        if !matches!(request.command, Command::Snapshot) {
            state
                .replies
                .push_back((request.id, fingerprint, reply.clone()));
            if state.replies.len() > 256 {
                state.replies.pop_front();
            }
        }
        reply
    }

    pub fn run_one(&self) -> anyhow::Result<bool> {
        let job = {
            let mut state = self.0.state.lock().unwrap();
            if state.paused || state.active.is_some() {
                return Ok(false);
            }
            let Some(job) = state.queue.pop_front() else {
                return Ok(false);
            };
            let record = self
                .0
                .db
                .get_by_remote_id(&job.id)?
                .ok_or_else(|| anyhow::anyhow!("unknown_item"))?;
            state.active = Some(Transfer {
                id: job.id.clone(),
                name: record.metadata.name,
                direction: if job.upload { "upload" } else { "download" }.into(),
                done: 0,
                total: record.metadata.size,
                outcome: "running".into(),
            });
            state.revision += 1;
            job
        };
        let result = (|| -> anyhow::Result<()> {
            let backend = self
                .0
                .backend
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("capability_unavailable"))?;
            let record = self
                .0
                .db
                .get_by_remote_id(&job.id)?
                .ok_or_else(|| anyhow::anyhow!("unknown_item"))?;
            if job.upload {
                anyhow::ensure!(
                    twodrive_fs::recover_dirty_record(
                        &self.0.db,
                        backend.as_ref(),
                        record,
                        &mut |done, total| {
                            self.progress(done, total);
                            Ok(())
                        }
                    )?,
                    "upload_unconfirmed_content_retained"
                );
            } else {
                let observed = Observed {
                    backend: backend.clone(),
                    engine: self.clone(),
                };
                twodrive_fs::hydrate_record(&self.0.db, &self.0.cache, &observed, &record)?;
            }
            Ok(())
        })();
        let mut state = self.0.state.lock().unwrap();
        if let Some(mut transfer) = state.active.take() {
            transfer.outcome = if result.is_ok() {
                "completed"
            } else {
                "failed"
            }
            .into();
            if result.is_ok() {
                transfer.done = transfer.total;
            }
            state.recent.push_front(transfer);
            state.recent.truncate(32);
        }
        state.revision += 1;
        result.map(|()| true)
    }

    fn progress(&self, done: u64, total: u64) {
        let mut state = self.0.state.lock().unwrap();
        if let Some(active) = &mut state.active {
            active.done = done.min(total);
            active.total = total;
        }
        state.revision += 1;
    }
}

struct Observed {
    backend: Arc<MockBackend>,
    engine: Engine,
}
impl CloudBackend for Observed {
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        self.backend.list_all()
    }
    fn download(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        self.backend.download(id)
    }
    fn download_sized_to(
        &self,
        id: &str,
        size: u64,
        writer: &mut dyn std::io::Write,
        callback: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        self.backend
            .download_sized_to(id, size, writer, &mut |done| {
                self.engine.progress(done, size);
                callback(done)
            })
    }
    fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        self.backend.upload(path, content)
    }
    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        self.backend.create_folder(path)
    }
    fn rename(&self, id: &str, path: &str) -> anyhow::Result<MetadataEntry> {
        self.backend.rename(id, path)
    }
    fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.backend.delete(id)
    }
}
