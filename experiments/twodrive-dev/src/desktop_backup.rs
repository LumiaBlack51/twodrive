//! Upload-only folder backups. Sources and completed remote files are never deleted.
use super::{Profile, State};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use twodrive_dev::{
    model::{CloudBackend, MetadataEntry},
    provider::{Backend, Progress},
};

const RECENT_LIMIT: usize = 20;

#[derive(Clone, Serialize, Deserialize)]
pub struct Folder {
    pub local: PathBuf,
    pub remote: String,
    pub enabled: bool,
}
#[derive(Default, Serialize, Deserialize)]
struct Record {
    hash: String,
    etag: Option<String>,
    intent: Option<String>,
    #[serde(default)]
    fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version_intent: Option<VersionIntent>,
}
#[derive(Serialize, Deserialize)]
struct VersionIntent {
    path: String,
    hash: String,
}
impl Record {
    fn is_confirmed(&self) -> bool {
        !self.hash.is_empty() && self.intent.is_none()
    }
}
#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    binding: String,
    files: BTreeMap<String, Record>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    retained_versions: BTreeMap<String, Record>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    recent: Vec<String>,
}
impl Manifest {
    fn confirmed_files(&self) -> usize {
        self.files
            .values()
            .filter(|record| record.is_confirmed())
            .count()
    }
}
fn directory(state: &State, p: &Profile) -> anyhow::Result<State> {
    State::new(state.dir.join(format!("backup-{}", p.id)))
}
pub fn folders(state: &State, p: &Profile) -> anyhow::Result<Vec<Folder>> {
    let s = directory(state, p)?;
    if s.dir.join("folders.json").exists() {
        s.read("folders.json")
    } else {
        Ok(vec![])
    }
}
pub fn save_folders(state: &State, p: &Profile, folders: &[Folder]) -> anyhow::Result<()> {
    directory(state, p)?.save("folders.json", &folders)
}
pub fn prepare_worker(state: &State, p: &Profile, binding: Option<&str>) -> anyhow::Result<bool> {
    reset_report(state, p, binding)?;
    Ok(folders(state, p)?.iter().any(|folder| folder.enabled))
}

pub fn reset_report(state: &State, p: &Profile, binding: Option<&str>) -> anyhow::Result<()> {
    let folders = folders(state, p)?;
    let count = folders.iter().filter(|f| f.enabled).count();
    let s = directory(state, p)?;
    let manifest = s
        .read::<Manifest>("manifest.json")
        .ok()
        .filter(|m| !m.binding.is_empty() && binding == Some(m.binding.as_str()));
    let recent = manifest
        .as_ref()
        .map(|m| restore_recent(&s, m, &folders))
        .unwrap_or_default();
    let confirmed_files = manifest
        .as_ref()
        .map(Manifest::confirmed_files)
        .unwrap_or(0);
    s.save(
        "report.json",
        &json!({"state":if count == 0 { "idle" } else { "starting" },"active":[],"recent":recent,"folders":count,
            "completed_scan":false,"confirmed_files":confirmed_files}),
    )
}

fn confirmed_item(path: &str, record: &Record, folders: &[Folder]) -> Option<Value> {
    if !record.is_confirmed() || path == "/" || twodrive_dev::provider::valid_path(path).is_err() {
        return None;
    }
    let name = path.rsplit('/').next()?;
    let mut item = json!({"name":name,"path":path,"kind":"upload","state":"已备份"});
    if let Some(version) = &record.version_path {
        item["path"] = json!(version);
        item["logical_path"] = json!(path);
    }
    if let Some((size, _)) = record.fingerprint.split_once(':')
        && let Ok(size) = size.parse::<u64>()
    {
        item["bytes_done"] = json!(size);
        item["bytes_total"] = json!(size);
    }
    if let Some((folder, relative)) = folders
        .iter()
        .filter_map(|folder| {
            let suffix = path.strip_prefix(&folder.remote)?.strip_prefix('/')?;
            let relative = Path::new(suffix);
            relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
                .then_some((folder, relative))
        })
        .max_by_key(|(folder, _)| folder.remote.len())
    {
        item["local_path"] = json!(folder.local.join(relative));
    }
    Some(item)
}

