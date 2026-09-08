use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use twodrive_backend::{CloudBackend, GraphBackend};
use twodrive_core::{
    AppPaths, Config, Database, KnownFolderConfig, join_cloud_path, normalize_cloud_path, now_unix,
};
use twodrive_fs::{mount_graph, mount_mock};

fn main() -> anyhow::Result<()> {
    if env::args()
        .nth(1)
        .is_some_and(|arg| matches!(arg.as_str(), "--version" | "-V" | "version"))
    {
        println!("twodrive-daemon {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let paths = AppPaths::discover()?;
    paths.ensure()?;
    Database::new(paths.db_path.clone()).init()?;
    if env::var("TWODRIVE_BACKEND").as_deref() == Ok("mock") {
        mount_mock(paths)
    } else {
        start_known_folder_sync(paths.clone());
        mount_graph(paths)
    }
}

fn start_known_folder_sync(paths: AppPaths) {
    let Ok(config) = Config::load_or_create(&paths) else {
        return;
    };
    if !config.known_folders.enabled {
        return;
    }
    if config.known_folders.mode != "upload_only" {
        eprintln!(
            "twodrive: known folders mode '{}' is unsupported; expected upload_only",
            config.known_folders.mode
        );
        return;
    }
    if config.known_folders.upload_deletes {
        eprintln!("twodrive: known folders upload_deletes=true is ignored for safety");
    }

    thread::spawn(move || {
        if let Err(err) = run_known_folder_sync(paths, config) {
            eprintln!("twodrive: known folder sync stopped: {err:#}");
        }
    });
}

fn run_known_folder_sync(paths: AppPaths, config: Config) -> anyhow::Result<()> {
    let debounce = Duration::from_secs(config.known_folder_debounce_seconds()?);
    let rescan_interval = match config.known_folder_rescan_seconds()? {
        0 => None,
        seconds => Some(Duration::from_secs(seconds)),
    };
    let roots = configured_known_folders(&config.known_folders.folders)?;
    if roots.is_empty() {
        return Ok(());
    }

    paths.ensure()?;
    let db = Database::new(paths.db_path.clone());
    db.init()?;
    let backend = GraphBackend::from_paths(&paths)?;
    let mut state = KnownFolderState::load(&paths.data_dir)?;

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = tx.send(event);
    })?;
    for root in &roots {
        if root.local.exists() {
            watcher.watch(&root.local, RecursiveMode::Recursive)?;
            eprintln!(
                "twodrive: watching known folder {} -> {}",
                root.local.display(),
                root.remote
            );
        }
    }

    retry_known_folder_uploads(&paths, &backend, &db, &config, &mut state)?;

    if !state.baseline_initialized {
        for root in &roots {
            baseline_known_folder_path(&config, &root.local, &mut state)?;
        }
        state.baseline_initialized = true;
        state.save(&paths.data_dir)?;
        eprintln!(
            "twodrive: initialized known folder baseline with {} files",
            state.files.len()
        );
    }

    if config.known_folders.startup_scan {
        for root in &roots {
            sync_known_folder_root(&paths, &backend, &db, &config, root, &mut state)?;
        }
        state.save(&paths.data_dir)?;
    }

    let mut pending = HashSet::new();
    let mut last_scan = Instant::now();
    let mut last_retry = Instant::now();
    let wake_interval = rescan_interval
        .unwrap_or(Duration::from_secs(60))
        .min(Duration::from_secs(60));
    loop {
        if last_retry.elapsed() >= Duration::from_secs(60) {
            retry_known_folder_uploads(&paths, &backend, &db, &config, &mut state)?;
            last_retry = Instant::now();
        }
        match recv_watch_wake(&rx, Some(wake_interval)) {
            WatchWake::Event(Ok(event)) => {
                if !queue_addition_event(&event, &roots, &config, &mut pending) {
                    continue;
                }
                wait_for_relevant_quiet(&rx, debounce, &roots, &config, &mut pending);
                sync_known_folder_paths(
                    &paths, &backend, &db, &config, &roots, &pending, &mut state,
                )?;
                pending.clear();
                state.save(&paths.data_dir)?;
            }
            WatchWake::Event(Err(err)) => {
                eprintln!("twodrive: known folder watcher event error: {err}");
            }
            WatchWake::Rescan => {
                if rescan_interval.is_none_or(|interval| last_scan.elapsed() < interval) {
                    continue;
                }
                last_scan = Instant::now();
                for root in &roots {
                    sync_known_folder_root(&paths, &backend, &db, &config, root, &mut state)?;
                }
                state.save(&paths.data_dir)?;
            }
            WatchWake::Disconnected => break,
        }
    }
    Ok(())
}

