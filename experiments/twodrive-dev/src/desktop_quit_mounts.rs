use super::*;
use std::{
    collections::{BTreeMap, HashSet},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::fs::MetadataExt,
    },
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Identity {
    pid: u32,
    start_time: u64,
}

struct ProcessInfo {
    identity: Identity,
    parent: u32,
    name: String,
}

struct Holder {
    name: String,
    can_close: bool,
}

type Holders = BTreeMap<Identity, Holder>;

struct Mount {
    name: String,
    path: PathBuf,
    root: PathBuf,
    id: Option<String>,
}

impl Mount {
    fn still_mounted(&self) -> bool {
        if let Some(id) = &self.id {
            return linux_twodrive_fuse_mount_id(&self.path)
                .is_ok_and(|current| current.as_ref() == Some(id));
        }
        self.root == self.path || fs::read_link(&self.path).is_ok_and(|path| path == self.root)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("「{name}」挂载仍在使用（{}），请关闭相关文件、窗口或终端后重试", path.display())]
pub(super) struct BusyMount {
    pub name: String,
    pub path: PathBuf,
}

impl BusyMount {
    fn reply(&self) -> Value {
        json!({"quit_blockers":[{"name":self.name,"path":self.path,"programs":[]}]})
    }
}

pub(super) fn blockers_after_failed_disconnect(
    error: &anyhow::Error,
    inspect: impl FnOnce() -> anyhow::Result<Option<Value>>,
) -> anyhow::Result<Option<Value>> {
    Ok(inspect()?.or_else(|| error.downcast_ref::<BusyMount>().map(BusyMount::reply)))
}

fn mounted_roots(app: &App) -> Vec<Mount> {
    app.mounted_profiles()
        .filter_map(|profile| {
            let path = profile.mount_path.clone();
            let (root, id) = match linux_twodrive_fuse_mount_id(&path).ok()? {
                Some(id) => (path.clone(), Some(id)),
                _ => (gvfs_export(app, profile)?, None),
            };
            Some(Mount {
                name: profile.name.clone(),
                path,
                root,
                id,
            })
        })
        .collect()
}

fn gvfs_export(app: &App, profile: &Profile) -> Option<PathBuf> {
    if profile.kind != "icloud" {
        return None;
    }
    let root = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join("gvfs");
    let export = fs::read_link(&profile.mount_path).ok()?;
    if export.parent()? != root || linux_mount_info(&root).ok().flatten()?.1 != "fuse.gvfsd-fuse" {
        return None;
    }
    // GVfs shares a single kernel mount across accounts. Match just this
    // worker's authenticated export, never the GVfs mount ID.
    let state = app
        .mount_state(profile)
        .join("linux-readonly/gnome-webdav.json");
    let owner: Value = serde_json::from_slice(&fs::read(state).ok()?).ok()?;
    let session = app.saved_session(profile)?;
    (owner["revision"].as_str()? == profile.revision
        && owner["binding"].as_str()? == session["binding"].as_str()?
        && Path::new(owner["path"].as_str()?) == profile.mount_path
        && Path::new(owner["export"].as_str()?) == export)
        .then_some(export)
}

fn process_info(pid: u32) -> Option<ProcessInfo> {
    let proc = PathBuf::from(format!("/proc/{pid}"));
    if fs::metadata(&proc).ok()?.uid() != unsafe { libc::geteuid() } {
        return None;
    }
    let raw = fs::read_to_string(proc.join("stat")).ok()?;
    let (_, fields) = raw.rsplit_once(") ")?;
    let fields: Vec<_> = fields.split_whitespace().collect();
    let start_time = fields.get(19)?.parse().ok()?;
    let name = fs::read_to_string(proc.join("comm")).ok()?;
    Some(ProcessInfo {
        identity: Identity { pid, start_time },
        parent: fields.get(1)?.parse().ok()?,
        name: name.split_whitespace().collect::<Vec<_>>().join(" "),
    })
}

fn in_process_tree(mut pid: u32, ancestor: u32, processes: &BTreeMap<u32, ProcessInfo>) -> bool {
    while pid != 0 {
        if pid == ancestor {
            return true;
        }
        let Some(process) = processes.get(&pid) else {
            break;
        };
        if pid == process.parent {
            break;
        }
        pid = process.parent;
    }
    false
}

fn matching_mounts(proc: &Path, mounts: &[Mount]) -> HashSet<usize> {
    let mut matched = HashSet::new();
    let mut match_path = |path: &Path| {
        for (index, mount) in mounts.iter().enumerate() {
            if path.starts_with(&mount.root) {
                matched.insert(index);
            }
        }
    };
    for name in ["cwd", "root", "exe"] {
        if let Ok(path) = fs::read_link(proc.join(name)) {
            match_path(&path);
        }
    }
    if let Ok(fds) = fs::read_dir(proc.join("fd")) {
        for fd in fds.flatten() {
            if let Ok(path) = fs::read_link(fd.path()) {
                match_path(&path);
            }
        }
    }
    if let Ok(maps) = fs::read_to_string(proc.join("maps")) {
        for line in maps.lines() {
            if let Some(index) = line.find('/') {
                match_path(Path::new(&line[index..]));
            }
        }
    }
    if mounts.iter().any(|mount| mount.id.is_some())
        && let Ok(fds) = fs::read_dir(proc.join("fdinfo"))
    {
        for fd in fds.flatten() {
            if let Ok(info) = fs::read_to_string(fd.path())
                && let Some(id) = info.lines().find_map(|line| line.strip_prefix("mnt_id:"))
            {
                for (index, mount) in mounts.iter().enumerate() {
                    if mount.id.as_deref() == Some(id.trim()) {
                        matched.insert(index);
                    }
                }
            }
        }
    }
    matched
}

fn find_holders(mounts: &[Mount], engine: u32) -> Vec<Holders> {
    let mut holders: Vec<_> = mounts.iter().map(|_| Holders::new()).collect();
    let Ok(namespace) = fs::read_link("/proc/self/ns/mnt") else {
        return holders;
    };
    let Ok(proc) = fs::read_dir("/proc") else {
        return holders;
    };
    let processes: BTreeMap<_, _> = proc
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse().ok()?;
            process_info(pid).map(|process| (pid, process))
        })
        .collect();
    for (pid, process) in &processes {
        if in_process_tree(*pid, engine, &processes)
            || matches!(
                process.name.as_str(),
                "systemd"
                    | "gnome-shell"
                    | "gnome-session-b"
                    | "Xorg"
                    | "Xwayland"
                    | "dbus-daemon"
                    | "dbus-broker"
            )
            || process.name.starts_with("gvfsd")
        {
            continue;
        }
        let can_close = !in_process_tree(engine, *pid, &processes);
        let path = PathBuf::from(format!("/proc/{pid}"));
        if !fs::read_link(path.join("ns/mnt")).is_ok_and(|current| current == namespace) {
            continue;
        }
        for index in matching_mounts(&path, mounts) {
            holders[index].insert(
                process.identity,
                Holder {
                    name: process.name.clone(),
                    can_close,
                },
            );
        }
    }
    holders
}