fn restore_recent(s: &State, manifest: &Manifest, folders: &[Folder]) -> Vec<Value> {
    let report = s.read::<Value>("report.json").unwrap_or_default();
    let reported = report["recent"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(logical_path);
    let mut seen = BTreeSet::new();
    let history: Vec<_> = manifest
        .recent
        .iter()
        .map(String::as_str)
        .chain(reported)
        .filter(|path| seen.insert(*path))
        .filter_map(|path| confirmed_item(path, manifest.files.get(path)?, folders))
        .take(RECENT_LIMIT)
        .collect();
    if !history.is_empty() {
        return history;
    }
    // Older manifests have confirmations but no completion order. Show a
    // bounded sample without inventing upload times.
    manifest
        .files
        .iter()
        .filter_map(|(path, record)| confirmed_item(path, record, folders))
        .take(RECENT_LIMIT)
        .collect()
}

fn recent_paths(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .filter_map(logical_path)
        .map(str::to_owned)
        .collect()
}
fn logical_path(item: &Value) -> Option<&str> {
    item["logical_path"]
        .as_str()
        .or_else(|| item["path"].as_str())
}
pub fn cleanup_spool(state: &State, p: &Profile) -> anyhow::Result<()> {
    let file = directory(state, p)?.dir.join("snapshot");
    if file.exists() {
        fs::remove_file(file)?;
    }
    Ok(())
}
pub fn validate(
    folders: &[Folder],
    p: &Profile,
    state: &State,
    protected: &[PathBuf],
) -> anyhow::Result<()> {
    ensure!(folders.len() <= 32, "最多选择 32 个备份文件夹");
    let home = PathBuf::from(
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .context("home missing")?,
    );
    let mut roots: Vec<(PathBuf, String)> = vec![];
    for folder in folders {
        if !folder.enabled {
            continue;
        }
        let source =
            twodrive_core::inspect_known_folder_source(&folder.local, &home, &p.mount_path);
        ensure!(
            source == twodrive_core::KnownFolderSourceState::Ready,
            "请选择可访问的本地文件夹；不能选择主目录、挂载目录或符号链接"
        );
        ensure!(
            !is_link(&fs::symlink_metadata(&folder.local)?),
            "不备份符号链接或重解析点"
        );
        let local = folder.local.canonicalize()?;
        ensure!(local.parent().is_some(), "不能备份整个磁盘根目录");
        let private = state.dir.canonicalize()?;
        ensure!(
            !local.starts_with(&private) && !private.starts_with(&local),
            "备份目录不能包含应用的私有状态"
        );
        for peer in protected {
            ensure!(
                !local.starts_with(peer) && !peer.starts_with(&local),
                "备份目录不能包含配对身份"
            );
        }
        let remote = &folder.remote;
        ensure!(
            remote.starts_with('/')
                && remote != "/"
                && !remote.ends_with('/')
                && !remote.contains(['\\', '\0'])
                && remote.trim() == remote
                && remote[1..]
                    .split('/')
                    .all(|v| !v.is_empty() && v != "." && v != ".."),
            "远端备份目录必须是有效的非根目录绝对路径"
        );
        for (a, b) in &roots {
            ensure!(
                !local.starts_with(a) && !a.starts_with(&local),
                "备份源不能互相包含"
            );
            ensure!(
                remote != b
                    && !remote.starts_with(&format!("{b}/"))
                    && !b.starts_with(&format!("{remote}/")),
                "远端备份目录不能互相包含"
            );
        }
        roots.push((local, remote.clone()));
    }
    Ok(())
}
pub fn report(state: &State, p: &Profile) -> anyhow::Result<Value> {
    let s = directory(state, p)?;
    Ok(s.read("report.json")
        .unwrap_or(json!({"state":"idle","active":[],"recent":[],"completed_scan":false})))
}
struct HashWriter(Sha256);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    metadata.is_symlink()
}
fn snapshot(source: &Path, target: &Path, paused: &Path) -> anyhow::Result<String> {
    let mut source_options = fs::OpenOptions::new();
    source_options.read(true);
    let mut target_options = fs::OpenOptions::new();
    target_options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        source_options.custom_flags(libc::O_NOFOLLOW);
        target_options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        source_options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
        target_options.custom_flags(0x00200000);
    }
    let mut input = source_options.open(source)?;
    let before = input.metadata()?;
    ensure!(before.is_file() && !before.is_symlink(), "仅备份普通文件");
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(before.file_attributes() & 0x400 == 0, "不备份重解析点");
    }
    let mut output = target_options.open(target)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        ensure!(!paused.exists(), "同步已暂停");
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        output.write_all(&buffer[..n])?;
        hash.update(&buffer[..n]);
    }
    output.sync_all()?;
    let after = input.metadata()?;
    ensure!(
        before.len() == after.len() && before.modified()? == after.modified()?,
        "文件正在修改，将在下一轮重试"
    );
    Ok(format!("{:x}", hash.finalize()))
}
fn ensure_directory(
    backend: &Backend,
    path: &str,
    mut checked: Option<&mut BTreeSet<String>>,
) -> anyhow::Result<()> {
    let mut current = String::new();
    for part in path.trim_matches('/').split('/') {
        current.push('/');
        current.push_str(part);
        if checked
            .as_ref()
            .is_some_and(|paths| paths.contains(&current))
        {
            continue;
        }
        match backend.get_metadata_by_path(&current)? {
            Some(m) => ensure!(m.is_dir, "远端路径已存在同名文件"),
            None => {
                backend.create_folder(&current)?;
            }
        }
        if let Some(paths) = &mut checked {
            paths.insert(current.clone());
        }
    }
    Ok(())
}
fn reconcile_intent(
    backend: &Backend,
    dest: &str,
    record: &mut Record,
    paused: &Path,
) -> anyhow::Result<()> {
    if let Some(intent) = &record.intent {
        // The previous process may have died after the provider committed upload.
        if let Some(meta) = backend.get_metadata_by_path(dest)?
            && Some(&meta.etag) != record.etag.as_ref()
        {
            let mut digest = HashWriter(Sha256::new());
            backend.download_path_to(dest, &mut digest, &mut |_| {
                ensure!(!paused.exists(), "同步已暂停");
                Ok(())
            })?;
            ensure!(
                &format!("{:x}", digest.0.finalize()) == intent,
                "远端版本发生冲突，未覆盖；请检查备份目标"
            );
            record.hash = intent.clone();
            record.etag = Some(meta.etag);
        }
        record.intent = None;
    }
    Ok(())
}

