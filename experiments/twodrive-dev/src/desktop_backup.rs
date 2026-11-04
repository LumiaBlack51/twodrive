//! Upload-only folder backups. Sources and completed remote files are never deleted.
use super::{Profile, State};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use twodrive_dev::{model::CloudBackend, provider::Backend};

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
}
#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    binding: String,
    files: BTreeMap<String, Record>,
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
pub fn reset_report(state: &State, p: &Profile) -> anyhow::Result<()> {
    let count = folders(state, p)?.iter().filter(|f| f.enabled).count();
    directory(state, p)?.save(
        "report.json",
        &json!({"state":"starting","active":[],"recent":[],"folders":count,"completed_scan":false}),
    )
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
fn ensure_directory(backend: &Backend, path: &str) -> anyhow::Result<()> {
    let mut current = String::new();
    for part in path.trim_matches('/').split('/') {
        current.push('/');
        current.push_str(part);
        match backend.get_metadata_by_path(&current)? {
            Some(m) => ensure!(m.is_dir, "远端路径已存在同名文件"),
            None => {
                backend.create_folder(&current)?;
            }
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
    ensure!(
        backend.capabilities().backup(),
        "此连接不支持安全的文件夹备份"
    );
    let s = directory(state, p)?;
    let _lock = s.run_lock()?;
    let paused = state.dir.join("paused");
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
    let mut recent: Vec<Value> = vec![];
    let mut scanned = false;
    loop {
        let selected = folders(state, p)?;
        let count = selected.iter().filter(|f| f.enabled).count();
        let mut status = json!({"state":"scanning","active":[],"recent":recent,"folders":count,"completed_scan":scanned});
        let result = (|| -> anyhow::Result<()> {
            if paused.exists() {
                status["state"] = json!("paused");
                return Ok(());
            }
            validate(&selected, p, state, &protected)?;
            s.save("report.json", &status)?;
            let mut seen = 0;
            for folder in selected.iter().filter(|f| f.enabled) {
                let root = folder.local.canonicalize()?;
                let mut queue = vec![(root.clone(), folder.remote.clone())];
                while let Some((local, remote)) = queue.pop() {
                    ensure!(!paused.exists(), "同步已暂停");
                    ensure_directory(&backend, &remote)?;
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
                        ensure!(!name.contains(['\\', '\0']), "文件名不能映射到远端路径");
                        let source = item.path().canonicalize()?;
                        ensure!(source.starts_with(&root), "备份路径离开所选目录");
                        let dest = format!("{remote}/{name}");
                        if kind.is_dir() {
                            queue.push((source, dest));
                            continue;
                        }
                        if !kind.is_file() {
                            continue;
                        }
                        let info = fs::metadata(&source)?;
                        let fingerprint = format!("{}:{:?}", info.len(), info.modified()?);
                        if manifest
                            .files
                            .get(&dest)
                            .is_some_and(|r| r.fingerprint == fingerprint && r.intent.is_none())
                        {
                            continue;
                        }
                        let spool = s.dir.join("snapshot");
                        let hash = snapshot(&source, &spool, &paused)?;
                        let record = manifest.files.entry(dest.clone()).or_default();
                        let report_path = s.dir.clone();
                        let pause_path = paused.clone();
                        let mut active_status = status.clone();
                        active_status["recent"] = json!(recent);
                        let last = std::sync::Mutex::new(Instant::now() - Duration::from_secs(1));
                        let display = name.clone();
                        active_status["state"] = json!("uploading");
                        let progress: twodrive_dev::provider::Progress = Arc::new(
                            move |done, total| {
                                ensure!(!pause_path.exists(), "同步已暂停");
                                let mut last = last.lock().unwrap();
                                if done == 0
                                    || done == total
                                    || last.elapsed() > Duration::from_millis(200)
                                {
                                    let mut report = active_status.clone();
                                    report["active"] = json!([{"name":display,"kind":"upload","bytes_done":done,"bytes_total":total}]);
                                    State::new(report_path.clone())?
                                        .save("report.json", &report)?;
                                    *last = Instant::now();
                                }
                                Ok(())
                            },
                        );
                        let mut uploaded = resume_matching_create(record, &hash, || {
                            backend.resume_create_upload(&dest, &spool, progress.clone())
                        })?;
                        if uploaded.is_none() {
                            reconcile_intent(&backend, &dest, record, &paused)?;
                            if record.hash != hash {
                                let etag = record.etag.clone();
                                record.intent = Some(hash.clone());
                                s.save("manifest.json", &manifest)?;
                                uploaded = Some(backend.upload_path(
                                    &dest,
                                    &spool,
                                    etag.as_deref(),
                                    progress,
                                )?);
                            }
                        }
                        if let Some(uploaded) = uploaded {
                            let record = manifest.files.get_mut(&dest).unwrap();
                            record.hash = hash;
                            record.etag = Some(uploaded.etag);
                            record.intent = None;
                            recent.insert(0, json!({"name":name,"kind":"upload","state":"已备份","bytes_done":uploaded.size,"bytes_total":uploaded.size}));
                            recent.truncate(20);
                        }
                        manifest.files.get_mut(&dest).unwrap().fingerprint = fingerprint;
                        s.save("manifest.json", &manifest)?;
                        fs::remove_file(spool)?;
                    }
                }
            }
            scanned = true;
            status["state"] = json!("idle");
            Ok(())
        })();
        status["active"] = json!([]);
        status["recent"] = json!(recent);
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
}
