use crate::protocol::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};
use twodrive_backend::{CloudBackend, MockBackend};
use twodrive_core::{AppPaths, Database, FileState, MetadataEntry, TokenStore};
mod cloud;

#[derive(Clone)]
pub struct Engine(Arc<Inner>);
struct Inner {
    paths: AppPaths,
    db: Database,
    cache: PathBuf,
    backend: Option<Arc<MockBackend>>,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    cloud_identity: Option<twodrive_core::CloudIdentity>,
    cloud_status: String,
    cloud_error: Option<String>,
    cloud_offset: usize,
    refresh_cancel: Option<Arc<AtomicBool>>,
    download_cancel: Option<(String, Arc<AtomicBool>)>,
    epoch: u64,
    browse_links: std::collections::HashSet<String>,
    graph: Option<Arc<twodrive_backend::GraphBackend>>,
    directory: Option<DirectoryView>,
    browse_cancel: Option<Arc<AtomicBool>>,
    browse_workers: usize,
    signing_out: bool,
    signed_in: bool,
    login: Option<Arc<AtomicBool>>,
    auth_error: Option<String>,
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
        db.init_cloud()?;
        let backend = mock.then(|| Arc::new(MockBackend::new()));
        if let Some(backend) = &backend {
            for entry in backend.list_all()? {
                db.upsert_metadata(&entry)?;
            }
        }
        let paused = db.get_state_value("windows_paused")?.as_deref() == Some("true");
        let paths = auth_paths(root);
        let signed_in = !mock && TokenStore::new(&paths.token_path).load()?.is_some();
        let cloud_identity = if signed_in {
            db.get_state_value("cloud_identity")?
                .and_then(|s| serde_json::from_str(&s).ok())
        } else {
            None
        };
        if let Some(identity) = &cloud_identity {
            for mut task in db.cloud_tasks(identity)? {
                if matches!(task.state.as_str(), "downloading" | "queued") {
                    task.state = "error".into();
                    task.error = Some("interrupted_restart_resume_available".into());
                    db.cloud_task_save(&task)?;
                }
            }
        }
        Ok(Self(Arc::new(Inner {
            paths,
            db,
            cache,
            backend,
            state: Mutex::new(State {
                cloud_identity,
                cloud_status: "idle".into(),
                signed_in,
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
            cloud: self.cloud_view(state)?,
            directory: state.directory.clone().map(|mut d| {
                if d.fetched_at
                    .is_some_and(|t| twodrive_core::now_unix() - t > 300)
                    && matches!(d.status.as_str(), "complete" | "partial")
                {
                    d.status = "stale".into();
                }
                d
            }),
            auth_status: if state.signing_out {
                "signing_out"
            } else if state.login.is_some() {
                "signing_in"
            } else if state.signed_in {
                "signed_in"
            } else {
                "signed_out"
            }
            .into(),
            auth_error: state.auth_error.clone(),
            engine_version: env!("CARGO_PKG_VERSION").into(),
            engine_pid: std::process::id(),
            revision: state.revision,
            mode: if mock {
                "isolated_mock"
            } else if state.signed_in {
                "authenticated_preview"
            } else {
                "unconfigured"
            }
            .into(),
            status: if state.login.is_some() {
                "signing_in"
            } else if state.signed_in && !mock {
                "signed_in"
            } else if state.paused && state.active.is_some() {
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
                vec![
                    "pause",
                    "browser_login",
                    "cloud_browse",
                    "logout",
                    "cloud_index",
                    "cloud_download",
                ]
            }
            .into_iter()
            .map(str::to_string)
            .collect(),
            unsupported: [
                "cfapi",
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
                Command::RefreshIndex => self.refresh_index(&mut state)?,
                Command::CancelRefresh => {
                    if let Some(c) = &state.refresh_cancel {
                        c.store(true, Ordering::SeqCst);
                    }
                }
                Command::IndexPage { offset } => {
                    state.cloud_offset = *offset;
                }
                Command::DownloadCloud { id } => self.start_cloud_download(&mut state, id)?,
                Command::CancelDownload { id } => {
                    if let Some((active, c)) = &state.download_cancel
                        && active == id
                    {
                        c.store(true, Ordering::SeqCst);
                    }
                }
                Command::OpenCached { id, reveal } => self.open_cached(&state, id, *reveal)?,
                Command::Snapshot => (),
                Command::Browse {
                    query_id,
                    drive_id,
                    item_id,
                } => {
                    anyhow::ensure!(state.signed_in && !state.signing_out, "not_signed_in");
                    anyhow::ensure!(
                        !query_id.is_empty() && query_id.len() <= 128,
                        "invalid_query_id"
                    );
                    anyhow::ensure!(state.browse_workers < 4, "browse_busy_retry");
                    if let Some(cancel) = state.browse_cancel.take() {
                        cancel.store(true, Ordering::SeqCst);
                    }
                    state.directory = Some(DirectoryView {
                        page_number: 0,
                        query_id: query_id.clone(),
                        status: "loading".into(),
                        error: None,
                        page: None,
                        fetched_at: None,
                    });
                    state.browse_links.clear();
                    self.launch_browse(
                        &mut state,
                        query_id.clone(),
                        drive_id.clone(),
                        item_id.clone(),
                        None,
                    );
                }
                Command::BrowseNext { query_id } => {
                    anyhow::ensure!(state.signed_in && !state.signing_out, "not_signed_in");
                    anyhow::ensure!(state.browse_workers < 4, "browse_busy_retry");
                    let directory = state
                        .directory
                        .as_mut()
                        .ok_or_else(|| anyhow::anyhow!("no_directory"))?;
                    anyhow::ensure!(
                        directory.query_id == *query_id && directory.status != "loading",
                        "stale_query"
                    );
                    let page = directory
                        .page
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("no_directory"))?;
                    anyhow::ensure!(
                        directory
                            .fetched_at
                            .is_some_and(|t| twodrive_core::now_unix() - t <= 300),
                        "directory_cache_expired"
                    );
                    let next = page
                        .next_link
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("no_more_pages"))?;
                    let drive = page.drive_id.clone();
                    let item = page.item_id.clone();
                    directory.status = "loading".into();
                    directory.error = None;
                    self.launch_browse(
                        &mut state,
                        query_id.clone(),
                        Some(drive),
                        Some(item),
                        Some(next),
                    );
                }
                Command::CancelBrowse { query_id } => {
                    if state
                        .directory
                        .as_ref()
                        .is_some_and(|d| d.query_id == *query_id)
                    {
                        if let Some(cancel) = state.browse_cancel.take() {
                            cancel.store(true, Ordering::SeqCst);
                        }
                        state.directory = None;
                        state.revision += 1;
                    }
                }
                Command::Logout => {
                    state.epoch += 1;
                    if let Some(c) = &state.refresh_cancel {
                        c.store(true, Ordering::SeqCst);
                    }
                    if let Some((_, c)) = &state.download_cancel {
                        c.store(true, Ordering::SeqCst);
                    }
                    state.cloud_identity = None;
                    self.0.db.set_state_value("cloud_identity", "")?;
                    anyhow::ensure!(
                        state.login.is_none() && !state.signing_out,
                        "authentication_busy"
                    );
                    anyhow::ensure!(self.0.backend.is_none(), "capability_unavailable");
                    if let Some(cancel) = state.browse_cancel.take() {
                        cancel.store(true, Ordering::SeqCst);
                    }
                    state.directory = None;
                    state.signed_in = false;
                    state.signing_out = true;
                    state.replies.clear();
                    let graph = state.graph.take();
                    let engine = self.clone();
                    thread::spawn(move || {
                        let result = match graph {
                            Some(g) => g.forget_credentials(),
                            None => TokenStore::new(&engine.0.paths.token_path).delete(),
                        };
                        let mut state = engine.0.state.lock().unwrap();
                        state.signing_out = false;
                        state.auth_error = result.err().map(|_| "credential_removal_failed".into());
                        state.revision += 1;
                    });
                    state.revision += 1;
                }
                Command::Login => {
                    anyhow::ensure!(!state.signing_out, "authentication_busy");
                    anyhow::ensure!(self.0.backend.is_none(), "login_unavailable_in_mock");
                    anyhow::ensure!(!state.signed_in, "already_signed_in");
                    anyhow::ensure!(state.login.is_none(), "login_in_progress");
                    let cancelled = Arc::new(AtomicBool::new(false));
                    state.login = Some(cancelled.clone());
                    state.auth_error = None;
                    state.revision += 1;
                    let engine = self.clone();
                    thread::spawn(move || {
                        let result = twodrive_backend::GraphBackend::login_cancellable(
                            &engine.0.paths,
                            &cancelled,
                        );
                        let mut state = engine.0.state.lock().unwrap();
                        state.login = None;
                        state.signed_in = result.is_ok();
                        state.auth_error = result.err().map(|_| {
                            if cancelled.load(Ordering::SeqCst) {
                                "登录已取消"
                            } else {
                                "登录未完成：可能已超时、授权被拒绝或网络不可用，请重试。"
                            }
                            .into()
                        });
                        state.revision += 1;
                    });
                }
                Command::CancelLogin => {
                    if let Some(cancelled) = &state.login {
                        cancelled.store(true, Ordering::SeqCst);
                    }
                }
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
            cloud: None,
            directory: None,
            auth_status: "unknown".into(),
            auth_error: None,
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
        if !matches!(
            request.command,
            Command::Snapshot
                | Command::Browse { .. }
                | Command::BrowseNext { .. }
                | Command::CancelBrowse { .. }
                | Command::Logout
        ) {
            state
                .replies
                .push_back((request.id, fingerprint, reply.clone()));
            if state.replies.len() > 256 {
                state.replies.pop_front();
            }
        }
        reply
    }

    fn launch_browse(
        &self,
        state: &mut State,
        query: String,
        drive: Option<String>,
        item: Option<String>,
        next: Option<String>,
    ) {
        let cancel = Arc::new(AtomicBool::new(false));
        state.browse_cancel = Some(cancel.clone());
        state.browse_workers += 1;
        state.revision += 1;
        let engine = self.clone();
        thread::spawn(move || {
            let mut check = || {
                anyhow::ensure!(!cancel.load(Ordering::SeqCst), "browse_cancelled");
                Ok(())
            };
            let result = (|| {
                check()?;
                let graph = {
                    let mut state = engine.0.state.lock().unwrap();
                    check()?;
                    if state.graph.is_none() {
                        state.graph = Some(Arc::new(
                            twodrive_backend::GraphBackend::from_paths(&engine.0.paths)
                                .map_err(|_| anyhow::anyhow!("reauthentication_required"))?,
                        ));
                    }
                    state.graph.as_ref().unwrap().clone()
                };
                graph.browse_directory(
                    drive.as_deref(),
                    item.as_deref(),
                    next.as_deref(),
                    &mut check,
                )
            })();
            let mut state = engine.0.state.lock().unwrap();
            state.browse_workers -= 1;
            finish_browse(&mut state, &query, &cancel, result);
        });
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

pub fn auth_paths(root: &Path) -> AppPaths {
    let config_dir = root.join("auth");
    AppPaths {
        config_path: config_dir.join("config.toml"),
        token_path: config_dir.join("tokens.json"),
        config_dir,
        data_dir: root.join("account-data"),
        cache_dir: root.join("account-data/cache"),
        db_path: root.join("account-data/metadata.sqlite3"),
        mount_dir: root.join("account-data/unmounted"),
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

fn finish_browse(
    state: &mut State,
    query: &str,
    cancel: &AtomicBool,
    result: anyhow::Result<twodrive_backend::onedrive::browse::DirectoryPage>,
) {
    if cancel.load(Ordering::SeqCst) || !state.signed_in {
        return;
    }
    let Some(directory) = state.directory.as_mut().filter(|d| d.query_id == query) else {
        return;
    };
    match result {
        Ok(page) => {
            directory.error = None;
            if page
                .next_link
                .as_ref()
                .is_some_and(|link| !state.browse_links.insert(link.clone()))
            {
                directory.status = "failed".into();
                directory.error = Some("repeated_directory_continuation".into());
                state.revision += 1;
                return;
            }
            directory.page_number += 1;
            directory.status = if page.has_more { "partial" } else { "complete" }.into();
            directory.page = Some(page);
            directory.fetched_at = Some(twodrive_core::now_unix());
        }
        Err(e) => {
            directory.status = "failed".into();
            directory.error = Some(e.to_string());
        }
    }
    state.revision += 1;
}

#[cfg(test)]
mod browse_tests {
    use super::*;
    use twodrive_backend::onedrive::browse::DirectoryPage;
    fn state() -> State {
        State {
            signed_in: true,
            directory: Some(DirectoryView {
                query_id: "new".into(),
                status: "loading".into(),
                error: None,
                page: None,
                page_number: 0,
                fetched_at: None,
            }),
            ..State::default()
        }
    }
    fn page(next: Option<&str>) -> DirectoryPage {
        DirectoryPage {
            account_id: "account".into(),
            drive_id: "drive".into(),
            item_id: "root".into(),
            items: vec![],
            next_link: next.map(str::to_owned),
            has_more: next.is_some(),
        }
    }
    #[test]
    fn late_completion_after_switch_cancel_or_logout_cannot_publish() {
        for reason in ["switch", "cancel", "logout"] {
            let mut state = state();
            if reason == "logout" {
                state.signed_in = false;
            }
            let cancel = AtomicBool::new(reason == "cancel");
            finish_browse(
                &mut state,
                if reason == "switch" { "old" } else { "new" },
                &cancel,
                Ok(page(None)),
            );
            assert!(state.directory.unwrap().page.is_none());
            assert!(state.recent.is_empty());
        }
    }
    #[test]
    fn empty_is_confirmed_only_on_success_and_page_failure_preserves_partial() {
        let mut state = state();
        let cancel = AtomicBool::new(false);
        assert!(state.directory.as_ref().unwrap().page.is_none());
        finish_browse(
            &mut state,
            "new",
            &cancel,
            Err(anyhow::anyhow!("permission_denied")),
        );
        assert_eq!(state.directory.as_ref().unwrap().status, "failed");
        assert!(state.directory.as_ref().unwrap().page.is_none());
        finish_browse(&mut state, "new", &cancel, Ok(page(Some("next"))));
        assert_eq!(state.directory.as_ref().unwrap().status, "partial");
        finish_browse(
            &mut state,
            "new",
            &cancel,
            Err(anyhow::anyhow!("network_unavailable")),
        );
        assert!(
            state
                .directory
                .as_ref()
                .unwrap()
                .page
                .as_ref()
                .unwrap()
                .has_more
        );
        finish_browse(&mut state, "new", &cancel, Ok(page(None)));
        assert_eq!(state.directory.as_ref().unwrap().status, "complete");
        assert_eq!(state.directory.as_ref().unwrap().page_number, 2);
        assert!(state.recent.is_empty());
    }
    #[test]
    fn continuation_cycles_are_rejected() {
        let mut state = state();
        let cancel = AtomicBool::new(false);
        finish_browse(&mut state, "new", &cancel, Ok(page(Some("A"))));
        finish_browse(&mut state, "new", &cancel, Ok(page(Some("B"))));
        finish_browse(&mut state, "new", &cancel, Ok(page(Some("A"))));
        assert_eq!(
            state.directory.as_ref().unwrap().error.as_deref(),
            Some("repeated_directory_continuation")
        );
        assert_eq!(state.directory.as_ref().unwrap().page_number, 2);
    }
}