enum WatchWake {
    Event(notify::Result<Event>),
    Rescan,
    Disconnected,
}

fn recv_watch_wake(
    rx: &mpsc::Receiver<notify::Result<Event>>,
    rescan_interval: Option<Duration>,
) -> WatchWake {
    match rescan_interval {
        Some(interval) => match rx.recv_timeout(interval) {
            Ok(event) => WatchWake::Event(event),
            Err(mpsc::RecvTimeoutError::Timeout) => WatchWake::Rescan,
            Err(mpsc::RecvTimeoutError::Disconnected) => WatchWake::Disconnected,
        },
        None => match rx.recv() {
            Ok(event) => WatchWake::Event(event),
            Err(_) => WatchWake::Disconnected,
        },
    }
}

fn queue_addition_event(
    event: &Event,
    roots: &[KnownFolderRoot],
    config: &Config,
    pending: &mut HashSet<PathBuf>,
) -> bool {
    if !is_addition_event(event) {
        return false;
    }

    let before = pending.len();
    for path in &event.paths {
        if roots.iter().any(|root| path.starts_with(&root.local)) && !should_skip(path, config) {
            pending.insert(path.clone());
        }
    }
    pending.len() > before
}

fn wait_for_relevant_quiet(
    rx: &mpsc::Receiver<notify::Result<Event>>,
    debounce: Duration,
    roots: &[KnownFolderRoot],
    config: &Config,
    pending: &mut HashSet<PathBuf>,
) {
    let mut quiet_until = Instant::now() + debounce;
    while let Some(remaining) = quiet_until.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(Ok(event)) => {
                let queued = queue_addition_event(&event, roots, config, pending);
                if queued || event_touches_pending(&event, pending) {
                    quiet_until = Instant::now() + debounce;
                }
            }
            Ok(Err(err)) => {
                eprintln!("twodrive: known folder watcher event error: {err}");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn is_addition_event(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_)
            | EventKind::Modify(ModifyKind::Name(RenameMode::To | RenameMode::Both))
    )
}

fn event_touches_pending(event: &Event, pending: &HashSet<PathBuf>) -> bool {
    event.paths.iter().any(|path| {
        pending
            .iter()
            .any(|pending_path| path.starts_with(pending_path) || pending_path.starts_with(path))
    })
}

fn configured_known_folders(folders: &[KnownFolderConfig]) -> anyhow::Result<Vec<KnownFolderRoot>> {
    let mut roots = Vec::new();
    for folder in folders {
        if folder.local.trim().is_empty() || folder.remote.trim().is_empty() {
            continue;
        }
        let local = expand_home(&folder.local)?;
        let remote = normalize_cloud_path(&folder.remote);
        roots.push(KnownFolderRoot { local, remote });
    }
    Ok(roots)
}

fn sync_known_folder_paths<B: CloudBackend>(
    paths: &AppPaths,
    backend: &B,
    db: &Database,
    config: &Config,
    roots: &[KnownFolderRoot],
    pending: &HashSet<PathBuf>,
    state: &mut KnownFolderState,
) -> anyhow::Result<()> {
    let mut jobs = Vec::new();
    for path in pending {
        let Some(root) = roots.iter().find(|root| path.starts_with(&root.local)) else {
            continue;
        };
        if !path.exists() {
            state.remove_path(path);
            continue;
        }
        collect_local_path(config, root, path, state, &mut jobs)?;
    }
    process_known_folder_uploads(paths, backend, db, config, jobs, state)
}

