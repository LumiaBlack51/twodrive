//! Owned Windows read-only mount over a generic private Provider bridge.
use super::*;
use std::path::{Component, Prefix};
use twodrive_dev::{rclone_drive, readonly_mount::Bridge, state::random_secret};

pub(super) fn installed() -> bool {
    ["ProgramFiles(x86)", "ProgramFiles"]
        .into_iter()
        .filter_map(std::env::var_os)
        .any(|root| {
            PathBuf::from(root)
                .join("WinFsp/bin/winfsp-x64.dll")
                .is_file()
        })
}
pub(super) fn support() -> Value {
    json!({"supported":true,"platform":"windows","driver_installed":installed(),"readonly":false,"editable":true,
        "help":"挂载权限跟随每个网盘的写入锁；解锁后可编辑。保存先进入本机缓存，请等待上传完成再卸载。",
        "driver_url":"https://winfsp.dev/rel/"})
}
pub(super) fn require_owner(profile: &Profile, args: &Value) -> anyhow::Result<()> {
    ensure!(
        args["profile"].as_str() == Some(&profile.id)
            && args["revision"].as_str() == Some(&profile.revision),
        "挂载所属连接或配置已改变，请刷新后重试"
    );
    Ok(())
}
fn validate_target(path: &Path, private: &Path, protected: &[PathBuf]) -> anyhow::Result<()> {
    ensure!(
        path.is_absolute()
            && matches!(path.components().next(), Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_)))
            && path
                .components()
                .skip(1)
                .all(|p| matches!(p, Component::RootDir | Component::Normal(_))),
        "挂载目录必须是本机磁盘上的绝对路径"
    );
    ensure!(
        !path.exists() && fs::symlink_metadata(path).is_err(),
        "挂载目录必须是尚不存在的子文件夹，请更换目录"
    );
    // Canonicalize the deepest existing ancestor before creating anything.
    let parent = path.parent().context("挂载目录缺少父目录")?;
    let mut ancestor = parent;
    while !ancestor.exists() {
        ancestor = ancestor.parent().context("挂载目录缺少磁盘")?;
    }
    let real = ancestor.canonicalize()?;
    let proposed = real
        .join(parent.strip_prefix(ancestor)?)
        .join(path.file_name().context("invalid mount folder")?);
    for sensitive in std::iter::once(private).chain(protected.iter().map(PathBuf::as_path)) {
        let sensitive = sensitive.canonicalize()?;
        ensure!(
            !proposed.starts_with(&sensitive) && !sensitive.starts_with(&proposed),
            "挂载目录不能覆盖账号、配置或缓存目录"
        );
    }
    fs::create_dir_all(parent)?;
    Ok(())
}
pub(super) fn start(app: &App, p: &Profile, cancel: &AtomicBool) -> anyhow::Result<Process> {
    ensure!(
        installed(),
        "请先安装 WinFsp，再重试挂载（https://winfsp.dev/rel/）"
    );
    rclone_drive::binary()?;
    validate_target(
        &p.mount_path,
        &app.state.dir,
        &providers::protected_paths(p)?,
    )?;
    ensure!(
        app.cache(p, false)?["legacy_pending"].as_u64().unwrap_or(0) == 0,
        "旧挂载缓存仍有待处理数据，请先恢复原挂载"
    );
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--state")
        .arg(&app.state.dir)
        .arg("--peer-bin")
        .arg(&app.args.peer_bin)
        .arg("--launcher")
        .arg(&app.args.launcher)
        .arg("--mount-worker")
        .arg(&p.id);
    let mut process = spawn_with_input(command, true)?;
    drain_errors(&mut process, app.network.clone());
    let stdout = process.0.stdout.take().context("missing mount response")?;
    let (send, receive) = mpsc::sync_channel(1);
    thread::spawn(move || {
        use std::io::Read;
        let mut line = String::new();
        let _ = BufReader::new(stdout).take(4096).read_line(&mut line);
        let _ = send.send(line);
    });
    let started = Instant::now();
    loop {
        ensure!(!cancel.load(Ordering::Relaxed), "挂载已取消");
        ensure!(
            process.0.try_wait()?.is_none(),
            "挂载启动失败，请检查 WinFsp 已安装、目录不存在且网络可用"
        );
        if let Ok(line) = receive.recv_timeout(Duration::from_millis(50)) {
            let reply: Value = serde_json::from_str(&line).context("挂载组件启动失败")?;
            ensure!(
                reply["ready"] == true
                    && reply["profile"] == p.id
                    && reply["revision"] == p.revision,
                "挂载身份检查失败"
            );
            return Ok(process);
        }
        ensure!(
            started.elapsed() < providers::mount_startup_timeout(p),
            "挂载超时，已回收本次挂载组件"
        );
    }
}
pub(super) fn stop(process: &mut Process) -> anyhow::Result<()> {
    if process.0.try_wait()?.is_some() {
        return Ok(());
    }
    let input = process.0.stdin.as_mut().context("missing mount control")?;
    input.write_all(b"unmount\n")?;
    input.flush()?;
    let start = Instant::now();
    while process.0.try_wait()?.is_none() {
        ensure!(
            start.elapsed() < Duration::from_secs(12),
            "挂载尚未卸载，请关闭正在打开的文件后重试"
        );
        thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}
struct Control {
    client: reqwest::blocking::Client,
    url: String,
    username: String,
    password: String,
}
impl Control {
    fn call(&self, endpoint: &str, args: Value) -> anyhow::Result<Value> {
        let response = self
            .client
            .post(format!("{}{endpoint}", self.url))
            .timeout(Duration::from_secs(if endpoint == "mount/mount" {
                55
            } else {
                6
            }))
            .basic_auth(&self.username, Some(&self.password))
            .json(&args)
            .send()
            .map_err(|_| anyhow::anyhow!("本机挂载组件暂不可用"))?;
        ensure!(
            response.status().is_success(),
            "本机挂载操作失败，请检查目录或 WinFsp 安装"
        );
        // Never return arbitrary component errors/logs to public IPC.
        use std::io::Read;
        let mut bytes = Vec::new();
        response.take(65537).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 65536, "mount reply exceeds limit");
        serde_json::from_slice(&bytes).context("invalid local mount response")
    }
}
fn start_component(
    exe: &Path,
    bridge_address: std::net::SocketAddr,
    bridge_token: &str,
    cache_path: &Path,
) -> anyhow::Result<(Process, Control)> {
    let reservation = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = reservation.local_addr()?;
    let control = Control {
        client: reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(8))
            .build()?,
        url: format!("http://{address}/"),
        username: random_secret(),
        password: random_secret(),
    };
    let mut command = Command::new(exe);
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("RCLONE_")
        {
            command.env_remove(key);
        }
    }
    // Cloud credentials stay exclusively in the Provider. Both authenticated
    // local endpoints use random process-private environment credentials.
    command
        .args([
            "rcd",
            "--config=NUL",
            "--ask-password=false",
            "--log-level=ERROR",
            "--stats=0",
            "--retries=1",
            "--low-level-retries=1",
            "--timeout=330s",
            "--contimeout=5s",
        ])
        .env("RCLONE_CACHE_DIR", cache_path)
        .env("RCLONE_RC_ADDR", address.to_string())
        .env("RCLONE_RC_USER", &control.username)
        .env("RCLONE_RC_PASS", &control.password)
        .env("RCLONE_CONFIG_TWODRIVE_TYPE", "webdav")
        .env(
            "RCLONE_CONFIG_TWODRIVE_URL",
            format!("http://{bridge_address}/"),
        )
        .env("RCLONE_CONFIG_TWODRIVE_BEARER_TOKEN", bridge_token)
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost");
    drop(reservation);
    let mut component = spawn(command)?;
    // Neither stdout nor stderr is a public channel. Draining prevents pipe
    // backpressure; content (including paths) is deliberately discarded.
    for stream in [
        component
            .0
            .stdout
            .take()
            .map(|v| Box::new(v) as Box<dyn std::io::Read + Send>),
        component
            .0
            .stderr
            .take()
            .map(|v| Box::new(v) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        thread::spawn(move || {
            let _ = std::io::copy(&mut { stream }, &mut std::io::sink());
        });
    }
    let start = Instant::now();
    loop {
        ensure!(component.0.try_wait()?.is_none(), "挂载组件已退出");
        if let Ok(reply) = control.call("core/pid", json!({})) {
            ensure!(
                reply["pid"].as_u64() == Some(component.0.id() as u64),
                "挂载组件身份不匹配"
            );
            break;
        }
        ensure!(
            start.elapsed() < Duration::from_secs(15),
            "挂载组件启动超时"
        );
        thread::sleep(Duration::from_millis(50));
    }
    Ok((component, control))
}
pub(super) fn worker(app: &App, p: &Profile) -> anyhow::Result<()> {
    ensure!(installed(), "WinFsp 尚未安装");
    validate_target(
        &p.mount_path,
        &app.state.dir,
        &providers::protected_paths(p)?,
    )?;
    let session = app.state.read(&format!("session-{}.json", p.id))?;
    let (backend, binding) =
        providers::restore_session(p, &session, &app.state.dir.join("paused"))?;
    let cache = State::new(app.mount_state(p).join("windows-readonly"))?;
    let _lock = cache.run_lock()?;
    let writable = backend.manual_upload_supported();
    let persistent = State::new(account_cache(app, p, &binding).join("vfs"))?;
    let recovery = account_cache(app, p, &binding).join("write-recovery");
    if writable && dirty_cache(&persistent.dir)? == 0 && recovery.join("observed.json").exists() {
        fs::remove_file(recovery.join("observed.json"))?;
    }
    let bridge = if writable {
        Bridge::start_writable(backend, &cache.dir, &recovery)?
    } else {
        Bridge::start(backend, &cache.dir)?
    };
    let (mut component, control) = start_component(
        &rclone_drive::binary()?,
        bridge.address,
        &bridge.token,
        &persistent.dir,
    )?;
    control.call("mount/mount", json!({"fs":"TWODRIVE:","mountPoint":p.mount_path,"mountOpt":{"VolName":"TwoDrive"},"vfsOpt":{"ReadOnly":!writable,"CacheMode":if writable{3}else{0},"WriteBack":"0s","DirCacheTime":"5s","CacheMaxAge":"24h","CachePollInterval":"5s"}}))?;
    println!(
        "{}",
        json!({"ready":true,"profile":p.id,"revision":p.revision})
    );
    std::io::stdout().flush()?;
    let (send, receive) = mpsc::channel();
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            if line.is_ok_and(|line| line == "unmount") && send.send(()).is_err() {
                return;
            }
        }
        let _ = send.send(()); // Parent pipe closed: never leave a native mount.
    });
    let mut sample = Instant::now() - Duration::from_secs(2);
    loop {
        ensure!(component.0.try_wait()?.is_none(), "挂载组件已退出");
        if writable && sample.elapsed() >= Duration::from_secs(1) {
            let result = control.call("vfs/stats", json!({}));
            let (pending, error) = match result {
                Ok(v) => (
                    v["diskCache"]["uploadsInProgress"].as_u64().unwrap_or(0)
                        + v["diskCache"]["uploadsQueued"].as_u64().unwrap_or(0),
                    v["diskCache"]["erroredFiles"].as_u64().unwrap_or(0) > 0
                        || v["diskCache"]["outOfSpace"] == true,
                ),
                Err(_) => (1, true),
            };
            persistent.save("status.json", &json!({"pending":pending,"error":error}))?;
            sample = Instant::now();
        }
        match receive.recv_timeout(Duration::from_millis(200)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                if writable && dirty_cache(&persistent.dir)? > 0 {
                    continue;
                }
                if control
                    .call("mount/unmount", json!({"mountPoint":p.mount_path}))
                    .is_ok()
                {
                    let _ = control.call("core/quit", json!({}));
                    persistent.save("status.json", &json!({"pending":0,"error":false}))?;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    drop(component);
    drop(bridge);
    Ok(())
}

fn account_cache(app: &App, p: &Profile, binding: &str) -> PathBuf {
    twodrive_dev::mount_write::account_cache(&app.mount_state(p), binding)
}
/// Inspect only our private metadata, including dirty files whose handles have
/// not closed yet. RC upload queues alone do not include those open editors.
fn dirty_cache(root: &Path) -> anyhow::Result<u64> {
    let meta = root.join("vfsMeta");
    if !meta.exists() {
        return Ok(0);
    }
    let mut stack = vec![meta];
    let mut pending = 0;
    let mut visited = 0;
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            visited += 1;
            ensure!(visited <= 100000, "本机写入缓存项目过多，请先处理缓存");
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                pending += 1;
            } else if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                if entry.metadata()?.len() > 65536 {
                    pending += 1;
                    continue;
                }
                let value: Option<Value> = fs::read(entry.path())
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok());
                if value.as_ref().is_none_or(|v| v["Dirty"] == true) {
                    pending += 1;
                }
            }
        }
    }
    Ok(pending)
}
pub(super) fn cache_status(app: &App, p: &Profile) -> anyhow::Result<Value> {
    let binding = app
        .saved_session(p)
        .and_then(|v| v["binding"].as_str().map(str::to_owned))
        .unwrap_or_default();
    let root = account_cache(app, p, &binding);
    let vfs = root.join("vfs");
    let pending = dirty_cache(&vfs)?;
    let report: Value = fs::read(vfs.join("status.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(json!({}));
    let recovery = twodrive_dev::mount_write::recovery_status(&root.join("write-recovery"))?;
    let mut directories = vec![app.mount_state(p).join("windows-accounts")];
    if let Ok(d) = providers::descriptor(&p.kind)
        && let Some(key) = d["write_recovery_field"].as_str()
    {
        directories.push(PathBuf::from(p.option(key)).join("recovery"));
    }
    Ok(
        json!({"bytes":recovery["bytes"],"pending":pending,"write_error":report["error"]==true,
        "recovery_files":recovery["files"],"recovery_directories":directories,"released":0}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unmount_owner_is_bound_to_revision_without_requiring_a_live_backend() {
        let profile: Profile = serde_json::from_value(
            json!({"id":"owner","revision":"r1","kind":"dav","name":"Owner"}),
        )
        .unwrap();
        require_owner(&profile, &json!({"profile":"owner","revision":"r1"})).unwrap();
        assert!(require_owner(&profile, &json!({"profile":"other","revision":"r1"})).is_err());
        assert!(require_owner(&profile, &json!({"profile":"owner","revision":"old"})).is_err());
    }
    #[test]
    fn native_mount_target_never_covers_existing_or_private_data() {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("private");
        fs::create_dir(&private).unwrap();
        let saved = root.path().join("keep.txt");
        fs::write(&saved, b"user data").unwrap();
        assert!(validate_target(&saved, &private, &[]).is_err());
        assert_eq!(fs::read(saved).unwrap(), b"user data");
        assert!(validate_target(&private.join("nested/mount"), &private, &[]).is_err());
        assert!(!private.join("nested").exists());
        assert!(validate_target(&root.path().join("..\\escape"), &private, &[]).is_err());
        let external = root.path().join("other-account");
        fs::create_dir(&external).unwrap();
        assert!(
            validate_target(
                &external.join("mount"),
                &private,
                std::slice::from_ref(&external)
            )
            .is_err()
        );
        let target = root.path().join("view/readonly");
        validate_target(&target, &private, &[]).unwrap();
        assert!(target.parent().unwrap().is_dir() && !target.exists());
    }
    #[test]
    fn owned_worker_receives_unmount_and_is_reaped() {
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "if ([Console]::ReadLine() -eq 'unmount') { exit 0 } else { exit 2 }",
        ]);
        let mut process = spawn_with_input(command, true).unwrap();
        stop(&mut process).unwrap();
        assert!(process.0.try_wait().unwrap().unwrap().success());
    }
    #[test]
    #[ignore = "requires the pinned official Rclone executable; no driver or cloud account"]
    fn official_local_control_authentication_and_readonly_options() {
        let exe = PathBuf::from(
            std::env::var_os("TWODRIVE_TEST_RCLONE").expect("TWODRIVE_TEST_RCLONE required"),
        );
        let (_component, control) = start_component(
            &exe,
            "127.0.0.1:1".parse().unwrap(),
            "synthetic-bridge-token",
            tempfile::tempdir().unwrap().path(),
        )
        .unwrap();
        assert_eq!(
            control
                .client
                .post(format!("{}core/pid", control.url))
                .json(&json!({}))
                .send()
                .unwrap()
                .status(),
            401
        );
        let mounts = control.call("mount/listmounts", json!({})).unwrap();
        assert!(mounts["mountPoints"].as_array().unwrap().is_empty());
        control.call("options/set",json!({"vfs":{"ReadOnly":true,"CacheMode":0,"DirCacheTime":"5s"},"mount":{"VolName":"TwoDrive"}})).unwrap();
        let options = control.call("options/get", json!({})).unwrap();
        assert_eq!(options["vfs"]["ReadOnly"], true);
        let _ = control.call("core/quit", json!({}));
    }
}