fn new_version_path(dest: &str) -> String {
    let (parent, name) = dest.rsplit_once('/').unwrap();
    let (stem, extension) = name
        .rsplit_once('.')
        .filter(|(stem, ext)| {
            !stem.is_empty()
                && ext.len() <= 20
                && !ext.is_empty()
                && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        })
        .map_or((name, ""), |(stem, _)| (stem, &name[stem.len()..]));
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let nonce: u128 = rand::random();
    let suffix = format!(".twodrive-{timestamp}-{nonce:032x}{extension}");
    let end = stem.floor_char_boundary(240 - suffix.len());
    format!("{parent}/{}{suffix}", &stem[..end])
}

fn verified_version(
    backend: &Backend,
    dest: &str,
    intent: &VersionIntent,
) -> anyhow::Result<Option<MetadataEntry>> {
    twodrive_dev::provider::valid_path(&intent.path)?;
    ensure!(
        intent.path != dest
            && intent.path.rsplit_once('/').map(|(parent, _)| parent)
                == dest.rsplit_once('/').map(|(parent, _)| parent),
        "备份版本路径不在目标目录中"
    );
    let Some(entry) = backend.get_metadata_by_path(&intent.path)? else {
        return Ok(None);
    };
    if entry.is_dir || entry.path != intent.path {
        return Ok(None);
    }
    let mut digest = HashWriter(Sha256::new());
    let bytes = backend.download_to(&entry.remote_id, &mut digest, &mut |_| Ok(()))?;
    Ok(
        (bytes == entry.size && format!("{:x}", digest.0.finalize()) == intent.hash)
            .then_some(entry),
    )
}

fn confirm_version(record: &mut Record, entry: &MetadataEntry) {
    let intent = record.version_intent.take().unwrap();
    record.hash = intent.hash;
    record.version_path = Some(intent.path);
    record.etag = Some(entry.etag.clone());
}

fn save_version(
    backend: &Backend,
    state: &State,
    manifest: &mut Manifest,
    dest: &str,
    spool: &Path,
    hash: &str,
    progress: Progress,
) -> anyhow::Result<Option<MetadataEntry>> {
    let record = manifest.files.entry(dest.to_owned()).or_default();
    let mut recovered = None;
    if let Some(intent) = &record.version_intent
        && let Some(entry) = verified_version(backend, dest, intent)?
    {
        confirm_version(record, &entry);
        recovered = Some(entry);
    }
    if record.hash == hash {
        record.version_intent = None;
        return Ok(recovered);
    }
    // Never write to the path of an uncertain or confirmed version. A late
    // commit can only produce a separate copy.
    let path = new_version_path(dest);
    record.version_intent = Some(VersionIntent {
        path: path.clone(),
        hash: hash.to_owned(),
    });
    state.save("manifest.json", manifest)?;
    backend.upload_backup_version(&path, spool, progress)?;
    let record = manifest.files.get_mut(dest).unwrap();
    let entry = verified_version(backend, dest, record.version_intent.as_ref().unwrap())?
        .context("备份版本内容未确认，请稍后重试")?;
    confirm_version(record, &entry);
    Ok(Some(entry))
}

