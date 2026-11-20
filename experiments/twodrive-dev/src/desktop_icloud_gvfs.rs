//! One authenticated GNOME WebDAV mount per iCloud worker.
use super::*;
use serde::{Deserialize, Serialize};
use std::os::unix::fs::{PermissionsExt, symlink};

#[path = "desktop_icloud_history.rs"]
mod history;
pub(in super::super) use history::rows as opened_files;

const OWNER: &str = "gnome-webdav.json";

#[derive(Deserialize, Serialize)]
struct Owner {
    path: PathBuf,
    export: Option<PathBuf>,
    restore_mode: Option<u32>,
    uri: String,
    revision: String,
    binding: String,
}

pub(super) fn selected(app: &App, p: &Profile) -> bool {
    p.kind == "icloud" && app.state.dir.join("icloud-gnome-webdav").is_file()
}

fn safe_uri(uri: &str) -> bool {
    url::Url::parse(uri).is_ok_and(|u| {
        u.scheme() == "dav"
            && u.host_str() == Some("127.0.0.1")
            && u.port().is_some()
            && u.username().strip_prefix("iCloud-").is_some_and(|value| {
                value.len() == 8
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
            && u.password().is_none()
            && u.path() == "/"
            && u.query().is_none()
            && u.fragment().is_none()
    })
}

fn export_root() -> anyhow::Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").context("缺少 GNOME 会话目录")?)
            .join("gvfs"),
    )
}

fn safe_export(path: &Path) -> bool {
    export_root().is_ok_and(|root| path.parent() == Some(root.as_path()))
        && path.file_name().is_some()
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}

fn call_gio(mode: &str, uri: &str, password: Option<&str>) -> anyhow::Result<Vec<u8>> {
    ensure!(safe_uri(uri), "无效的本机 iCloud 地址");
    let mut command = Command::new("/usr/bin/python3");
    command
        .args(["-c", include_str!("desktop_icloud_gvfs.py"), mode, uri])
        .env("GIO_USE_VFS", "gvfs");
    let mut process = spawn_with_input(command, password.is_some())?;
    if let Some(password) = password {
        let mut input = process.0.stdin.take().context("缺少本机挂载输入")?;
        writeln!(input, "{password}")?;
    }
    let started = Instant::now();
    let status = loop {
        if let Some(status) = process.0.try_wait()? {
            break status;
        }
        ensure!(
            started.elapsed() < Duration::from_secs(10),
            "GNOME 挂载操作超时"
        );
        thread::sleep(Duration::from_millis(40));
    };
    ensure!(status.success(), "GNOME 无法完成 iCloud 挂载操作");
    let mut output = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(
            process.0.stdout.take().context("缺少 GNOME 挂载结果")?,
            8192,
        ),
        &mut output,
    )?;
    Ok(output)
}

fn forget(cache: &State, owner: &Owner) -> anyhow::Result<()> {
    if let Some(export) = &owner.export
        && fs::read_link(&owner.path).is_ok_and(|target| target == *export)
    {
        fs::remove_file(&owner.path)?;
    }
    if let Some(mode) = owner.restore_mode
        && fs::symlink_metadata(&owner.path)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        fs::create_dir(&owner.path)?;
        fs::set_permissions(&owner.path, fs::Permissions::from_mode(mode))?;
    }
    fs::remove_file(cache.dir.join(OWNER))?;
    Ok(())
}

fn belongs(owner: &Owner, p: &Profile, binding: &str) -> bool {
    owner.path == p.mount_path
        && owner.binding == binding
        && safe_uri(&owner.uri)
        && owner.export.as_ref().is_none_or(|path| safe_export(path))
}

pub(super) fn recover(app: &App, p: &Profile) -> anyhow::Result<()> {
    if p.kind != "icloud" {
        return Ok(());
    }
    let dir = app.mount_state(p).join("linux-readonly");
    if !dir.join(OWNER).exists() {
        return Ok(());
    }
    let binding = app.saved_session(p).context("缺少 iCloud 会话")?;
    let cache = State::new(dir)?;
    let _lock = cache.run_lock().context("iCloud 已在另一个进程中挂载")?;
    let owner: Owner = cache.read(OWNER)?;
    ensure!(
        belongs(
            &owner,
            p,
            binding["binding"].as_str().context("缺少账号标识")?
        ),
        "旧 GNOME 挂载身份不匹配"
    );
    call_gio("unmount", &owner.uri, None)?;
    forget(&cache, &owner)
}

