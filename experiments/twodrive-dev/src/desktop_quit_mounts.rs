use super::*;
use std::{
    collections::{BTreeMap, HashSet},
    os::unix::fs::MetadataExt,
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
}

type Holders = BTreeMap<Identity, Holder>;

struct Mount {
    name: String,
    path: PathBuf,
    root: PathBuf,
    id: Option<String>,
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
                _ => return None,
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
        let path = PathBuf::from(format!("/proc/{pid}"));
        if !fs::read_link(path.join("ns/mnt")).is_ok_and(|current| current == namespace) {
            continue;
        }
        for index in matching_mounts(&path, mounts) {
            holders[index].insert(
                process.identity,
                Holder {
                    name: process.name.clone(),
                },
            );
        }
    }
    holders
}

pub(super) fn preflight(app: &App, _args: &Value) -> anyhow::Result<Option<Value>> {
    let mounts = mounted_roots(app);
    if mounts.is_empty() {
        return Ok(None);
    }
    let holders = find_holders(&mounts, std::process::id());
    let blockers: Vec<_> = mounts
        .iter()
        .zip(holders)
        .filter(|(_, processes)| !processes.is_empty())
        .map(|(mount, processes)| {
            let programs: Vec<_> = processes
                .into_iter()
                .map(|(p, holder)| {
                    json!({"pid":p.pid,"start_time":p.start_time,
                    "name":holder.name})
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
    fn live_holders_are_reported_and_left_running() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("mount");
        let sibling = temp.path().join("mount-other");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&sibling).unwrap();
        let mut holder = ChildGuard(
            Command::new("sleep")
                .arg("30")
                .current_dir(&root)
                .spawn()
                .unwrap(),
        );
        let mut unrelated = ChildGuard(
            Command::new("sleep")
                .arg("30")
                .current_dir(&sibling)
                .spawn()
                .unwrap(),
        );
        let identity = process_info(holder.0.id()).unwrap().identity;
        let mounts = [Mount {
            name: "Fixture".into(),
            path: root.clone(),
            root,
            id: None,
        }];
        let found = find_holders(&mounts, 0);
        assert_eq!(found[0].keys().copied().collect::<Vec<_>>(), vec![identity]);
        assert!(holder.0.try_wait().unwrap().is_none());
        assert!(unrelated.0.try_wait().unwrap().is_none());
    }
}
