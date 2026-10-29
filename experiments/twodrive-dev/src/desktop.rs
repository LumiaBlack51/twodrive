use anyhow::{Context, ensure};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use twodrive_core::{Database, FileState};
use twodrive_dev::{
    model::{CloudBackend, MetadataEntry},
    network::Diagnostics,
    profile::Profile,
    provider::Backend,
    state::State,
};

#[path = "desktop_backup.rs"]
mod backup;
#[path = "desktop_connections.rs"]
mod connections;
#[path = "desktop_local_files.rs"]
mod local_files;
#[cfg(all(target_os = "linux", feature = "mount"))]
#[path = "mount.rs"]
mod mount;
#[path = "desktop_mount_paths.rs"]
mod mount_paths;
#[cfg(any(windows, all(target_os = "linux", feature = "mount")))]
#[path = "desktop_readonly_mount.rs"]
mod native_mount;
#[path = "desktop_sharing.rs"]
mod sharing;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    state: PathBuf,
    #[arg(long)]
    peer_bin: PathBuf,
    #[arg(long)]
    launcher: PathBuf,
    #[arg(long)]
    mount_worker: Option<String>,
    #[arg(long)]
    backup_worker: Option<String>,
    #[arg(long, hide = true)]
    native_local_fixture_root: Option<PathBuf>,
}

#[path = "desktop_providers.rs"]
mod providers;
#[derive(Clone, Default, Serialize, Deserialize)]
struct Config {
    profiles: Vec<Profile>,
    selected: Option<String>,
}
#[derive(Deserialize)]
struct Request {
    id: u64,
    op: String,
    #[serde(default)]
    args: Value,
}

type Output = Arc<Mutex<std::io::Stdout>>;
type Cancellations = Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>;
fn emit(out: &Output, value: Value) {
    if let Ok(mut out) = out.lock() {
        let _ = writeln!(out, "{value}");
        let _ = out.flush();
    }
}
fn string<'a>(args: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .context(format!("missing {key}"))
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}
#[cfg(target_os = "linux")]
fn linux_mount_info(path: &Path) -> anyhow::Result<Option<(String, String, String)>> {
    for line in fs::read_to_string("/proc/self/mountinfo")?.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let fields: Vec<_> = left.split_whitespace().collect();
        let Some(target) = fields.get(4) else {
            continue;
        };
        let target = target
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\");
        if Path::new(&target) == path {
            let mut parts = right.split_whitespace();
            return Ok(Some((
                fields[0].into(),
                parts.next().context("missing mount type")?.into(),
                parts.next().context("missing mount source")?.into(),
            )));
        }
    }
    Ok(None)
}
#[cfg(target_os = "linux")]
fn is_mount(path: &Path) -> anyhow::Result<bool> {
    Ok(linux_mount_info(path)?.is_some())
}
#[cfg(not(any(windows, target_os = "linux")))]
fn is_mount(path: &Path) -> anyhow::Result<bool> {
    Ok(Command::new("mountpoint")
        .args(["-q", "--"])
        .arg(path)
        .status()?
        .success())
}
fn metadata(m: MetadataEntry) -> Value {
    json!({"path":m.path,"name":m.name,"directory":m.is_dir,"size":m.size,
        "modified":m.modified_unix,"etag":m.etag})
}