fn sync_known_folder_root<B: CloudBackend>(
    paths: &AppPaths,
    backend: &B,
    db: &Database,
    config: &Config,
    root: &KnownFolderRoot,
    state: &mut KnownFolderState,
) -> anyhow::Result<()> {
    if !root.local.exists() {
        eprintln!(
            "twodrive: known folder source does not exist: {}",
            root.local.display()
        );
        return Ok(());
    }
    let mut jobs = Vec::new();
    collect_local_path(config, root, &root.local, state, &mut jobs)?;
    process_known_folder_uploads(paths, backend, db, config, jobs, state)
}

fn baseline_known_folder_path(
    config: &Config,
    path: &Path,
    state: &mut KnownFolderState,
) -> anyhow::Result<()> {
    if should_skip(path, config) {
        return Ok(());
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            baseline_known_folder_path(config, &entry?.path(), state)?;
        }
    } else if metadata.is_file() {
        state
            .files
            .entry(path.to_string_lossy().into_owned())
            .or_insert_with(|| FileSnapshot::from_metadata(&metadata));
    }
    Ok(())
}

fn collect_local_path(
    config: &Config,
    root: &KnownFolderRoot,
    path: &Path,
    state: &KnownFolderState,
    jobs: &mut Vec<KnownFolderUploadJob>,
) -> anyhow::Result<()> {
    if should_skip(path, config) {
        return Ok(());
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            collect_local_path(config, root, &entry.path(), state, jobs)?;
        }
        return Ok(());
    }
    if !metadata.is_file() {
        return Ok(());
    }

    let snapshot = FileSnapshot::from_metadata(&metadata);
    let local_key = path.to_string_lossy().into_owned();
    if state.files.get(&local_key) == Some(&snapshot) {
        return Ok(());
    }

    let remote_path = remote_path_for(root, path)?;
    jobs.push(KnownFolderUploadJob {
        local_path: path.to_path_buf(),
        remote_path,
        snapshot,
    });
    Ok(())
}

fn retry_known_folder_uploads<B: CloudBackend>(
    paths: &AppPaths,
    backend: &B,
    db: &Database,
    config: &Config,
    state: &mut KnownFolderState,
) -> anyhow::Result<()> {
    state.prune_missing_pending();
    let jobs = state
        .pending
        .iter()
        .filter_map(|(local, pending)| {
            let metadata = fs::metadata(local).ok()?;
            Some(KnownFolderUploadJob {
                local_path: PathBuf::from(local),
                remote_path: pending.remote_path.clone(),
                snapshot: FileSnapshot::from_metadata(&metadata),
            })
        })
        .collect();
    state.save(&paths.data_dir)?;
    process_known_folder_uploads(paths, backend, db, config, jobs, state)
}

fn process_known_folder_uploads<B: CloudBackend>(
    paths: &AppPaths,
    backend: &B,
    db: &Database,
    config: &Config,
    jobs: Vec<KnownFolderUploadJob>,
    state: &mut KnownFolderState,
) -> anyhow::Result<()> {
    if jobs.is_empty() {
        return Ok(());
    }
    for job in &jobs {
        state.pending.insert(
            job.local_path.to_string_lossy().into_owned(),
            PendingKnownFolderUpload {
                remote_path: job.remote_path.clone(),
                snapshot: job.snapshot.clone(),
            },
        );
    }
    state.save(&paths.data_dir)?;

    let jobs = Arc::new(Mutex::new(jobs.into_iter()));
    let (result_tx, result_rx) = mpsc::channel();
    let concurrency = config.power.ac_upload_concurrency.max(1) as usize;
    thread::scope(|scope| -> anyhow::Result<()> {
        for _ in 0..concurrency {
            let jobs = Arc::clone(&jobs);
            let result_tx = result_tx.clone();
            scope.spawn(move || {
                loop {
                    let job = match jobs.lock() {
                        Ok(mut jobs) => jobs.next(),
                        Err(_) => return,
                    };
                    let Some(job) = job else {
                        return;
                    };
                    let result = upload_known_folder_file(paths, backend, db, &job);
                    let _ = result_tx.send((job, result));
                }
            });
        }
        drop(result_tx);

        for (job, result) in result_rx {
            let local_key = job.local_path.to_string_lossy().into_owned();
            match result {
                Ok(uploaded) => {
                    db.upsert_metadata(&uploaded)?;
                    state.files.insert(local_key.clone(), job.snapshot);
                    state.pending.remove(&local_key);
                }
                Err(err) => {
                    eprintln!(
                        "twodrive: known folder upload remains queued for {}: {err:#}",
                        job.local_path.display()
                    );
                }
            }
            state.save(&paths.data_dir)?;
        }
        Ok(())
    })?;
    Ok(())
}