fn request_close(process: Identity) -> bool {
    // Pin the kernel process across the identity check and signal so a
    // recycled numeric PID cannot redirect a previously approved request.
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, process.pid, 0) };
    if raw < 0 {
        return false;
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    if !process_info(process.pid).is_some_and(|live| live.identity == process) {
        return false;
    }
    unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            libc::SIGTERM,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        ) == 0
    }
}

fn close_approved(
    mounts: &[Mount],
    holders: &[Holders],
    approved: &HashSet<Identity>,
) -> HashSet<Identity> {
    let mut checked = HashSet::new();
    let mut closing = HashSet::new();
    for (index, processes) in holders.iter().enumerate() {
        for (&process, holder) in processes {
            if holder.can_close
                && approved.contains(&process)
                && !checked.contains(&process)
                && matching_mounts(Path::new(&format!("/proc/{}", process.pid)), mounts)
                    .contains(&index)
                && mounts[index].still_mounted()
            {
                checked.insert(process);
                if request_close(process) {
                    closing.insert(process);
                }
            }
        }
    }
    closing
}

pub(super) fn preflight(app: &App, args: &Value) -> anyhow::Result<Option<Value>> {
    let mounts = mounted_roots(app);
    if mounts.is_empty() {
        return Ok(None);
    }
    let approved: HashSet<Identity> = args
        .get("close_mount_processes")
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()?
        .unwrap_or_default();
    let engine = std::process::id();
    let mut holders = find_holders(&mounts, engine);
    if !approved.is_empty() {
        let closing = close_approved(&mounts, &holders, &approved);
        let until = Instant::now() + Duration::from_secs(3);
        while holders
            .iter()
            .any(|processes| processes.keys().any(|p| closing.contains(p)))
            && Instant::now() < until
        {
            thread::sleep(Duration::from_millis(120));
            holders = find_holders(&mounts, engine);
        }
    }
    let blockers: Vec<_> = mounts
        .iter()
        .zip(holders)
        .filter(|(_, processes)| !processes.is_empty())
        .map(|(mount, processes)| {
            let programs: Vec<_> = processes
                .into_iter()
                .map(|(p, holder)| {
                    json!({"pid":p.pid,"start_time":p.start_time,
                    "name":holder.name,"can_close":holder.can_close})
                })
                .collect();
            json!({"name":mount.name,"path":mount.path,"programs":programs})
        })
        .collect();
    Ok((!blockers.is_empty()).then(|| json!({"quit_blockers":blockers})))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ChildGuard(Child);

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn a_bridge_disconnect_error_can_report_new_mount_holders() {
        let generic = anyhow::anyhow!("bridge worker could not stop");
        let blockers = json!({"quit_blockers":[{"name":"Bridge","path":"/fixture",
            "programs":[{"pid":123,"name":"editor"}]}]});
        assert_eq!(
            blockers_after_failed_disconnect(&generic, || Ok(Some(blockers.clone()))).unwrap(),
            Some(blockers)
        );
        assert!(
            blockers_after_failed_disconnect(&generic, || Ok(None))
                .unwrap()
                .is_none()
        );
        let busy = BusyMount {
            name: "Direct".into(),
            path: "/fixture".into(),
        };
        let fallback = busy.reply();
        assert_eq!(
            blockers_after_failed_disconnect(&busy.into(), || Ok(None)).unwrap(),
            Some(fallback)
        );
    }

    #[test]
    fn only_supported_fuse_type_and_source_pairs_are_owned() {
        for (kind, source) in [
            ("fuse", "twodrive"),
            ("fuse", "twodrive-dev"),
            ("fuse.rclone", "twodrive-ro-fixture"),
        ] {
            assert!(linux_twodrive_fuse_source(kind, source));
        }
        for (kind, source) in [
            ("fuse", "unrelated"),
            ("fuse.sshfs", "twodrive"),
            ("fuse.rclone", "unrelated"),
            ("fuse.gvfsd-fuse", "gvfsd-fuse"),
        ] {
            assert!(!linux_twodrive_fuse_source(kind, source));
        }
    }

    #[test]
    fn a_holding_ancestor_is_reported_but_never_signalled() {
        let temp = tempfile::tempdir().unwrap();
        let mount_path = temp.path().join("mount");
        fs::create_dir(&mount_path).unwrap();
        let _reference = fs::File::open(&mount_path).unwrap();
        let engine = ChildGuard(Command::new("sleep").arg("30").spawn().unwrap());
        let identity = process_info(std::process::id()).unwrap().identity;
        let mounts = [Mount {
            name: "Fixture".into(),
            path: mount_path.clone(),
            root: mount_path,
            id: None,
        }];
        let holders = find_holders(&mounts, engine.0.id());
        assert!(!holders[0][&identity].can_close);
        assert!(close_approved(&mounts, &holders, &HashSet::from([identity])).is_empty());
    }

    #[test]
    fn only_a_confirmed_live_holder_is_signalled() {
        let temp = tempfile::tempdir().unwrap();
        let mount_path = temp.path().join("mount");
        let sibling = temp.path().join("mount-other");
        fs::create_dir(&mount_path).unwrap();
        fs::create_dir(&sibling).unwrap();
        let spawn = |path: &Path| {
            ChildGuard(
                Command::new("sleep")
                    .arg("30")
                    .current_dir(path)
                    .spawn()
                    .unwrap(),
            )
        };
        let mut confirmed = spawn(&mount_path);
        let mut unconfirmed = spawn(&mount_path);
        let mut unrelated = spawn(&sibling);
        let id = |child: &ChildGuard| process_info(child.0.id()).unwrap().identity;
        let confirmed_id = id(&confirmed);
        let unconfirmed_id = id(&unconfirmed);
        let unrelated_id = id(&unrelated);
        let mounts = [Mount {
            name: "Fixture".into(),
            path: mount_path.clone(),
            root: mount_path,
            id: None,
        }];
        assert!(find_holders(&mounts, std::process::id())[0].is_empty());
        let holders = find_holders(&mounts, 0);
        assert!(holders[0].contains_key(&confirmed_id));
        assert!(holders[0].contains_key(&unconfirmed_id));
        assert!(!holders[0].contains_key(&unrelated_id));
        assert!(close_approved(&mounts, &holders, &HashSet::new()).is_empty());
        let stale = Identity {
            start_time: confirmed_id.start_time + 1,
            ..confirmed_id
        };
        assert!(close_approved(&mounts, &holders, &HashSet::from([stale])).is_empty());
        assert_eq!(
            close_approved(
                &mounts,
                &holders,
                &HashSet::from([confirmed_id, unrelated_id])
            ),
            HashSet::from([confirmed_id]),
        );
        assert!(!confirmed.0.wait().unwrap().success());
        assert!(unconfirmed.0.try_wait().unwrap().is_none());
        assert!(unrelated.0.try_wait().unwrap().is_none());
    }

    #[cfg(feature = "mount")]
    #[test]
    #[ignore = "requires /dev/fuse and fusermount3; isolated synthetic mount"]
    fn closing_confirmed_holder_allows_a_normal_fuse_unmount() {
        struct MountCleanup(PathBuf);
        impl Drop for MountCleanup {
            fn drop(&mut self) {
                let _ = Command::new("fusermount3")
                    .args(["-u", "--"])
                    .arg(&self.0)
                    .output();
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("mount");
        fs::create_dir(&path).unwrap();
        let _mount_cleanup = MountCleanup(path.clone());
        let mut worker = ChildGuard(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "desktop::stale_mount::tests::native_fuse_fixture_worker",
                ])
                .env("TWODRIVE_STALE_NATIVE_FIXTURE", temp.path())
                .stdin(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        let id = loop {
            assert!(worker.0.try_wait().unwrap().is_none());
            if let Some(id) = linux_twodrive_fuse_mount_id(&path).unwrap() {
                break id;
            }
            assert!(
                Instant::now() < deadline,
                "synthetic mount never became ready"
            );
            thread::sleep(Duration::from_millis(25));
        };
        let mut holder = ChildGuard(
            Command::new("sleep")
                .arg("30")
                .current_dir(&path)
                .spawn()
                .unwrap(),
        );
        let identity = process_info(holder.0.id()).unwrap().identity;
        let mounts = [Mount {
            name: "Fixture".into(),
            path: path.clone(),
            root: path.clone(),
            id: Some(id),
        }];
        let unmount = || {
            Command::new("fusermount3")
                .env("LC_ALL", "C")
                .args(["-u", "--"])
                .arg(&path)
                .output()
                .unwrap()
        };
        let before = unmount();
        assert!(!before.status.success());
        assert!(String::from_utf8_lossy(&before.stderr).contains("Device or resource busy"));
        let holders = find_holders(&mounts, worker.0.id());
        assert_eq!(
            holders[0].keys().copied().collect::<Vec<_>>(),
            vec![identity]
        );
        assert_eq!(
            close_approved(&mounts, &holders, &HashSet::from([identity])),
            HashSet::from([identity]),
        );
        assert!(!holder.0.wait().unwrap().success());
        assert!(unmount().status.success());
        assert!(worker.0.wait().unwrap().success());
        assert!(!is_mount(&path).unwrap());
    }
}