pub(in super::super) fn browse_uri(app: &App, id: &str) -> Option<String> {
    let p = app.profile(id).ok()?;
    if p.kind != "icloud" {
        return None;
    }
    let session = app.saved_session(&p)?;
    let data = fs::read(app.mount_state(&p).join("linux-readonly").join(OWNER)).ok()?;
    let owner: Owner = serde_json::from_slice(&data).ok()?;
    (owner.revision == p.revision
        && belongs(&owner, &p, session["binding"].as_str()?)
        && owner.export.is_some())
    .then_some(owner.uri)
}

pub(super) fn run(
    p: &Profile,
    backend: Backend,
    cache: &State,
    recovery: Option<&Path>,
    binding: &str,
) -> anyhow::Result<()> {
    let nonce = random_secret();
    let username = format!("iCloud-{}", &nonce[..8]);
    let bridge = Bridge::start_gvfs(backend, &cache.dir, recovery, &username)?;
    let uri = format!("dav://{username}@{}/", bridge.address);
    let mut journal = history::Journal::new(cache, p, binding, &uri)?;
    let mut owner = Owner {
        path: p.mount_path.clone(),
        export: None,
        restore_mode: fs::symlink_metadata(&p.mount_path)
            .ok()
            .filter(|metadata| metadata.is_dir())
            .map(|metadata| metadata.permissions().mode() & 0o7777),
        uri,
        revision: p.revision.clone(),
        binding: binding.into(),
    };
    cache.save(OWNER, &owner)?;
    let result = (|| -> anyhow::Result<()> {
        let output = call_gio("mount", &owner.uri, Some(&bridge.token))?;
        let export: PathBuf =
            serde_json::from_slice(&output).context("GNOME 未导出本机文件路径")?;
        ensure!(safe_export(&export), "GNOME 导出的文件路径不受信任");
        owner.export = Some(export.clone());
        cache.save(OWNER, &owner)?;
        if owner.restore_mode.is_some() {
            fs::remove_dir(&owner.path).context("iCloud 挂载目录必须为空")?;
        }
        symlink(&export, &owner.path).context("无法创建 iCloud 本机路径")?;
        println!(
            "{}",
            json!({"ready":true,"profile":p.id,"revision":p.revision})
        );
        std::io::stdout().flush()?;
        let receive = unmount_requests();
        let mut requested = false;
        let mut last_update = None;
        let mut published = None;
        loop {
            requested |= WORKER_TERMINATING.load(Ordering::Relaxed)
                || !matches!(
                    receive.recv_timeout(Duration::from_millis(200)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                );
            if requested
                || last_update.is_none_or(|last: Instant| last.elapsed() >= Duration::from_secs(1))
            {
                last_update = Some(Instant::now());
                let _ = journal.refresh(cache, &owner.path, &export);
                if let Ok(paths) = bridge.cached_paths()
                    && published.as_ref() != Some(&paths)
                    && cache
                        .save(
                            "gnome-cache.json",
                            &json!({"export": export, "paths": paths}),
                        )
                        .is_ok()
                {
                    published = Some(paths);
                }
            }
            if !requested {
                continue;
            }
            if recovery.is_some_and(|path| {
                !twodrive_dev::mount_write::recovery_status(path)
                    .is_ok_and(|value| value["editor_pending"] == 0)
            }) {
                thread::sleep(Duration::from_millis(200));
                continue;
            }
            if call_gio("unmount", &owner.uri, None).is_ok() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(250));
        }
    })();
    if result.is_ok() || call_gio("unmount", &owner.uri, None).is_ok() {
        let forgotten = forget(cache, &owner);
        result.and(forgotten)
    } else {
        // Leave ownership recorded if the desktop still holds the mount.
        result
    }
}