fn upload_known_folder_file<B: CloudBackend>(
    paths: &AppPaths,
    backend: &B,
    db: &Database,
    job: &KnownFolderUploadJob,
) -> anyhow::Result<twodrive_core::MetadataEntry> {
    let local_path = &job.local_path;
    let remote_path = &job.remote_path;
    let snapshot = &job.snapshot;

    let name = local_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let mut activity = ActivityEntry::start(
        &paths.data_dir,
        "upload",
        remote_path,
        name,
        Some(snapshot.size),
    );
    let before = fs::metadata(local_path)?;
    if FileSnapshot::from_metadata(&before) != *snapshot {
        anyhow::bail!("local file changed before upload started");
    }
    if let Some(parent) = parent_cloud_path(remote_path) {
        ensure_remote_dir(backend, db, &parent)?;
    }
    let existing = db.get_by_path(remote_path)?;
    let uploaded = backend.upload_file_with_version(
        remote_path,
        local_path,
        existing
            .as_ref()
            .and_then(|record| record.cloud_remote_id.as_deref()),
        existing
            .as_ref()
            .map(|record| record.metadata.etag.as_str()),
        &mut |done, total| {
            activity.set_progress(done, Some(total));
            Ok(())
        },
    )?;
    let after = fs::metadata(local_path)?;
    let after_snapshot = FileSnapshot::from_metadata(&after);
    if after_snapshot != *snapshot {
        anyhow::bail!("local file changed while uploading");
    }
    activity.finish();
    Ok(uploaded)
}

fn ensure_remote_dir<B: CloudBackend>(
    backend: &B,
    db: &Database,
    remote_dir: &str,
) -> anyhow::Result<()> {
    let remote_dir = normalize_cloud_path(remote_dir);
    if remote_dir == "/" {
        return Ok(());
    }

    let mut current = String::from("/");
    for part in remote_dir.trim_matches('/').split('/') {
        current = join_cloud_path(&current, part);
        if db.get_by_path(&current)?.is_some() {
            continue;
        }
        match backend.create_folder(&current) {
            Ok(entry) => db.upsert_metadata(&entry)?,
            Err(err) => {
                eprintln!("twodrive: create folder skipped for {current}: {err:#}");
            }
        }
    }
    Ok(())
}

fn remote_path_for(root: &KnownFolderRoot, path: &Path) -> anyhow::Result<String> {
    let relative = path.strip_prefix(&root.local)?;
    let mut remote = root.remote.clone();
    for part in relative.components() {
        let value = part.as_os_str().to_string_lossy();
        if !value.is_empty() {
            remote = join_cloud_path(&remote, &value);
        }
    }
    Ok(normalize_cloud_path(&remote))
}

fn parent_cloud_path(path: &str) -> Option<String> {
    let path = normalize_cloud_path(path);
    let (parent, _) = path.rsplit_once('/')?;
    if parent.is_empty() {
        Some("/".to_string())
    } else {
        Some(parent.to_string())
    }
}

fn should_skip(path: &Path, config: &Config) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if matches!(name, "." | "..") {
        return true;
    }
    if name.starts_with(".") && name != "." {
        return true;
    }
    if name.starts_with("~$")
        || name.ends_with('~')
        || name.ends_with(".swp")
        || name.ends_with(".swo")
        || name.ends_with(".swx")
        || matches!(name, "Thumbs.db" | "desktop.ini" | ".DS_Store")
    {
        return true;
    }
    config
        .known_folders
        .exclude_suffixes
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

fn expand_home(value: &str) -> anyhow::Result<PathBuf> {
    if value == "~" || value.starts_with("~/") {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
        if value == "~" {
            return Ok(home);
        }
        return Ok(home.join(value.trim_start_matches("~/")));
    }
    Ok(PathBuf::from(value))
}

