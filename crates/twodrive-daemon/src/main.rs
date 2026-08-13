use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
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

    if config.known_folders.startup_scan {
        for root in &roots {
            sync_known_folder_root(&paths, &backend, &db, &config, root, &mut state)?;
        }
        state.save(&paths.data_dir)?;
    }

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

    let mut pending = HashSet::new();
    loop {
        match recv_watch_wake(&rx, rescan_interval) {
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

fn sync_known_folder_paths(
    paths: &AppPaths,
    backend: &GraphBackend,
    db: &Database,
    config: &Config,
    roots: &[KnownFolderRoot],
    pending: &HashSet<PathBuf>,
    state: &mut KnownFolderState,
) -> anyhow::Result<()> {
    for path in pending {
        let Some(root) = roots.iter().find(|root| path.starts_with(&root.local)) else {
            continue;
        };
        if !path.exists() {
            state.remove_path(path);
            continue;
        }
        sync_local_path(paths, backend, db, config, root, path, state)?;
    }
    Ok(())
}

fn sync_known_folder_root(
    paths: &AppPaths,
    backend: &GraphBackend,
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
    sync_local_path(paths, backend, db, config, root, &root.local, state)
}

fn sync_local_path(
    paths: &AppPaths,
    backend: &GraphBackend,
    db: &Database,
    config: &Config,
    root: &KnownFolderRoot,
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
        let remote_dir = remote_path_for(root, path)?;
        ensure_remote_dir(backend, db, &remote_dir)?;
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            sync_local_path(paths, backend, db, config, root, &entry.path(), state)?;
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
    upload_known_folder_file(paths, backend, db, path, &remote_path, snapshot, state)
}

fn upload_known_folder_file(
    paths: &AppPaths,
    backend: &GraphBackend,
    db: &Database,
    local_path: &Path,
    remote_path: &str,
    snapshot: FileSnapshot,
    state: &mut KnownFolderState,
) -> anyhow::Result<()> {
    if let Some(parent) = parent_cloud_path(remote_path) {
        ensure_remote_dir(backend, db, &parent)?;
    }

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
    let content = fs::read(local_path)?;
    activity.set_progress(content.len() as u64, Some(snapshot.size));

    let after = fs::metadata(local_path)?;
    let after_snapshot = FileSnapshot::from_metadata(&after);
    if after_snapshot != snapshot {
        eprintln!(
            "twodrive: known folder file changed while reading; will retry later: {}",
            local_path.display()
        );
        return Ok(());
    }

    let uploaded = backend.upload(remote_path, content)?;
    db.upsert_metadata(&uploaded)?;
    state
        .files
        .insert(local_path.to_string_lossy().into_owned(), snapshot);
    activity.finish();
    Ok(())
}

fn ensure_remote_dir(
    backend: &GraphBackend,
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

#[derive(Debug, Default, Serialize, Deserialize)]
struct KnownFolderState {
    files: HashMap<String, FileSnapshot>,
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
        fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    fn remove_path(&mut self, path: &Path) {
        let key = path.to_string_lossy();
        self.files.retain(|local_path, _| {
            local_path != key.as_ref() && !local_path.starts_with(&format!("{key}/"))
        });
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
    fs::write(path, serde_json::to_string_pretty(&snapshot)?)?;
    Ok(())
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos}-{}", std::process::id())
}