#[path = "desktop_process.rs"]
mod process_support;
struct Process(Child, #[cfg(windows)] process_support::WindowsJob);
impl Drop for Process {
    fn drop(&mut self) {
        #[cfg(windows)]
        let _ = &self.1;
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn spawn(command: Command) -> anyhow::Result<Process> {
    spawn_with_input(command, false)
}
fn spawn_with_input(mut command: Command, input: bool) -> anyhow::Result<Process> {
    command
        .stdin(if input { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // No gateway or mount worker should outlive a crashed desktop engine.
    #[cfg(target_os = "linux")]
    unsafe {
        command.pre_exec(|| {
            let parent = libc::getppid();
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(std::io::Error::other("parent exited"));
            }
            Ok(())
        });
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn()?;
    #[cfg(windows)]
    let job = process_support::WindowsJob::attach(&mut child)?;
    // The mutable binding is needed by the Windows job attachment.
    let _ = &mut child;
    Ok(Process(
        child,
        #[cfg(windows)]
        job,
    ))
}
fn drain_errors(process: &mut Process, network: Arc<Diagnostics>) {
    if let Some(stderr) = process.0.stderr.take() {
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                // Only the network layer's credential-free diagnostics enter the UI.
                if line.starts_with("network:") {
                    network.record(&line);
                } else if std::env::var_os("TWODRIVE_MOUNT_TRACE").is_some()
                    && line.starts_with("mount-lifecycle ")
                {
                    eprintln!("{line}");
                }
            }
        });
    }
}

struct App {
    inactive: HashMap<String, connections::Parked>,
    jobs: connections::Jobs,
    native_fixture: local_files::NativeFixture,
    args: Args,
    state: State,
    config: Config,
    connected: Option<Profile>,
    backend: Option<Backend>,
    gateway: Option<Process>,
    mounted: Option<Process>,
    backup: Option<Process>,
    network: Arc<Diagnostics>,
    hosting: sharing::Hosting,
}
impl App {
    fn saved_session(&self, p: &Profile) -> Option<Value> {
        let session: Value = self.state.read(&format!("session-{}.json", p.id)).ok()?;
        (session["kind"].as_str() == Some(&p.kind)
            && session["revision"].as_str() == Some(&p.revision)
            && session["binding"].as_str().is_some_and(|s| !s.is_empty()))
        .then_some(session)
    }
    fn profile(&self, id: &str) -> anyhow::Result<Profile> {
        self.config
            .profiles
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .context("profile not found")
    }
    fn account_name(&self, p: &Profile) -> Option<String> {
        let session: Value = self.state.read(&format!("session-{}.json", p.id)).ok()?;
        if session["kind"].as_str() != Some(&p.kind)
            || session["revision"].as_str() != Some(&p.revision)
        {
            return None;
        }
        session["account_name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
    }
    fn public_profiles(&self) -> Value {
        let profiles: Vec<_> = self
            .config
            .profiles
            .iter()
            .map(|p| {
                let mut public = providers::public_profile(p);
                public["account_name"] = json!(self.account_name(p));
                public["connected"] = json!(self.online(&p.id));
                public["mount_readonly"] = json!(!providers::editable(p));
                public["startup_ready"] = json!(self.saved_session(p).is_some());
                public
            })
            .collect();
        json!({"providers":providers::catalog(),"profiles":profiles,"selected":self.config.selected})
    }
    fn mount_state(&self, p: &Profile) -> PathBuf {
        self.state.dir.join(format!("mount-{}", p.id))
    }
    fn cache(&self, p: &Profile, clear: bool) -> anyhow::Result<Value> {
        let path = self.mount_state(p).join("mount.sqlite3");
        if !path.exists() {
            #[cfg(windows)]
            return native_mount::cache_status(self, p);
            #[cfg(all(target_os = "linux", feature = "mount"))]
            if providers::uses_readonly_bridge(p) {
                return native_mount::cache_status(self, p);
            }
            #[cfg(not(windows))]
            return Ok(json!({"bytes":0,"pending":0,"released":0}));
        }
        let db = Database::new(path);
        db.init()?;
        let deletes = db.pending_deletes()?;
        let deferred = deletes
            .iter()
            .filter(|d| !d.retry_due(twodrive_core::now_unix()))
            .count();
        let rejected = deletes
            .iter()
            .filter(|d| {
                matches!(
                    d.failure_kind,
                    Some(
                        twodrive_core::DeleteFailureKind::Permission
                            | twodrive_core::DeleteFailureKind::Authentication
                            | twodrive_core::DeleteFailureKind::Rejected
                    )
                )
            })
            .count();
        let next_retry = deletes
            .iter()
            .filter(|d| !d.retry_due(twodrive_core::now_unix()))
            .map(|d| d.next_retry_unix)
            .min();
        let released = if clear { db.prune_cache(0)? } else { 0 };
        let records = db.all_records()?;
        let pending = records
            .iter()
            .filter(|r| {
                matches!(
                    r.state,
                    FileState::Writing
                        | FileState::Dirty
                        | FileState::Uploading
                        | FileState::Conflict
                        | FileState::Error
                )
            })
            .count()
            + deletes.len()
            + db.pending_metadata_operations()?.len();
        let bytes: u64 = records
            .iter()
            .filter_map(|r| r.cache_path.as_ref())
            .filter_map(|p| fs::metadata(p).ok())
            .map(|m| m.len())
            .sum();
        let mut waiting: Vec<_> = records.iter().filter(|r| matches!(r.state, FileState::Writing | FileState::Dirty | FileState::Uploading | FileState::Conflict | FileState::Error)).take(30).map(|r| json!({"name":r.metadata.name,"path":r.metadata.path,"kind":"upload","state":if matches!(r.state, FileState::Conflict | FileState::Error) {"需要处理"} else {"等待同步"},"bytes_done":0,"bytes_total":r.metadata.size})).collect();
        waiting.extend(deletes.iter().take(30_usize.saturating_sub(waiting.len())).map(|d| json!({
            "name":d.path.rsplit('/').next().unwrap_or(""),"kind":"delete",
            "state":if matches!(d.failure_kind, Some(twodrive_core::DeleteFailureKind::Permission | twodrive_core::DeleteFailureKind::Authentication | twodrive_core::DeleteFailureKind::Rejected)) {"云端删除被拒绝，任务已保留"} else {"本地已删除，云端删除待确认"},
            "next_retry_unix":d.next_retry_unix,"http_status":d.last_status
        })));
        let mut result = json!({"bytes":bytes,"pending":pending,"released":released,"waiting":waiting,
            "deletes_pending":deletes.len(),"deletes_deferred":deferred,"deletes_rejected":rejected,"delete_next_retry_unix":next_retry});
        result["legacy_pending"] = json!(pending);
        #[cfg(any(windows, all(target_os = "linux", feature = "mount")))]
        {
            let native = native_mount::cache_status(self, p)?;
            result["pending"] = json!(pending as u64 + native["pending"].as_u64().unwrap_or(0));
            for key in ["write_error", "recovery_files", "recovery_directories"] {
                result[key] = native[key].clone();
            }
        }
        Ok(result)
    }
    fn unmount(&mut self, check_pending: bool) -> anyhow::Result<()> {
        if self.mounted.is_none() {
            return Ok(());
        }
        #[cfg(windows)]
        {
            if check_pending
                && let Some(p) = &self.connected
                && providers::editable(p)
            {
                ensure!(
                    native_mount::cache_status(self, p)?["pending"] == 0,
                    "挂载仍有未上传内容，请关闭编辑器并等待同步完成；本机缓存已保留"
                );
            }
            native_mount::stop(self.mounted.as_mut().unwrap())?;
            self.mounted.take();
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let p = self.connected.as_ref().context("missing mount profile")?;
            #[cfg(all(target_os = "linux", feature = "mount"))]
            if providers::uses_readonly_bridge(p) {
                if check_pending && providers::editable(p) {
                    ensure!(
                        native_mount::cache_status(self, p)?["pending"] == 0,
                        "挂载仍有未上传内容，请关闭编辑器并等待同步完成；本机缓存已保留"
                    );
                }
                native_mount::stop(self.mounted.as_mut().unwrap())?;
                self.mounted.take();
                return Ok(());
            }
            if !is_mount(&p.mount_path)? {
                self.mounted.take();
                return Ok(());
            }
            if check_pending && providers::editable(p) {
                ensure!(
                    self.cache(p, false)?["pending"] == 0,
                    "挂载仍有未上传或冲突的数据，请先完成同步；缓存已保留"
                );
            }
            let result = Command::new("fusermount3")
                .arg("-u")
                .arg(&p.mount_path)
                .output()?;
            ensure!(
                result.status.success(),
                "挂载仍被占用，请关闭使用该目录的程序后重试"
            );
            self.mounted.take();
            Ok(())
        }
    }
    fn mount_current(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        #[cfg(windows)]
        {
            let profile = self.connected.as_ref().context("请先连接")?.clone();
            ensure!(self.mounted.is_none(), "已经挂载");
            let process = native_mount::start(self, &profile, cancel)?;
            self.mounted = Some(process);
            Ok(())
        }
        #[cfg(not(windows))]
        {
            ensure!(
                cfg!(all(target_os = "linux", feature = "mount")),
                "此平台暂不支持文件管理器挂载，请使用文件浏览与文件夹备份"
            );
            ensure!(self.mounted.is_none(), "已经挂载");
            let p = self.connected.as_ref().context("请先连接")?;
            ensure!(
                !is_mount(&p.mount_path)?,
                "目录已被其他进程挂载，请使用另一个空目录"
            );
            ensure!(self.capabilities()["mount"] == true, "此连接不支持安全挂载");
            #[cfg(all(target_os = "linux", feature = "mount"))]
            if providers::uses_readonly_bridge(p) {
                let process = native_mount::start(self, p, cancel)?;
                self.mounted = Some(process);
                return Ok(());
            }
            let mut cmd = Command::new(std::env::current_exe()?);
            cmd.arg("--state")
                .arg(&self.state.dir)
                .arg("--peer-bin")
                .arg(&self.args.peer_bin)
                .arg("--launcher")
                .arg(&self.args.launcher)
                .arg("--mount-worker")
                .arg(&p.id);
            let mut child = spawn(cmd)?;
            drain_errors(&mut child, self.network.clone());
            if let Some(mut stdout) = child.0.stdout.take() {
                thread::spawn(move || {
                    let _ = std::io::copy(&mut stdout, &mut std::io::sink());
                });
            }
            let start = Instant::now();
            let ready = (|| -> anyhow::Result<()> {
                loop {
                    ensure!(!cancel.load(Ordering::Relaxed), "挂载已取消");
                    ensure!(
                        child.0.try_wait()?.is_none(),
                        "挂载失败，请检查目录为空、FUSE 可用、缓存绑定匹配"
                    );
                    if is_mount(&p.mount_path)? {
                        break Ok(());
                    }
                    ensure!(
                        start.elapsed() < providers::mount_startup_timeout(p),
                        "挂载超时"
                    );
                    thread::sleep(Duration::from_millis(100));
                }
            })();
            self.mounted = Some(child);
            if let Err(error) = ready {
                self.unmount(false)
                    .context("挂载启动未完成且目录被占用，请先关闭占用程序再卸载")?;
                return Err(error);
            }
            Ok(())
        }
    }
    fn disconnect(&mut self) -> anyhow::Result<()> {
        self.unmount(true)?;
        self.backup.take();
        if let Some(p) = &self.connected {
            backup::cleanup_spool(&self.state, p)?;
        }
        self.backend = None;
        self.gateway.take();
        self.connected = None;
        self.network = Arc::default();
        Ok(())
    }
    fn connect(&mut self, id: &str, cancel: &AtomicBool) -> anyhow::Result<Value> {
        self.open_connection(id, cancel, false)
    }
    fn open_connection(
        &mut self,
        id: &str,
        cancel: &AtomicBool,
        resume: bool,
    ) -> anyhow::Result<Value> {
        self.refresh_inactive();
        if let Some(result) = self.select_online(id)? {
            return Ok(result);
        }
        if self.inactive.contains_key(id) || self.connected.as_ref().is_some_and(|p| p.id == id) {
            self.disconnect_profile(id)?;
        }
        let p = self.profile(id)?;
        let network: Arc<Diagnostics> = Arc::default();
        let connection = if resume {
            providers::resume(
                &p,
                &self.args,
                &self
                    .saved_session(&p)
                    .context("没有可恢复的登录，请手动连接")?,
                cancel,
                network.clone(),
            )?
        } else {
            providers::connect(&p, &self.args, cancel, network.clone())?
        };
        let entries = connection.backend.list_directory_checked("/", &mut || {
            ensure!(!cancel.load(Ordering::Relaxed), "连接已取消");
            Ok(())
        })?;
        ensure!(!cancel.load(Ordering::Relaxed), "连接已取消");
        self.state.save(
            &format!("session-{}.json", p.id),
            &providers::session(&connection, &p),
        )?;
        let revision = p.revision.clone();
        self.park_active();
        self.network = network;
        self.backend = Some(connection.backend);
        self.gateway = connection.gateway;
        self.connected = Some(p);
        self.config.selected = Some(id.to_owned());
        self.state.save("profiles.json", &self.config)?;
        self.start_backup()?;
        Ok(
            json!({"entries":entries.into_iter().map(metadata).collect::<Vec<_>>(),"path":"/","revision":revision}),
        )
    }
    fn start_backup(&mut self) -> anyhow::Result<()> {
        self.backup.take();
        let p = self.connected.as_ref().context("请先连接")?;
        backup::cleanup_spool(&self.state, p)?;
        if !self
            .backend
            .as_ref()
            .context("请先连接")?
            .capabilities()
            .backup()
        {
            return Ok(());
        }
        backup::reset_report(&self.state, p)?;
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.arg("--state")
            .arg(&self.state.dir)
            .arg("--peer-bin")
            .arg(&self.args.peer_bin)
            .arg("--launcher")
            .arg(&self.args.launcher)
            .arg("--backup-worker")
            .arg(&p.id);
        let mut child = spawn(cmd)?;
        drain_errors(&mut child, self.network.clone());
        if let Some(mut stdout) = child.0.stdout.take() {
            thread::spawn(move || {
                let _ = std::io::copy(&mut stdout, &mut std::io::sink());
            });
        }
        self.backup = Some(child);
        Ok(())
    }
    fn autostart_path(&self) -> anyhow::Result<PathBuf> {
        #[cfg(windows)]
        return Ok(
            PathBuf::from(std::env::var_os("APPDATA").context("APPDATA missing")?)
                .join("Microsoft/Windows/Start Menu/Programs/Startup/TwoDrive Development.vbs"),
        );
        #[cfg(not(windows))]
        {
            let config = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or(
                    PathBuf::from(std::env::var_os("HOME").context("HOME is unset")?)
                        .join(".config"),
                );
            Ok(config.join("autostart/twodrive-dav.desktop"))
        }
    }
    fn capabilities(&self) -> Value {
        let c = self
            .backend
            .as_ref()
            .map(|b| b.capabilities())
            .unwrap_or_default();
        let mut value = serde_json::to_value(c).unwrap();
        value["backup"] = json!(c.backup());
        value["manual_upload"] = json!(
            self.backend
                .as_ref()
                .is_some_and(|b| b.manual_upload_supported())
        );
        #[cfg(windows)]
        let supported_mount = native_mount::installed();
        #[cfg(not(windows))]
        let supported_mount = cfg!(all(target_os = "linux", feature = "mount"))
            && self
                .connected
                .as_ref()
                .is_some_and(providers::mount_supported)
            && (!c.upload
                || self
                    .connected
                    .as_ref()
                    .is_some_and(providers::uses_readonly_bridge)
                || (c.backup() && c.rename && c.delete));
        value["mount"] = json!(self.backend.is_some() && supported_mount);
        value
    }
    fn status(&mut self) -> anyhow::Result<Value> {
        self.refresh_inactive();
        let accounts = self.account_states()?;
        let overall = connections::aggregate_accounts(&accounts);
        let mounts: Vec<_> = accounts
            .iter()
            .filter(|a| a["mounted"] == true)
            .map(|a| {
                json!({"profile":a["profile"],"revision":a["revision"],"name":a["name"],
                "path":a["mount_path"],"writable":a["writable"],"readonly":a["mount_readonly"],"failed":a["mount_failed"]})
            })
            .collect();
        let gateway_dead = self
            .gateway
            .as_mut()
            .is_some_and(|p| p.0.try_wait().ok().flatten().is_some());
        let mount_dead = self
            .mounted
            .as_mut()
            .is_some_and(|p| p.0.try_wait().ok().flatten().is_some());
        if gateway_dead {
            self.backend = None;
        }
        let cache = self
            .connected
            .as_ref()
            .map(|p| self.cache(p, false))
            .transpose()?;
        let backup = self
            .connected
            .as_ref()
            .map(|p| backup::report(&self.state, p))
            .transpose()?;
        let mount_activity: Value = self
            .connected
            .as_ref()
            .and_then(|p| fs::read(self.mount_state(p).join("activity.json")).ok())
            .and_then(|s| serde_json::from_slice(&s).ok())
            .unwrap_or(json!({"active":[]}));
        let backup_failed = self
            .backup
            .as_mut()
            .is_some_and(|p| p.0.try_wait().ok().flatten().is_some());
        let network = self.network.snapshot();
        let sharing = self.hosting.status(&self.state).unwrap_or_else(|_| json!({"supported":true,"running":false,"error":"共享配置无法读取，请检查私有状态目录"}));
        let mount_support = {
            #[cfg(windows)]
            {
                native_mount::support()
            }
            #[cfg(not(windows))]
            {
                #[cfg(all(target_os = "linux", feature = "mount"))]
                {
                    let mut support = native_mount::support();
                    support["readonly"] = json!(
                        !self
                            .backend
                            .as_ref()
                            .is_some_and(|backend| backend.manual_upload_supported())
                    );
                    support
                }
                #[cfg(not(all(target_os = "linux", feature = "mount")))]
                json!({"supported":false})
            }
        };
        Ok(
            json!({"providers":providers::catalog(),"capabilities":self.capabilities(),"route_fallback":self.network.fallback(),"paused":self.state.dir.join("paused").exists(),"backup":backup,"backup_failed":backup_failed,"mount_activity":mount_activity,
              "version":env!("CARGO_PKG_VERSION"),"sharing":sharing,"local_files":self.native_fixture.capabilities(),
            "connected":self.backend.is_some(),"profile":self.connected.as_ref().map(|p|&p.id),
            "multi_connection":true,"storage_usage":true,"connections":self.config.profiles.iter().filter(|p|self.online(&p.id)).map(|p|json!({"id":p.id,"revision":p.revision,"account_name":self.account_name(p)})).collect::<Vec<_>>(),
            "mounted":self.mounted.is_some(),"mount_failed":mount_dead,"gateway_failed":gateway_dead,"mount_support":mount_support,
            "accounts":accounts,"overall":overall,"mounts":mounts,
            "cache":cache,"network":network,
            "account_name": self.connected.as_ref().and_then(|p| self.account_name(p)),
            "autostart":self.autostart_path()?.exists()}),
        )
    }
    fn handle(
        &mut self,
        req: &Request,
        cancel: Arc<AtomicBool>,
        out: &Output,
    ) -> anyhow::Result<Value> {
        let a = &req.args;
        if self.args.native_local_fixture_root.is_some() {
            ensure!(
                matches!(
                    req.op.as_str(),
                    "status"
                        | "profiles"
                        | "subscribe"
                        | "local_preflight"
                        | "local_items"
                        | "local_read"
                        | "local_dehydrate"
                        | "local_pin"
                        | "local_unpin"
                        | "local_offline"
                        | "local_reconnect"
                        | "local_stop"
                        | "quit"
                ),
                "generated fixture session cannot access real profiles or accounts"
            );
        }
        match req.op.as_str() {
            "local_items" | "local_read" | "local_dehydrate" | "local_pin" | "local_unpin"
            | "local_offline" | "local_reconnect" | "local_stop" => {
                self.native_fixture.request(&req.op, a)
            }
            "local_preflight" => local_files::preflight(a),
            "share_start" => self.hosting.start(&self.state, &self.args, a, &cancel),
            "share_stop" => {
                self.hosting.stop();
                self.hosting.status(&self.state)
            }
            "share_export" => self.hosting.export(
                &self.state,
                Path::new(string(a, "output")?),
                a["replace"] == true,
            ),
            "share_revoke" => self.hosting.revoke(&self.state, string(a, "device")?),
            "provider_action" => providers::action(
                string(a, "kind")?,
                string(a, "action")?,
                a,
                &self.state,
                &cancel,
            ),
            "profiles" => Ok(self.public_profiles()),
            "write_recovery" => {
                let p = self.profile(string(a, "profile")?)?;
                let mut directories = vec![
                    self.mount_state(&p)
                        .join(twodrive_dev::mount_write::account_cache_directory()),
                ];
                if let Some(key) = providers::descriptor(&p.kind)?["write_recovery_field"].as_str()
                {
                    directories.push(PathBuf::from(p.option(key)).join("recovery"));
                }
                for directory in &directories {
                    State::new(directory.clone())?;
                }
                Ok(json!({"directories":directories}))
            }
            "save_profile" => {
                let mut p: Profile = serde_json::from_value(a.clone())?;
                p.writable = providers::editable(&p);
                p.revision = twodrive_dev::state::random_secret();
                if p.id.is_empty() {
                    p.id = format!(
                        "p-{}",
                        twodrive_dev::state::random_secret()
                            .replace('_', "-")
                            .replace('-', "a")
                    );
                }
                ensure!(valid_id(&p.id), "invalid profile id");
                ensure!(
                    matches!(p.network_route.as_str(), "auto" | "direct" | "system"),
                    "invalid network route"
                );
                ensure!(!self.online(&p.id), "请先断开连接再编辑配置");
                ensure!(
                    !p.name.trim().is_empty() && p.name.len() <= 120,
                    "连接名称不能为空"
                );
                if p.mount_path.as_os_str().is_empty() {
                    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                        .context("home missing")?;
                    p.mount_path =
                        mount_paths::default_path(Path::new(&home), &p, &self.config.profiles)?;
                }
                ensure!(p.mount_path.is_absolute(), "挂载目录必须是绝对路径");
                let previous = self.config.profiles.iter().find(|c| c.id == p.id);
                providers::retain_secrets(&mut p, previous, a["clear_password"] == true);
                providers::prepare(&mut p, &self.state)?;
                providers::validate(&p)?;
                let mut next = self.config.clone();
                next.profiles.retain(|c| c.id != p.id);
                next.profiles.push(p.clone());
                if self.connected.is_none() {
                    next.selected = Some(p.id.clone());
                }
                self.state.save("profiles.json", &next)?;
                self.config = next;
                Ok(json!({"id":p.id}))
            }
            "remove_profile" => {
                let id = string(a, "id")?;
                ensure!(!self.online(id), "请先断开连接");
                let p = self.profile(id)?;
                ensure!(
                    self.cache(&p, false)?["pending"] == 0,
                    "此连接有待恢复的缓存，不能删除配置"
                );
                let mut next = self.config.clone();
                next.profiles.retain(|p| p.id != id);
                if next.selected.as_deref() == Some(id) {
                    next.selected = None;
                }
                self.state.save("profiles.json", &next)?;
                self.config = next;
                Ok(json!({}))
            }
            "connect" => self.connect(string(a, "id")?, &cancel),
            "select" => self.select_profile(a),
            "resume" => {
                let p = self.profile(string(a, "id")?)?;
                ensure!(
                    a["revision"].as_str() == Some(&p.revision),
                    "连接配置已改变，请刷新后重试"
                );
                self.open_connection(&p.id, &cancel, true)
            }
            "disconnect" => {
                let id = a["id"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| self.connected.as_ref().map(|p| p.id.clone()));
                if let Some(id) = id {
                    self.disconnect_profile(&id)?;
                }
                Ok(json!({}))
            }
            "quit" => {
                self.disconnect_all()?;
                self.hosting.stop();
                Ok(json!({}))
            }
            "status" => self.status(),
            "pause" => {
                let file = self.state.dir.join("paused");
                if a["paused"] == true {
                    self.state.save("paused", &true)?;
                } else if file.exists() {
                    fs::remove_file(file)?;
                }
                self.status()
            }
            "backups" | "save_backups" => {
                let p = self.profile(string(a, "id")?)?;
                if req.op == "save_backups" {
                    ensure!(
                        !self.online(&p.id)
                            || self
                                .connected
                                .as_ref()
                                .is_some_and(|current| current.id == p.id),
                        "请先切换到此网盘再修改备份设置"
                    );
                    providers::validate(&p)?;
                    let public = providers::public_profile(&p);
                    ensure!(
                        public["capabilities"]["backup"] == true,
                        "此连接不支持文件夹备份"
                    );
                    let folders: Vec<backup::Folder> =
                        serde_json::from_value(a["folders"].clone())?;
                    backup::validate(&folders, &p, &self.state, &providers::protected_paths(&p)?)?;
                    if self.connected.as_ref().is_some_and(|v| v.id == p.id) {
                        self.backup.take();
                        backup::cleanup_spool(&self.state, &p)?;
                    }
                    backup::save_folders(&self.state, &p, &folders)?;
                    if self.connected.as_ref().is_some_and(|v| v.id == p.id) {
                        self.start_backup()?;
                    }
                }
                Ok(json!({"folders":backup::folders(&self.state, &p)?}))
            }
            "sign_out" => {
                let id = string(a, "id")?.to_owned();
                let p = self.profile(&id)?;
                ensure!(self.cache(&p, false)?["pending"] == 0, "请先处理待上传缓存");
                self.disconnect_profile(&id)?;
                providers::sign_out(&p)?;
                self.config.profiles.retain(|p| p.id != id);
                if self.config.selected.as_deref() == Some(&id) {
                    self.config.selected = None;
                }
                self.state.save("profiles.json", &self.config)?;
                Ok(json!({}))
            }
            "autostart" => {
                let path = self.autostart_path()?;
                if a["enabled"] == true {
                    ensure!(self.args.launcher.is_file(), "installed launcher not found");
                    fs::create_dir_all(path.parent().unwrap())?;
                    let launcher = self.args.launcher.to_string_lossy();
                    ensure!(
                        !launcher.contains(['\n', '\r', '%']),
                        "unsupported launcher path"
                    );
                    #[cfg(not(windows))]
                    let escaped = launcher
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                        .replace('`', "\\`")
                        .replace('$', "\\$");
                    #[cfg(not(windows))]
                    fs::write(
                        &path,
                        format!(
                            "[Desktop Entry]\nType=Application\nName=TwoDrive\nExec=\"{escaped}\" --background\nIcon=twodrive-dav-minimal\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
                        ),
                    )?;
                    #[cfg(windows)]
                    {
                        let escaped = launcher.replace('"', "\"\"");
                        let script = format!(
                            "CreateObject(\"WScript.Shell\").Run \"\"\"{escaped}\"\" --background\", 0, False\r\n"
                        );
                        let bytes: Vec<u8> = std::iter::once(0xfeffu16)
                            .chain(script.encode_utf16())
                            .flat_map(u16::to_le_bytes)
                            .collect();
                        fs::write(&path, bytes)?;
                    }
                } else if path.exists() {
                    fs::remove_file(path)?;
                }
                self.status()
            }
            "retry_deletes" => {
                let p = self.profile(string(a, "id")?)?;
                let path = self.mount_state(&p).join("mount.sqlite3");
                let count = if path.exists() {
                    let db = Database::new(path);
                    db.init()?;
                    db.retry_pending_deletes()?
                } else {
                    0
                };
                Ok(json!({"eligible":count,"cloud_delete":"unconfirmed"}))
            }
            "cache_info" | "clear_cache" => {
                let p = self.profile(string(a, "id")?)?;
                self.cache(&p, req.op == "clear_cache")
            }
            "mount" | "unmount" => self.mount_profile(a, req.op == "mount", &cancel),
            "list" | "mkdir" | "rename" | "delete" | "upload" | "download" | "quota" => {
                self.file_context(a)?.execute(req, &cancel, out)
            }
            _ => anyhow::bail!("unknown operation"),
        }
    }
}
impl Drop for App {
    fn drop(&mut self) {
        self.cleanup_inactive();
    }
}

pub fn run() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.native_local_fixture_root.is_some() {
        ensure!(
            !args.state.exists() && args.mount_worker.is_none() && args.backup_worker.is_none(),
            "native fixture requires new dedicated desktop state and no worker"
        );
    }
    let state = State::new(args.state.clone())?;
    let config = if state.dir.join("profiles.json").exists() {
        state.read("profiles.json")?
    } else {
        Config::default()
    };
    let mut app = App {
        args,
        state,
        config,
        connected: None,
        backend: None,
        gateway: None,
        mounted: None,
        backup: None,
        network: Arc::default(),
        hosting: sharing::Hosting::default(),
        inactive: HashMap::new(),
        jobs: HashMap::new(),
        native_fixture: local_files::NativeFixture::default(),
    };
    if let Some(id) = &app.args.backup_worker {
        let p = app.profile(id)?;
        let session = app.state.read(&format!("session-{}.json", p.id))?;
        let (backend, binding) =
            providers::restore_session(&p, &session, &app.state.dir.join("paused"))?;
        return backup::run(
            &app.state,
            &p,
            backend,
            binding,
            providers::protected_paths(&p)?,
        );
    }
    #[cfg(all(target_os = "linux", feature = "mount"))]
    if let Some(id) = &app.args.mount_worker {
        let p = app.profile(id)?;
        ensure!(providers::mount_supported(&p), "此连接不支持安全挂载");
        if providers::uses_readonly_bridge(&p) {
            return native_mount::worker(&app, &p);
        }
        ensure!(!is_mount(&p.mount_path)?, "mount target is already mounted");
        let session = app.state.read(&format!("session-{}.json", p.id))?;
        let (backend, binding) =
            providers::restore_session(&p, &session, &app.state.dir.join("paused"))?;
        let writable = backend.capabilities().upload;
        return mount::mount(
            State::new(app.mount_state(&p))?,
            p.mount_path,
            backend,
            binding,
            writable,
        );
    }
    #[cfg(windows)]
    if let Some(id) = &app.args.mount_worker {
        return native_mount::worker(&app, &app.profile(id)?);
    }
    let _lock = app.state.run_lock()?;
    if let Some(root) = &app.args.native_local_fixture_root {
        app.native_fixture = local_files::NativeFixture::start(root)?;
    }
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let cancellations: Cancellations = Arc::new(Mutex::new(HashMap::new()));
    let (send, receive) = mpsc::sync_channel::<Request>(32);
    let notifications = Arc::new(AtomicBool::new(false));
    let worker_notifications = notifications.clone();
    let worker_out = out.clone();
    let worker_cancel = cancellations.clone();
    let worker = thread::spawn(move || {
        for req in receive {
            let cancel = worker_cancel
                .lock()
                .unwrap()
                .get(&req.id)
                .cloned()
                .unwrap_or_default();
            app.reap_jobs();
            if matches!(req.op.as_str(), "upload" | "download" | "quota" | "list") {
                let id = req.id;
                if let Err(error) = app.start_file_job(
                    req,
                    cancel,
                    worker_out.clone(),
                    worker_cancel.clone(),
                    worker_notifications.load(Ordering::Relaxed),
                ) {
                    emit(
                        &worker_out,
                        json!({"id":id,"ok":false,"error":error.to_string()}),
                    );
                    worker_cancel.lock().unwrap().remove(&id);
                }
                continue;
            }
            let observable = worker_notifications.load(Ordering::Relaxed)
                && !matches!(
                    req.op.as_str(),
                    "status"
                        | "profiles"
                        | "list"
                        | "select"
                        | "backups"
                        | "local_preflight"
                        | "local_items"
                );
            if observable {
                emit(
                    &worker_out,
                    json!({"id":req.id,"event":"operation","op":req.op,"running":true}),
                );
            }
            let result = if cancel.load(Ordering::Relaxed) {
                Err(anyhow::anyhow!("操作已取消"))
            } else {
                app.handle(&req, cancel, &worker_out)
            };
            if observable {
                emit(
                    &worker_out,
                    json!({"id":req.id,"event":"operation","op":req.op,"running":false,
                    "status":app.status().ok(),"configuration":app.public_profiles()}),
                );
            }
            let quit = req.op == "quit" && result.is_ok();
            let value = match result {
                Ok(value) => json!({"id":req.id,"ok":true,"result":value}),
                Err(error) => json!({"id":req.id,"ok":false,"error":error.to_string()}),
            };
            emit(&worker_out, value);
            worker_cancel.lock().unwrap().remove(&req.id);
            if quit {
                break;
            }
        }
        app.stop_jobs(&worker_cancel);
    });
    let mut input = std::io::stdin().lock();
    let mut read_requests = std::collections::HashSet::new();
    loop {
        // UI requests never need large payloads: files stream from disk.
        let mut line = String::new();
        use std::io::Read;
        if input.by_ref().take(1024 * 1024 + 1).read_line(&mut line)? == 0 {
            break;
        }
        ensure!(line.len() <= 1024 * 1024, "IPC line exceeds limit");
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if req.op == "subscribe" {
            notifications.store(req.args["operations"] == true, Ordering::Relaxed);
            emit(
                &out,
                json!({"id":req.id,"ok":true,"result":{"operations":notifications.load(Ordering::Relaxed)}}),
            );
        } else if req.op == "cancel" {
            if let Some(id) = req.args["id"].as_u64()
                && let Some(cancel) = cancellations.lock().unwrap().get(&id)
            {
                cancel.store(true, Ordering::Relaxed);
            }
            emit(&out, json!({"id":req.id,"ok":true,"result":{}}));
        } else {
            let id = req.id;
            let mut flags = cancellations.lock().unwrap();
            if flags.contains_key(&id) {
                emit(
                    &out,
                    json!({"id":id,"ok":false,"error":"duplicate request id"}),
                );
                continue;
            }
            read_requests.retain(|id| flags.contains_key(id));
            if req.op == "quit" {
                // Abandon display reads and startup restoration/mounts only.
                // User transfers/mutations keep their completion protections.
                for id in &read_requests {
                    if let Some(flag) = flags.get(id) {
                        flag.store(true, Ordering::Relaxed);
                    }
                }
            } else if matches!(req.op.as_str(), "list" | "quota" | "resume")
                || req.op == "mount" && req.args["startup"] == true
            {
                read_requests.insert(id);
            }
            flags.insert(id, Arc::new(AtomicBool::new(false)));
            drop(flags);
            // Never block the reader behind a full work queue: cancellation must
            // still get through while a network request is waiting.
            match send.try_send(req) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(_)) => {
                    cancellations.lock().unwrap().remove(&id);
                    emit(
                        &out,
                        json!({"id":id,"ok":false,"error":"request queue is full"}),
                    );
                }
                Err(mpsc::TrySendError::Disconnected(_)) => break,
            }
        }
    }
    for cancel in cancellations.lock().unwrap().values() {
        cancel.store(true, Ordering::Relaxed);
    }
    drop(send);
    let _ = worker.join();
    Ok(())
}