fn resume_matching_create(
    record: &Record,
    source_hash: &str,
    resume: impl FnOnce() -> anyhow::Result<Option<twodrive_dev::model::MetadataEntry>>,
) -> anyhow::Result<Option<twodrive_dev::model::MetadataEntry>> {
    if record.etag.is_none() && record.intent.as_deref() == Some(source_hash) {
        resume()
    } else {
        Ok(None)
    }
}
pub fn run(
    state: &State,
    p: &Profile,
    backend: Backend,
    binding: String,
    protected: Vec<PathBuf>,
) -> anyhow::Result<()> {
    ensure!(backend.backup_supported(), "此连接不支持安全的文件夹备份");
    let s = directory(state, p)?;
    let _lock = s.run_lock()?;
    let paused = state.dir.join("paused");
    let versioned = p.option("backup_strategy") == "versioned" && backend.versioned_backup();
    ensure!(
        versioned || backend.backup_replace_supported(),
        "此连接只支持显式的版本备份"
    );
    let mut manifest: Manifest = if s.dir.join("manifest.json").exists() {
        s.read("manifest.json")?
    } else {
        Manifest::default()
    };
    ensure!(
        manifest.binding.is_empty() || manifest.binding == binding,
        "备份记录属于另一个服务器，不能重用"
    );
    manifest.binding = binding;
    if !versioned {
        ensure!(
            manifest.files.values().all(|r| r.version_intent.is_none()),
            "旧版本备份仍有未确认操作，请选择版本模式先恢复"
        );
        let paths: Vec<_> = manifest
            .files
            .iter()
            .filter(|(_, r)| r.version_path.is_some())
            .map(|(p, _)| p.clone())
            .collect();
        for path in paths {
            manifest
                .retained_versions
                .insert(path.clone(), manifest.files.remove(&path).unwrap());
        }
        s.save("manifest.json", &manifest)?;
    }
    let mut recent = restore_recent(&s, &manifest, &folders(state, p)?);
    manifest.recent = recent_paths(&recent);
    let mut confirmed_files = manifest.confirmed_files();
    let mut scanned = false;
    loop {
        let mut selected = folders(state, p)?;
        let count = selected.iter().filter(|f| f.enabled).count();
        let mut status = json!({"state":"scanning","active":[],"recent":recent,"folders":count,"completed_scan":scanned,"confirmed_files":confirmed_files});
        let result = (|| -> anyhow::Result<()> {
            if paused.exists() {
                status["state"] = json!("paused");
                return Ok(());
            }
            let policy = backend.sync_policy();
            let mut excluded = policy.canonical_backup_exclusions();
            excluded.extend(policy.exclude_directory_paths.iter().cloned());
            let is_excluded = |path: &Path| excluded.iter().any(|dir| path.starts_with(dir));
            status["unavailable_exclusion_paths"] = json!(
                policy
                    .exclude_directory_paths
                    .iter()
                    .filter(|p| !p.is_dir())
                    .collect::<Vec<_>>()
            );
            selected.retain(|f| {
                f.enabled && !is_excluded(&twodrive_dev::sync_policy::backup_path_key(&f.local))
            });
            validate(&selected, p, state, &protected)?;
            s.save("report.json", &status)?;
            let mut seen = 0;
            let mut checked_version_directories = BTreeSet::new();
            for folder in selected.iter().filter(|f| f.enabled) {
                let root = folder.local.canonicalize()?;
                if is_excluded(&root) {
                    continue;
                }
                let mut queue = vec![(root.clone(), folder.remote.clone())];
                while let Some((local, remote)) = queue.pop() {
                    ensure!(!paused.exists(), "同步已暂停");
                    if !versioned {
                        ensure_directory(&backend, &remote, None)?;
                    }
                    let mut has_content = false;
                    for item in fs::read_dir(&local)? {
                        let item = item?;
                        let kind = item.file_type()?;
                        if kind.is_symlink() || is_link(&fs::symlink_metadata(item.path())?) {
                            continue;
                        }
                        seen += 1;
                        ensure!(seen <= 100_000, "单轮备份超过 100000 项，请缩小备份范围");
                        let name = item
                            .file_name()
                            .into_string()
                            .map_err(|_| anyhow::anyhow!("文件名不是 UTF-8"))?;
                        if backend
                            .sync_policy()
                            .excludes_backup_entry(&name, kind.is_dir())
                        {
                            continue;
                        }
                        ensure!(!name.contains(['\\', '\0']), "文件名不能映射到远端路径");
                        let source = item.path().canonicalize()?;
                        ensure!(source.starts_with(&root), "备份路径离开所选目录");
                        if is_excluded(&source) {
                            continue;
                        }
                        let dest = format!("{remote}/{name}");
                        if kind.is_dir() {
                            has_content = true;
                            queue.push((source, dest));
                            continue;
                        }
                        if !kind.is_file() {
                            continue;
                        }
                        has_content = true;
                        let info = fs::metadata(&source)?;
                        let fingerprint = format!("{}:{:?}", info.len(), info.modified()?);
                        if manifest.files.get(&dest).is_some_and(|r| {
                            r.fingerprint == fingerprint
                                && r.intent.is_none()
                                && r.version_intent.is_none()
                        }) {
                            continue;
                        }
                        if versioned {
                            ensure_directory(
                                &backend,
                                &remote,
                                Some(&mut checked_version_directories),
                            )?;
                        }
                        let spool = s.dir.join("snapshot");
                        let hash = snapshot(&source, &spool, &paused)?;
                        let record = manifest.files.entry(dest.clone()).or_default();
                        let mut was_confirmed = record.is_confirmed();
                        let report_path = s.dir.clone();
                        let pause_path = paused.clone();
                        let mut active_status = status.clone();
                        active_status["recent"] = json!(
                            recent
                                .iter()
                                .filter(|item| logical_path(item) != Some(dest.as_str()))
                                .collect::<Vec<_>>()
                        );
                        active_status["confirmed_files"] =
                            json!(confirmed_files - usize::from(was_confirmed && !versioned));
                        let last = std::sync::Mutex::new(Instant::now() - Duration::from_secs(1));
                        let transfer = json!({
                            "name":name,"kind":"upload","source":"backup",
                            "revision":p.revision,"path":dest,"local_path":source,
                        });
                        let active_transfer = transfer.clone();
                        active_status["state"] = json!("uploading");
                        let progress: Progress = Arc::new(move |done, total| {
                            ensure!(!pause_path.exists(), "同步已暂停");
                            let mut last = last.lock().unwrap();
                            if done == 0
                                || done == total
                                || last.elapsed() > Duration::from_millis(200)
                            {
                                let mut report = active_status.clone();
                                let mut item = active_transfer.clone();
                                item["bytes_done"] = json!(done);
                                item["bytes_total"] = json!(total);
                                report["active"] = json!([item]);
                                State::new(report_path.clone())?.save("report.json", &report)?;
                                *last = Instant::now();
                            }
                            Ok(())
                        });
                        let uploaded = if versioned {
                            save_version(
                                &backend,
                                &s,
                                &mut manifest,
                                &dest,
                                &spool,
                                &hash,
                                progress,
                            )?
                        } else {
                            let mut uploaded = resume_matching_create(record, &hash, || {
                                if backend.capabilities().backup() {
                                    backend.resume_create_upload(&dest, &spool, progress.clone())
                                } else {
                                    Ok(None)
                                }
                            })?;
                            if uploaded.is_none() {
                                reconcile_intent(&backend, &dest, record, &paused)?;
                                if record.hash != hash {
                                    let etag = record.etag.clone();
                                    record.intent = Some(hash.clone());
                                    if was_confirmed {
                                        confirmed_files -= 1;
                                        was_confirmed = false;
                                        recent.retain(|item| {
                                            logical_path(item) != Some(dest.as_str())
                                        });
                                        manifest.recent = recent_paths(&recent);
                                    }
                                    s.save("manifest.json", &manifest)?;
                                    uploaded = Some(backend.upload_backup_path(
                                        &dest,
                                        &spool,
                                        etag.as_deref(),
                                        progress,
                                    )?);
                                }
                            }
                            uploaded
                        };
                        let record = manifest.files.get_mut(&dest).unwrap();
                        let uploaded_size = uploaded.map(|uploaded| {
                            if !versioned {
                                record.hash = hash;
                                record.etag = Some(uploaded.etag);
                                record.intent = None;
                            }
                            uploaded.size
                        });
                        record.fingerprint = fingerprint;
                        let newly_confirmed = !was_confirmed && record.is_confirmed();
                        if newly_confirmed {
                            confirmed_files += 1;
                        }
                        if let Some(size) =
                            uploaded_size.or_else(|| newly_confirmed.then_some(info.len()))
                        {
                            let mut item = transfer;
                            if let Some(path) = &record.version_path {
                                item["path"] = json!(path);
                                item["logical_path"] = json!(&dest);
                            }
                            item["state"] = json!("已备份");
                            item["bytes_done"] = json!(size);
                            item["bytes_total"] = json!(size);
                            recent.retain(|item| logical_path(item) != Some(dest.as_str()));
                            recent.insert(0, item);
                            recent.truncate(RECENT_LIMIT);
                            manifest.recent = recent_paths(&recent);
                        }
                        s.save("manifest.json", &manifest)?;
                        fs::remove_file(spool)?;
                    }
                    if versioned && !has_content {
                        ensure_directory(
                            &backend,
                            &remote,
                            Some(&mut checked_version_directories),
                        )?;
                    }
                }
            }
            scanned = true;
            status["state"] = json!("idle");
            Ok(())
        })();
        // Recovery can confirm an older version before a newer upload fails.
        // Reconcile even on errors, before the next scan sees it as confirmed.
        confirmed_files = manifest.confirmed_files();
        status["active"] = json!([]);
        status["recent"] = json!(recent);
        status["confirmed_files"] = json!(confirmed_files);
        status["completed_scan"] = json!(scanned);
        if let Err(error) = result {
            status["state"] = json!(if paused.exists() { "paused" } else { "error" });
            status["error"] = json!(if paused.exists() {
                "同步已暂停".to_owned()
            } else {
                error.to_string()
            });
        }
        s.save("report.json", &status)?;
        cleanup_spool(state, p)?;
        thread::sleep(Duration::from_secs(10));
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_provider::OpaqueProvider;
    use super::*;
    use twodrive_dev::provider::Provider;

    #[test]
    fn backup_report_is_idle_when_no_task_can_start() {
        let temp = tempfile::tempdir().unwrap();
        let state = State::new(temp.path().join("state")).unwrap();
        let profile = Profile {
            id: "empty".into(),
            ..Default::default()
        };
        reset_report(&state, &profile, None).unwrap();
        let report = report(&state, &profile).unwrap();
        assert_eq!(report["state"], "idle");
        assert_eq!(report["folders"], 0);
    }

    #[test]
    fn reset_preserves_confirmed_recent_order_for_the_bound_account() {
        let temp = tempfile::tempdir().unwrap();
        let state = State::new(temp.path().join("state")).unwrap();
        let profile = Profile {
            id: "synthetic".into(),
            ..Default::default()
        };
        let dir = directory(&state, &profile).unwrap();
        let local = temp.path().join("source");
        save_folders(
            &state,
            &profile,
            &[Folder {
                local: local.clone(),
                remote: "/backup".into(),
                enabled: true,
            }],
        )
        .unwrap();
        dir.save(
            "manifest.json",
            &json!({"binding":"bound","recent":["/backup/last.txt","/backup/older.txt"],
                "files":{"/backup/older.txt":{"hash":"confirmed","etag":"v1","intent":null},
                    "/backup/last.txt":{"hash":"confirmed","etag":"v2","intent":null,"fingerprint":"4:local"}}}),
        )
        .unwrap();
        dir.save(
            "report.json",
            &json!({"active":[{"name":"stopped upload"}],"recent":[]}),
        )
        .unwrap();
        assert!(prepare_worker(&state, &profile, Some("bound")).unwrap());
        let started = report(&state, &profile).unwrap();
        assert_eq!(started["confirmed_files"], 2);
        assert_eq!(started["active"], json!([]));
        assert_eq!(
            recent_paths(started["recent"].as_array().unwrap()),
            ["/backup/last.txt", "/backup/older.txt"],
        );
        assert_eq!(started["recent"][0]["bytes_total"], 4);
        assert_eq!(
            started["recent"][0]["local_path"],
            json!(local.join("last.txt"))
        );

        reset_report(&state, &profile, Some("other")).unwrap();
        let different = report(&state, &profile).unwrap();
        assert_eq!(different["confirmed_files"], 0);
        assert_eq!(different["recent"], json!([]));
    }

    #[test]
    fn legacy_manifest_recovers_only_confirmed_backups() {
        let temp = tempfile::tempdir().unwrap();
        let dir = State::new(temp.path().join("state")).unwrap();
        let manifest: Manifest = serde_json::from_value(json!({
            "binding":"bound",
            "files":{
                "/backup/complete.txt":{"hash":"confirmed","etag":"v1","intent":null},
                "/backup/pending.txt":{"hash":"","etag":null,"intent":"planned"},
                "/backup/replacing.txt":{"hash":"old-version","etag":"v1","intent":"replacement"},
            }
        }))
        .unwrap();
        dir.save(
            "report.json",
            &json!({"recent":[{"path":"/backup/replacing.txt"}]}),
        )
        .unwrap();
        let restored = restore_recent(&dir, &manifest, &[]);
        assert_eq!(manifest.confirmed_files(), 1);
        assert_eq!(recent_paths(&restored), ["/backup/complete.txt"]);
        assert_eq!(restored[0]["state"], "已备份");
        assert!(restored[0].get("completed_at").is_none());
        assert!(restored[0].get("local_path").is_none());
    }

    #[test]
    fn disabled_backup_stays_idle_on_connect_and_when_the_last_source_is_removed() {
        let temp = tempfile::tempdir().unwrap();
        let state = State::new(temp.path().join("state")).unwrap();
        let profile = Profile {
            id: "fixture".into(),
            ..Default::default()
        };
        let mut source = Folder {
            local: temp.path().join("source"),
            remote: "/saved".into(),
            enabled: false,
        };
        for sources in [vec![], vec![source.clone()]] {
            save_folders(&state, &profile, &sources).unwrap();
            assert!(!prepare_worker(&state, &profile, None).unwrap());
            let idle = report(&state, &profile).unwrap();
            assert_eq!(idle["state"], "idle");
            assert_eq!(idle["folders"], 0);
        }
        source.enabled = true;
        save_folders(&state, &profile, &[source]).unwrap();
        assert!(prepare_worker(&state, &profile, None).unwrap());
        assert_eq!(report(&state, &profile).unwrap()["state"], "starting");
        save_folders(&state, &profile, &[]).unwrap();
        assert!(!prepare_worker(&state, &profile, None).unwrap());
        assert_eq!(report(&state, &profile).unwrap()["state"], "idle");
    }

    #[test]
    fn only_unchanged_create_intents_can_resume_the_owned_session() {
        let record = Record {
            intent: Some("original-digest".into()),
            ..Default::default()
        };
        let entry = twodrive_dev::model::MetadataEntry {
            remote_id: "opaque-resumed".into(),
            path: "/backup/file".into(),
            parent_path: "/backup".into(),
            name: "file".into(),
            is_dir: false,
            size: 4,
            modified_unix: 0,
            created_unix: None,
            etag: "completed".into(),
        };
        assert!(
            resume_matching_create(&record, "original-digest", || Ok(Some(entry)))
                .unwrap()
                .is_some()
        );
        for (record, digest) in [
            (record, "changed-source"),
            (
                Record {
                    intent: Some("original-digest".into()),
                    etag: Some("old-version".into()),
                    ..Default::default()
                },
                "original-digest",
            ),
            (Record::default(), "original-digest"),
        ] {
            assert!(
                resume_matching_create(&record, digest, || panic!(
                    "unverified source/version resumed"
                ))
                .unwrap()
                .is_none()
            );
        }
    }
    #[test]
    fn backup_recovery_uses_opaque_ids_and_preserves_conflicts() {
        let p = OpaqueProvider::default();
        let entry = p.upload("/backup/file", b"committed".to_vec()).unwrap();
        let backend = Backend::new(p);
        let temp = tempfile::tempdir().unwrap();
        let pause = temp.path().join("paused");
        let hash = format!("{:x}", Sha256::digest(b"committed"));
        let mut record = Record {
            intent: Some(hash.clone()),
            etag: Some("old".into()),
            ..Default::default()
        };
        reconcile_intent(&backend, "/backup/file", &mut record, &pause).unwrap();
        assert_eq!(record.hash, hash);
        assert_eq!(record.etag.as_deref(), Some(entry.etag.as_str()));
        assert!(record.intent.is_none());
        record.intent = Some(format!("{:x}", Sha256::digest(b"uncommitted")));
        record.etag = Some("old".into());
        assert!(reconcile_intent(&backend, "/backup/file", &mut record, &pause).is_err());
        assert!(record.intent.is_some());
        assert_eq!(backend.download(&entry.remote_id).unwrap(), b"committed");
    }
    #[test]
    fn recovered_version_stays_counted_when_the_changed_source_upload_fails() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path().join("state")).unwrap();
        let profile = Profile {
            id: "recovery-count".into(),
            options: [("backup_strategy".into(), json!("versioned"))].into(),
            mount_path: root.path().join("mount"),
            ..Default::default()
        };
        let dir = directory(&state, &profile).unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), b"changed source").unwrap();
        save_folders(
            &state,
            &profile,
            &[Folder {
                local: source,
                remote: "/backup".into(),
                enabled: true,
            }],
        )
        .unwrap();
        let mut provider = OpaqueProvider::default();
        provider.versioned = true;
        provider.caps.conditional_upload = false;
        provider.create_folder("/backup").unwrap();
        let dest = "/backup/file.txt";
        let previous = provider
            .upload(&new_version_path(dest), b"previous source".to_vec())
            .unwrap();
        dir.save(
            "manifest.json",
            &json!({
                "binding": "synthetic-binding",
                "files": {dest: {
                    "hash": "", "etag": null, "intent": null,
                    "version_intent": {
                        "path": previous.path,
                        "hash": format!("{:x}", Sha256::digest(b"previous source")),
                    },
                }},
            }),
        )
        .unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let calls = attempts.clone();
        let stop_config = dir.dir.join("folders.json");
        provider.before_mutation = Some(Arc::new(move || {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                anyhow::bail!("synthetic upload failure");
            }
            // After the successful retry, stop the actual worker at its next
            // configuration read, without adding a production test hook.
            fs::write(&stop_config, b"invalid test configuration")?;
            Ok(())
        }));
        let backend = Backend::new(provider.clone());
        let worker = thread::spawn(move || {
            run(
                &state,
                &profile,
                backend,
                "synthetic-binding".into(),
                vec![],
            )
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        let failed = loop {
            let report = dir.read::<Value>("report.json").unwrap_or_default();
            if report["state"] == "error" || Instant::now() >= deadline || worker.is_finished() {
                break report;
            }
            thread::sleep(Duration::from_millis(10));
        };
        while !worker.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if !worker.is_finished() {
            fs::write(dir.dir.join("folders.json"), b"invalid test configuration").unwrap();
        }
        // Join before assertions so the fixture cannot outlive this test.
        assert!(worker.join().unwrap().is_err());
        let finished: Value = dir.read("report.json").unwrap();
        let manifest: Manifest = dir.read("manifest.json").unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(failed["error"], "synthetic upload failure");
        assert_eq!(failed["confirmed_files"], 1);
        assert_eq!(finished["state"], "idle");
        assert_eq!(finished["confirmed_files"], 1);
        assert_eq!(manifest.confirmed_files(), 1);
        assert_eq!(finished["recent"][0]["logical_path"], dest);
        assert_eq!(
            provider.download(&previous.remote_id).unwrap(),
            b"previous source"
        );
        let latest = provider
            .get_metadata_by_path(manifest.files[dest].version_path.as_ref().unwrap())
            .unwrap()
            .unwrap();
        assert_ne!(latest.path, previous.path);
        assert_eq!(
            provider.download(&latest.remote_id).unwrap(),
            b"changed source"
        );
    }

    #[test]
    fn default_backup_updates_original_path_even_when_versions_are_supported() {
        let temp = tempfile::tempdir().unwrap();
        let state = State::new(temp.path().join("state")).unwrap();
        let profile = Profile {
            id: "default-recoverable".into(),
            mount_path: temp.path().join("mount"),
            ..Default::default()
        };
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), b"new content").unwrap();
        save_folders(
            &state,
            &profile,
            &[Folder {
                local: source,
                remote: "/backup".into(),
                enabled: true,
            }],
        )
        .unwrap();
        let directory = directory(&state, &profile).unwrap();
        let mut provider = OpaqueProvider::default();
        provider.recoverable = true;
        provider.versioned = true;
        provider.caps.conditional_upload = false;
        provider.create_folder("/backup").unwrap();
        let old = provider
            .upload("/backup/file.txt", b"old content".to_vec())
            .unwrap();
        directory.save("manifest.json",&json!({"binding":"fixture","files":{"/backup/file.txt":{"hash":"previous","etag":old.etag,"intent":null}}})).unwrap();
        let stop = directory.dir.join("folders.json");
        provider.before_mutation = Some(Arc::new(move || {
            fs::write(&stop, b"stop disposable worker")?;
            Ok(())
        }));
        assert!(
            run(
                &state,
                &profile,
                Backend::new(provider.clone()),
                "fixture".into(),
                vec![]
            )
            .is_err()
        );
        let entries = provider.list_directory("/backup").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "/backup/file.txt");
        assert_eq!(entries[0].remote_id, old.remote_id);
        assert_eq!(provider.download(&old.remote_id).unwrap(), b"new content");
        let manifest: Manifest = directory.read("manifest.json").unwrap();
        assert!(manifest.files["/backup/file.txt"].version_path.is_none());
        assert!(manifest.files["/backup/file.txt"].is_confirmed());
    }

    #[test]
    fn backup_versions_recover_lost_confirmation_and_preserve_conflicting_cloud_files() {
        let mut provider = OpaqueProvider::default();
        provider.versioned = true;
        provider.caps.conditional_upload = false;
        let backend = Backend::new(provider.clone());
        assert!(backend.backup_supported());
        assert!(!backend.capabilities().conditional_upload);
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path().join("state")).unwrap();
        let spool = root.path().join("spool");
        let dest = "/backup/文档.txt";
        let original = provider.upload(dest, b"cloud original".to_vec()).unwrap();
        let mut manifest = Manifest::default();
        let progress: Progress = Arc::new(|_, _| Ok(()));

        fs::write(&spool, b"version one").unwrap();
        let first_hash = format!("{:x}", Sha256::digest(b"version one"));
        let first = save_version(
            &backend,
            &state,
            &mut manifest,
            dest,
            &spool,
            &first_hash,
            progress.clone(),
        )
        .unwrap()
        .unwrap();
        assert_ne!(first.path, dest);
        assert!(first.path.ends_with(".txt"));

        // Disk still contains the intent written before the successful upload.
        let mut manifest: Manifest = state.read("manifest.json").unwrap();
        let recovered = save_version(
            &backend,
            &state,
            &mut manifest,
            dest,
            &spool,
            &first_hash,
            progress.clone(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(recovered.remote_id, first.remote_id);
        assert_eq!(provider.list_all().unwrap().len(), 2);

        let conflict_path = new_version_path(dest);
        let competing = provider
            .upload(&conflict_path, b"cloud concurrent edit".to_vec())
            .unwrap();
        fs::write(&spool, b"version two").unwrap();
        let second_hash = format!("{:x}", Sha256::digest(b"version two"));
        manifest.files.get_mut(dest).unwrap().version_intent = Some(VersionIntent {
            path: conflict_path,
            hash: second_hash.clone(),
        });
        let second = save_version(
            &backend,
            &state,
            &mut manifest,
            dest,
            &spool,
            &second_hash,
            progress,
        )
        .unwrap()
        .unwrap();
        assert_ne!(second.path, first.path);
        assert_ne!(second.path, competing.path);
        assert_eq!(
            provider.download(&original.remote_id).unwrap(),
            b"cloud original"
        );
        assert_eq!(
            provider.download(&competing.remote_id).unwrap(),
            b"cloud concurrent edit"
        );
        assert_eq!(provider.download(&first.remote_id).unwrap(), b"version one");
        assert_eq!(
            provider.download(&second.remote_id).unwrap(),
            b"version two"
        );
        let item = confirmed_item(dest, &manifest.files[dest], &[]).unwrap();
        assert_eq!(item["path"], second.path);
        assert_eq!(recent_paths(&[item]), [dest]);
    }
}