#[derive(Debug, Clone)]
struct KnownFolderRoot {
    local: PathBuf,
    remote: String,
}

#[derive(Debug, Clone)]
struct KnownFolderUploadJob {
    local_path: PathBuf,
    remote_path: String,
    snapshot: FileSnapshot,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct KnownFolderState {
    #[serde(default)]
    baseline_initialized: bool,
    #[serde(default)]
    files: HashMap<String, FileSnapshot>,
    #[serde(default)]
    pending: HashMap<String, PendingKnownFolderUpload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingKnownFolderUpload {
    remote_path: String,
    snapshot: FileSnapshot,
}

impl KnownFolderState {
    fn load(data_dir: &Path) -> anyhow::Result<Self> {
        let path = data_dir.join("known-folders-state.json");
        match fs::read_to_string(&path) {
            Ok(data) => Ok(serde_json::from_str(&data)?),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err.into()),
        }
    }

    fn save(&self, data_dir: &Path) -> anyhow::Result<()> {
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

    fn remove_path(&mut self, path: &Path) {
        let key = path.to_string_lossy();
        self.files.retain(|local_path, _| {
            local_path != key.as_ref() && !local_path.starts_with(&format!("{key}/"))
        });
        self.pending.retain(|local_path, _| {
            local_path != key.as_ref() && !local_path.starts_with(&format!("{key}/"))
        });
    }

    fn prune_missing_pending(&mut self) -> usize {
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
struct FileSnapshot {
    size: u64,
    modified_unix: i64,
}

impl FileSnapshot {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
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

#[derive(Debug)]
struct ActivityEntry {
    path: PathBuf,
    id: String,
    finished: bool,
}

impl ActivityEntry {
    fn start(
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

    fn set_progress(&mut self, bytes_done: u64, bytes_total: Option<u64>) {
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

    fn finish(&mut self) {
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

fn update_activity<F>(path: &Path, update: F) -> anyhow::Result<()>
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

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos}-{}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use twodrive_backend::{DeltaResult, MockBackend};
    use twodrive_core::{FileRecord, MetadataEntry};

    #[derive(Debug)]
    struct DelayedBackend {
        inner: MockBackend,
        active: AtomicUsize,
        max_active: AtomicUsize,
    }

    impl DelayedBackend {
        fn new() -> Self {
            Self {
                inner: MockBackend::new(),
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
            }
        }
    }

    impl CloudBackend for DelayedBackend {
        fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
            self.inner.list_all()
        }

        fn list_delta(&self, delta_link: Option<&str>) -> anyhow::Result<DeltaResult> {
            self.inner.list_delta(delta_link)
        }

        fn download(&self, remote_id: &str) -> anyhow::Result<Vec<u8>> {
            self.inner.download(remote_id)
        }

        fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry> {
            self.inner.upload(path, content)
        }

        fn upload_file_with_version(
            &self,
            path: &str,
            source_path: &Path,
            remote_id: Option<&str>,
            if_match: Option<&str>,
            on_progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
        ) -> anyhow::Result<MetadataEntry> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            thread::sleep(if path.contains("slow") {
                Duration::from_millis(800)
            } else {
                Duration::from_millis(100)
            });
            let result = self.inner.upload_file_with_version(
                path,
                source_path,
                remote_id,
                if_match,
                on_progress,
            );
            self.active.fetch_sub(1, Ordering::SeqCst);
            result
        }

        fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
            self.inner.create_folder(path)
        }

        fn rename(&self, remote_id: &str, new_path: &str) -> anyhow::Result<MetadataEntry> {
            self.inner.rename(remote_id, new_path)
        }

        fn delete(&self, remote_id: &str) -> anyhow::Result<()> {
            self.inner.delete(remote_id)
        }
    }

    fn test_paths(name: &str) -> (PathBuf, AppPaths) {
        let root = std::env::temp_dir().join(format!(
            "twodrive-daemon-{name}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let paths = AppPaths {
            config_dir: root.join("config"),
            config_path: root.join("config/config.toml"),
            data_dir: root.join("data"),
            cache_dir: root.join("data/cache"),
            db_path: root.join("data/test.sqlite3"),
            mount_dir: root.join("mount"),
            token_path: root.join("config/tokens.json"),
        };
        (root, paths)
    }

    #[test]
    fn restart_retries_changed_backup_without_caching_or_propagating_deletion() {
        let (root, paths) = test_paths("retry-changed-backup");
        paths.ensure().unwrap();
        let db = Database::new(paths.db_path.clone());
        db.init().unwrap();
        let backend = twodrive_backend::MockBackend::new();
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        let file = source.join("photo.bin");
        fs::write(&file, b"old").unwrap();
        let mut state = KnownFolderState::default();
        state.pending.insert(
            file.to_string_lossy().into_owned(),
            PendingKnownFolderUpload {
                remote_path: "/Pictures/photo.bin".into(),
                snapshot: FileSnapshot::from_metadata(&fs::metadata(&file).unwrap()),
            },
        );
        state.save(&paths.data_dir).unwrap();
        fs::write(&file, b"new content after interruption").unwrap();
        let mut state = KnownFolderState::load(&paths.data_dir).unwrap();
        retry_known_folder_uploads(&paths, &backend, &db, &Config::default(), &mut state).unwrap();
        assert!(state.pending.is_empty());
        let record = db.get_by_path("/Pictures/photo.bin").unwrap().unwrap();
        assert_eq!(
            backend
                .download(record.cloud_remote_id.as_deref().unwrap())
                .unwrap(),
            b"new content after interruption"
        );
        assert!(record.cache_path.is_none());
        assert_eq!(fs::read_dir(&paths.cache_dir).unwrap().count(), 0);
        fs::remove_file(&file).unwrap();
        sync_known_folder_root(
            &paths,
            &backend,
            &db,
            &Config::default(),
            &KnownFolderRoot {
                local: source,
                remote: "/Pictures".into(),
            },
            &mut state,
        )
        .unwrap();
        assert!(
            backend
                .get_metadata_by_path("/Pictures/photo.bin")
                .unwrap()
                .is_some()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn known_folder_state_persists_pending_uploads() {
        let (root, paths) = test_paths("pending-state");
        let mut state = KnownFolderState::default();
        state.pending.insert(
            "/local/a.txt".to_string(),
            PendingKnownFolderUpload {
                remote_path: "/remote/a.txt".to_string(),
                snapshot: FileSnapshot {
                    size: 12,
                    modified_unix: 34,
                },
            },
        );
        state.save(&paths.data_dir).unwrap();

        let loaded = KnownFolderState::load(&paths.data_dir).unwrap();
        let pending = loaded.pending.get("/local/a.txt").unwrap();
        assert_eq!(pending.remote_path, "/remote/a.txt");
        assert_eq!(pending.snapshot.size, 12);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_pending_known_folder_file_is_pruned() {
        let (root, paths) = test_paths("missing-pending");
        let missing = root.join("no-longer-present.txt");
        let key = missing.to_string_lossy().into_owned();
        let mut state = KnownFolderState::default();
        state.files.insert(
            key.clone(),
            FileSnapshot {
                size: 5,
                modified_unix: 10,
            },
        );
        state.pending.insert(
            key.clone(),
            PendingKnownFolderUpload {
                remote_path: "/Known/no-longer-present.txt".to_string(),
                snapshot: FileSnapshot {
                    size: 5,
                    modified_unix: 10,
                },
            },
        );

        assert_eq!(state.prune_missing_pending(), 1);
        assert!(!state.pending.contains_key(&key));
        assert!(!state.files.contains_key(&key));
        fs::remove_dir_all(paths.data_dir.parent().unwrap()).unwrap_or(());
    }

    #[test]
    fn first_start_baseline_preserves_tracked_snapshots_and_adds_history() {
        let (root, _paths) = test_paths("baseline");
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        let tracked = source.join("tracked.txt");
        let historical = source.join("historical.txt");
        fs::write(&tracked, b"changed since last upload").unwrap();
        fs::write(&historical, b"old local history").unwrap();

        let mut config = Config::default();
        config.known_folders.exclude_suffixes = vec![".tmp".to_string()];
        let mut state = KnownFolderState::default();
        let old_snapshot = FileSnapshot {
            size: 3,
            modified_unix: 4,
        };
        state
            .files
            .insert(tracked.to_string_lossy().into_owned(), old_snapshot.clone());

        baseline_known_folder_path(&config, &source, &mut state).unwrap();

        assert_eq!(
            state.files.get(&tracked.to_string_lossy().into_owned()),
            Some(&old_snapshot)
        );
        assert_eq!(
            state.files.get(&historical.to_string_lossy().into_owned()),
            Some(&FileSnapshot::from_metadata(
                &fs::metadata(&historical).unwrap()
            ))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn known_folder_small_files_upload_concurrently() {
        let (root, paths) = test_paths("parallel");
        paths.ensure().unwrap();
        let db = Database::new(paths.db_path.clone());
        db.init().unwrap();
        let backend = DelayedBackend::new();
        let mut config = Config::default();
        config.power.ac_upload_concurrency = 4;
        let mut state = KnownFolderState::default();
        let source_dir = root.join("source");
        fs::create_dir_all(&source_dir).unwrap();
        let mut jobs = Vec::new();
        for index in 0..4 {
            let local_path = source_dir.join(format!("small-{index}.txt"));
            fs::write(&local_path, format!("small-{index}")).unwrap();
            jobs.push(KnownFolderUploadJob {
                snapshot: FileSnapshot::from_metadata(&fs::metadata(&local_path).unwrap()),
                local_path,
                remote_path: format!("/Parallel/small-{index}.txt"),
            });
        }

        process_known_folder_uploads(&paths, &backend, &db, &config, jobs, &mut state).unwrap();

        assert!(state.pending.is_empty());
        assert_eq!(state.files.len(), 4);
        assert!(backend.max_active.load(Ordering::SeqCst) >= 2);
        for index in 0..4 {
            let record: FileRecord = db
                .get_by_path(&format!("/Parallel/small-{index}.txt"))
                .unwrap()
                .unwrap();
            assert_eq!(record.metadata.size, format!("small-{index}").len() as u64);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completed_known_folder_job_is_persisted_while_slower_job_runs() {
        let (root, paths) = test_paths("incremental-results");
        paths.ensure().unwrap();
        let db = Database::new(paths.db_path.clone());
        db.init().unwrap();
        let mut config = Config::default();
        config.power.ac_upload_concurrency = 2;
        let source_dir = root.join("source");
        fs::create_dir_all(&source_dir).unwrap();
        let fast_path = source_dir.join("fast.txt");
        let slow_path = source_dir.join("slow.txt");
        fs::write(&fast_path, b"fast").unwrap();
        fs::write(&slow_path, b"slow").unwrap();
        let jobs = vec![
            KnownFolderUploadJob {
                snapshot: FileSnapshot::from_metadata(&fs::metadata(&fast_path).unwrap()),
                local_path: fast_path.clone(),
                remote_path: "/Parallel/fast.txt".to_string(),
            },
            KnownFolderUploadJob {
                snapshot: FileSnapshot::from_metadata(&fs::metadata(&slow_path).unwrap()),
                local_path: slow_path.clone(),
                remote_path: "/Parallel/slow.txt".to_string(),
            },
        ];
        let worker_paths = paths.clone();
        let worker = thread::spawn(move || {
            let mut state = KnownFolderState::default();
            process_known_folder_uploads(
                &worker_paths,
                &DelayedBackend::new(),
                &db,
                &config,
                jobs,
                &mut state,
            )
            .map(|()| state)
        });

        let fast_key = fast_path.to_string_lossy().into_owned();
        let slow_key = slow_path.to_string_lossy().into_owned();
        let mut observed_incremental_commit = false;
        for _ in 0..25 {
            if let Ok(state) = KnownFolderState::load(&paths.data_dir)
                && state.files.contains_key(&fast_key)
                && !state.pending.contains_key(&fast_key)
                && state.pending.contains_key(&slow_key)
            {
                observed_incremental_commit = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }

        assert!(
            observed_incremental_commit,
            "the fast result should be durable before the slow upload finishes"
        );
        let final_state = worker.join().unwrap().unwrap();
        assert!(final_state.pending.is_empty());
        assert_eq!(final_state.files.len(), 2);
        fs::remove_dir_all(root).unwrap();
    }
}