#[cfg(test)]
#[path = "../tests/support/opaque_provider.rs"]
mod test_provider;
#[cfg(test)]
mod tests {
    use super::*;
    use test_provider::OpaqueProvider;
    #[cfg(all(target_os = "linux", feature = "mount"))]
    #[test]
    fn remote_readonly_grant_remains_mountable_without_local_permission_controls() {
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path().join("private")).unwrap();
        let p = Profile {
            id: "locked-account".into(),
            revision: "saved-revision".into(),
            writable: true,
            ..Default::default()
        };
        let backend = Backend::new(OpaqueProvider::default()).with_write_access(false, None);
        let mut app = App {
            args: Args {
                state: state.dir.clone(),
                peer_bin: PathBuf::new(),
                launcher: PathBuf::new(),
                mount_worker: None,
                backup_worker: None,
                native_local_fixture_root: None,
            },
            state,
            config: Config {
                profiles: vec![p.clone()],
                selected: Some(p.id.clone()),
            },
            connected: Some(p.clone()),
            backend: Some(backend),
            gateway: None,
            mounted: None,
            backup: None,
            network: Arc::default(),
            hosting: sharing::Hosting::default(),
            inactive: HashMap::new(),
            jobs: HashMap::new(),
            native_fixture: local_files::NativeFixture::default(),
        };
        assert_eq!(app.capabilities()["upload"], false);
        assert_eq!(
            app.capabilities()["mount"],
            true,
            "remote read-only grants still support browsing mounts"
        );
        app.backend = Some(Backend::new(OpaqueProvider::default()));
        assert_eq!(app.capabilities()["mount"], true);
        assert_eq!(app.capabilities()["upload"], true);
    }
    #[test]
    fn retained_cloud_deletion_is_visible_and_manual_retry_keeps_the_original_task() {
        let temp = tempfile::tempdir().unwrap();
        let active = Profile {
            id: "active".into(),
            revision: "revision".into(),
            ..Default::default()
        };
        let state = State::new(temp.path().join("private")).unwrap();
        let mut app = App {
            args: Args {
                state: state.dir.clone(),
                peer_bin: PathBuf::new(),
                launcher: PathBuf::new(),
                mount_worker: None,
                backup_worker: None,
                native_local_fixture_root: None,
            },
            state,
            config: Config {
                profiles: vec![active.clone()],
                selected: Some(active.id.clone()),
            },
            connected: Some(active.clone()),
            backend: Some(Backend::new(OpaqueProvider::default())),
            gateway: None,
            mounted: None,
            backup: None,
            network: Arc::default(),
            hosting: sharing::Hosting::default(),
            inactive: HashMap::new(),
            jobs: HashMap::new(),
            native_fixture: local_files::NativeFixture::default(),
        };
        let directory = app.mount_state(&active);
        fs::create_dir_all(&directory).unwrap();
        let db = Database::new(directory.join("mount.sqlite3"));
        db.init().unwrap();
        db.queue_remote_delete("private-cloud-id", "/old-file")
            .unwrap();
        db.defer_pending_delete(
            "private-cloud-id",
            Some(403),
            twodrive_core::DeleteFailureKind::Permission,
            0,
        )
        .unwrap();
        let report = app.cache(&active, false).unwrap();
        assert_eq!(report["pending"], 1);
        assert_eq!(report["deletes_rejected"], 1);
        assert_eq!(report["deletes_deferred"], 1);
        assert_eq!(report["waiting"][0]["kind"], "delete");
        assert!(!report.to_string().contains("private-cloud-id"));
        let result = app
            .handle(
                &Request {
                    id: 1,
                    op: "retry_deletes".into(),
                    args: json!({"id":"active"}),
                },
                Arc::default(),
                &Arc::new(Mutex::new(std::io::stdout())),
            )
            .unwrap();
        assert_eq!(result["eligible"], 1);
        assert_eq!(result["cloud_delete"], "unconfirmed");
        assert_eq!(db.pending_deletes().unwrap().len(), 1);
        assert_eq!(
            db.pending_deletes().unwrap()[0].remote_id,
            "private-cloud-id"
        );
        assert_eq!(db.pending_deletes().unwrap()[0].next_retry_unix, 0);
        assert_eq!(app.cache(&active, true).unwrap()["deletes_pending"], 1);
    }
    #[test]
    fn connected_account_can_manage_other_profiles_without_changing_active_revision() {
        let temp = tempfile::tempdir().unwrap();
        let active = Profile {
            id: "active".into(),
            revision: "active-revision".into(),
            name: "Active".into(),
            ..Default::default()
        };
        let state = State::new(temp.path().join("private")).unwrap();
        let mut app = App {
            args: Args {
                state: state.dir.clone(),
                peer_bin: PathBuf::new(),
                launcher: PathBuf::new(),
                mount_worker: None,
                backup_worker: None,
                native_local_fixture_root: None,
            },
            state,
            config: Config {
                profiles: vec![active.clone()],
                selected: Some(active.id.clone()),
            },
            connected: Some(active),
            backend: Some(Backend::new(OpaqueProvider::default())),
            gateway: None,
            mounted: None,
            backup: None,
            network: Arc::default(),
            hosting: sharing::Hosting::default(),
            inactive: HashMap::new(),
            jobs: HashMap::new(),
            native_fixture: local_files::NativeFixture::default(),
        };
        let session = json!({"kind":app.connected.as_ref().unwrap().kind,
            "revision":"active-revision","account_name":"Actual signed in person",
            "binding":"private-binding","recipe":{"password":"private-secret"}});
        app.state.save("session-active.json", &session).unwrap();
        assert_eq!(
            app.status().unwrap()["account_name"],
            "Actual signed in person"
        );
        let public = app.public_profiles();
        assert_eq!(
            public["profiles"][0]["account_name"],
            "Actual signed in person"
        );
        assert!(!public.to_string().contains("private-secret"));
        assert!(!public.to_string().contains("private-binding"));
        assert_eq!(public["profiles"][0]["startup_ready"], true);
        let mut stale = session.clone();
        stale["revision"] = json!("old-revision");
        app.state.save("session-active.json", &stale).unwrap();
        assert!(app.status().unwrap()["account_name"].is_null());
        assert_eq!(app.public_profiles()["profiles"][0]["startup_ready"], false);
        stale = session;
        stale["kind"] = json!("another-provider");
        app.state.save("session-active.json", &stale).unwrap();
        assert!(app.public_profiles()["profiles"][0]["account_name"].is_null());
        assert_eq!(app.public_profiles()["profiles"][0]["startup_ready"], false);
        let out = Arc::new(Mutex::new(std::io::stdout()));
        let mut other = json!({"id":"other","name":"Other","kind":"dav","url":"https://example.invalid/dav/","mount_path":temp.path().join("mount"),"network_route":"system"});
        let call = |app: &mut App, op: &str, args: Value| {
            app.handle(
                &Request {
                    id: 1,
                    op: op.into(),
                    args,
                },
                Arc::default(),
                &out,
            )
        };
        call(&mut app, "save_profile", other.clone()).unwrap();
        other["name"] = json!("Edited other");
        call(&mut app, "save_profile", other.clone()).unwrap();
        assert_eq!(app.profile("other").unwrap().name, "Edited other");
        other["id"] = json!("active");
        assert!(call(&mut app, "save_profile", other).is_err());
        assert!(call(&mut app, "remove_profile", json!({"id":"active"})).is_err());
        call(&mut app, "remove_profile", json!({"id":"other"})).unwrap();
        assert_eq!(app.connected.as_ref().unwrap().revision, "active-revision");
        assert_eq!(app.config.selected.as_deref(), Some("active"));
        assert_eq!(app.profile("active").unwrap().revision, "active-revision");
        assert!(app.backend.is_some());

        // Inject a persistence failure after the online happy path: no failed
        // add/edit/remove may change the in-memory configuration or binding.
        let other = json!({"id":"other","name":"Unchanged","kind":"dav","url":"https://example.invalid/dav/","mount_path":temp.path().join("mount"),"network_route":"system"});
        call(&mut app, "save_profile", other.clone()).unwrap();
        let before = serde_json::to_value(&app.config).unwrap();
        fs::remove_file(app.state.dir.join("profiles.json")).unwrap();
        fs::create_dir(app.state.dir.join("profiles.json")).unwrap();
        let mut edited = other;
        edited["name"] = json!("Changed");
        for (op, args) in [
            (
                "save_profile",
                json!({"id":"new","name":"New","kind":"dav","url":"https://example.invalid/dav/","mount_path":temp.path().join("mount"),"network_route":"system"}),
            ),
            ("save_profile", edited),
            ("remove_profile", json!({"id":"other"})),
        ] {
            assert!(call(&mut app, op, args).is_err());
            assert_eq!(serde_json::to_value(&app.config).unwrap(), before);
            assert_eq!(app.connected.as_ref().unwrap().revision, "active-revision");
            assert!(app.backend.is_some());
        }
    }
    #[test]
    fn ipc_file_service_works_with_non_dav_opaque_ids() {
        let temp = tempfile::tempdir().unwrap();
        let provider = OpaqueProvider::default().with_caps(twodrive_dev::provider::Capabilities {
            upload: true,
            mkdir: true,
            rename: true,
            delete: true,
            conditional_upload: true,
        });
        let p = Profile {
            id: "opaque".into(),
            revision: "revision".into(),
            kind: "opaque".into(),
            writable: true,
            ..Default::default()
        };
        let state = State::new(temp.path().join("private")).unwrap();
        let mut app = App {
            args: Args {
                state: state.dir.clone(),
                peer_bin: PathBuf::new(),
                launcher: PathBuf::new(),
                mount_worker: None,
                backup_worker: None,
                native_local_fixture_root: None,
            },
            state,
            config: Config::default(),
            connected: Some(p),
            backend: Some(Backend::new(provider)),
            gateway: None,
            mounted: None,
            backup: None,
            network: Arc::default(),
            hosting: sharing::Hosting::default(),
            inactive: HashMap::new(),
            jobs: HashMap::new(),
            native_fixture: local_files::NativeFixture::default(),
        };
        let source = temp.path().join("source");
        fs::write(&source, b"opaque IPC bytes").unwrap();
        let output = temp.path().join("download");
        let out = Arc::new(Mutex::new(std::io::stdout()));
        let mut call = |op: &str, mut args: Value| {
            args["profile"] = json!("opaque");
            args["revision"] = json!("revision");
            app.handle(
                &Request {
                    id: 1,
                    op: op.into(),
                    args,
                },
                Arc::default(),
                &out,
            )
        };
        call("mkdir", json!({"path":"/folder"})).unwrap();
        call("upload", json!({"path":"/folder/file","source":source})).unwrap();
        assert_eq!(
            call("list", json!({"path":"/"})).unwrap()["entries"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        call("download", json!({"path":"/folder/file","output":output})).unwrap();
        assert_eq!(fs::read(&output).unwrap(), b"opaque IPC bytes");
        call(
            "rename",
            json!({"path":"/folder/file","to":"/folder/moved"}),
        )
        .unwrap();
        call("delete", json!({"path":"/folder/moved"})).unwrap();
        assert!(
            call("list", json!({"path":"/folder"})).unwrap()["entries"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
