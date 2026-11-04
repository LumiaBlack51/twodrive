//! iCloud adapter with opt-in manual writes through the bundled, separately versioned Rclone.
//! Credentials remain private; the generic mount bridge uses this Provider.
use crate::{
    model::{CloudBackend, DeltaResult, MetadataEntry},
    provider::{Capabilities, Progress, Provider, valid_path},
    state::{State, random_secret},
};
use anyhow::{Context, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashSet, VecDeque},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const CONFIG: &str = "rclone.conf";
const KEY: &str = "config-key.secret";
const MANIFEST: &str = "auth.json";
const HEADER: &[u8] = b"# Encrypted rclone configuration File";
const JSON_LIMIT: usize = 16 * 1024 * 1024;

mod gui;
mod quota;
mod times;
mod write;
pub use gui::action as gui_auth;

#[derive(Clone, Serialize, Deserialize)]
pub struct Auth {
    pub kind: String,
    pub binding: String,
    pub ready: bool,
}

pub fn binary() -> anyhow::Result<PathBuf> {
    let path = std::env::var_os("TWODRIVE_RCLONE_BIN")
        .map(PathBuf::from)
        .unwrap_or(
            std::env::current_exe()?
                .parent()
                .context("missing bundle")?
                .join(if cfg!(windows) {
                    "rclone.exe"
                } else {
                    "rclone"
                }),
        );
    ensure!(
        path.is_absolute() && path.is_file(),
        "缺少网盘访问组件，请使用完整的新版本目录"
    );
    Ok(path)
}

fn private_lock(state: &State, name: &str) -> anyhow::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let path = state.dir.join(name);
    if path.exists() {
        ensure!(
            !fs::symlink_metadata(&path)?.is_symlink(),
            "拒绝链接形式的登录锁"
        );
    }
    Ok(options.open(path)?)
}

fn wait_for_operation(
    state: &State,
    timeout: Duration,
    mut check: impl FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<File> {
    let lock = private_lock(state, "operation.lock")?;
    let start = Instant::now();
    loop {
        check()?;
        match lock.try_lock_exclusive() {
            Ok(()) => return Ok(lock),
            // fs2 returns ERROR_LOCK_VIOLATION (33) on Windows, whose Rust
            // ErrorKind is Uncategorized, not WouldBlock. Compare the native
            // contention error supplied by this library on each platform.
            Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                ensure!(start.elapsed() < timeout, "账号操作等待超时，请稍后重试");
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => bail!("无法锁定账号操作（系统错误 {:?}）", e.raw_os_error()),
        }
    }
}

fn password(state: &State) -> anyhow::Result<String> {
    String::from_utf8(twodrive_core::private_file::read_secret(
        &state.dir.join(KEY),
    )?)
    .context("无法读取本机加密密钥，请重新登录")
}

fn command(exe: &Path, state: &State, pass: &str, route: &str) -> Command {
    let mut command = Command::new(exe);
    // Prevent ambient remotes/endpoint overrides from replacing this account.
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("RCLONE_")
        {
            command.env_remove(key);
        }
    }
    if route == "direct" {
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            command.env_remove(key);
        }
    }
    command
        .env("RCLONE_CONFIG_PASS", pass)
        .env("RCLONE_CACHE_DIR", state.dir.join("cache"))
        .args(["--config"])
        .arg(state.dir.join(CONFIG))
        .args([
            "--ask-password=false",
            "--log-level=ERROR",
            "--stats=0",
            "--retries=1",
            "--low-level-retries=1",
            "--contimeout=15s",
            "--timeout=30s",
        ]);
    command
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[derive(Debug, thiserror::Error)]
#[error("网盘访问失败（组件代码 {code}）；请检查登录状态和网络，已有数据已保留")]
struct RemoteError {
    code: i32,
}

/// Bound output/latency, stream at most 64 KiB per chunk, and reap on cancellation.
fn stream(
    mut cmd: Command,
    writer: &mut dyn Write,
    mut progress: impl FnMut(u64) -> anyhow::Result<()>,
    limit: u64,
    cancel: &AtomicBool,
) -> anyhow::Result<u64> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW; never auth from a read.
    }
    let mut child = OwnedChild(
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("无法启动网盘访问组件")?,
    );
    let mut stdout = child.0.stdout.take().context("missing component pipe")?;
    let (send, receive) = mpsc::sync_channel(2);
    let reader = thread::spawn(move || {
        let mut buffer = [0u8; 65536];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    if send.send(Ok(buffer[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    let _ = send.send(Err(()));
                    break;
                }
            }
        }
    });
    let result = (|| -> anyhow::Result<u64> {
        let start = Instant::now();
        let budget = Duration::from_secs(if limit == u64::MAX {
            7200
        } else if limit == JSON_LIMIT as u64 {
            45
        } else {
            300
        });
        let mut done = 0u64;
        loop {
            ensure!(!cancel.load(Ordering::Relaxed), "连接已取消");
            ensure!(start.elapsed() < budget, "网盘操作超时，未发布未完成的下载");
            progress(done)?;
            match receive.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(bytes)) => {
                    done = done
                        .checked_add(bytes.len() as u64)
                        .context("component output overflow")?;
                    ensure!(done <= limit, "网盘返回内容超过预期限制，未发布下载");
                    writer.write_all(&bytes)?;
                }
                Ok(Err(())) => bail!("读取网盘组件失败，未发布下载"),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let status = loop {
            ensure!(!cancel.load(Ordering::Relaxed), "连接已取消");
            ensure!(start.elapsed() < budget, "网盘操作超时");
            if let Some(status) = child.0.try_wait()? {
                break status;
            }
            progress(done)?;
            thread::sleep(Duration::from_millis(25));
        };
        ensure!(
            status.success(),
            RemoteError {
                code: status.code().unwrap_or(-1)
            }
        );
        progress(done)?;
        Ok(done)
    })();
    if result.is_err() {
        let _ = child.0.kill();
    }
    drop(receive);
    let _ = child.0.wait();
    let _ = reader.join();
    result
}

fn json_command(
    exe: &Path,
    state: &State,
    pass: &str,
    route: &str,
    args: &[&str],
    cancel: &AtomicBool,
) -> anyhow::Result<Value> {
    let mut cmd = command(exe, state, pass, route);
    cmd.args(args);
    let mut bytes = Vec::new();
    stream(cmd, &mut bytes, |_| Ok(()), JSON_LIMIT as u64, cancel)?;
    serde_json::from_slice(&bytes).context("网盘组件返回的元数据格式无效")
}

fn check_config(
    exe: &Path,
    state: &State,
    pass: &str,
    route: &str,
    cancel: &AtomicBool,
) -> anyhow::Result<String> {
    let path = state.dir.join(CONFIG);
    ensure!(
        !fs::symlink_metadata(&path)?.is_symlink(),
        "拒绝链接形式的登录配置"
    );
    let bytes = fs::read(path)?;
    ensure!(
        bytes.starts_with(HEADER),
        "登录配置未加密，拒绝使用；请重新运行登录助手"
    );
    // Read into memory only; neither the config nor stderr enters public IPC.
    let config = json_command(exe, state, pass, route, &["config", "dump"], cancel)?;
    let remote = &config["twodrive"];
    ensure!(
        remote["type"] == "iclouddrive",
        "登录目录不是 iCloud Drive 账号"
    );
    ensure!(
        matches!(remote["service"].as_str(), None | Some("") | Some("drive")),
        "当前连接仅支持 iCloud Drive，请选择 Drive 服务"
    );
    for key in ["auth_url", "token_url", "url", "endpoint"] {
        ensure!(
            remote[key].as_str().unwrap_or_default().is_empty(),
            "拒绝自定义认证端点"
        );
    }
    let account = remote["apple_id"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .context("缺少 Apple 账号，请完成登录")?;
    Ok(format!(
        "{:x}",
        Sha256::digest(account.trim().to_lowercase().as_bytes())
    ))
}

pub struct RcloneDrive {
    state: State,
    exe: PathBuf,
    pass: String,
    route: String,
    pub binding: String,
    writable: bool,
    created_times: times::CreationTimes,
    _account_lock: File,
    cancel: Arc<AtomicBool>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Snapshot {
    binding: String,
    id: String,
    path: String,
    size: u64,
    version: String,
    directory: bool,
}
impl Snapshot {
    fn encode(&self) -> anyhow::Result<String> {
        Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(self)?))
    }
    fn decode(id: &str, binding: &str) -> anyhow::Result<Self> {
        ensure!(id.len() <= 32768, "invalid object snapshot");
        let snapshot: Self = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(id)?)?;
        valid_path(&snapshot.path)?;
        ensure!(
            snapshot.binding == binding && !snapshot.id.is_empty(),
            "文件属于其他账号或旧登录，请刷新"
        );
        Ok(snapshot)
    }
}

impl RcloneDrive {
    pub fn connect(state: State, route: &str, cancel: &AtomicBool) -> anyhow::Result<Self> {
        ensure!(matches!(route, "system" | "direct"), "unsupported route");
        let account_lock = private_lock(&state, "auth.lock")?;
        FileExt::try_lock_shared(&account_lock).context("登录助手正在运行，请完成后再连接")?;
        ensure!(
            !gui::pending(&state),
            "请先完成或取消客户端中的 iCloud 登录"
        );
        let auth: Auth = state
            .read(MANIFEST)
            .context("请先使用“登录 iCloud”完成 Apple 账号和双重认证")?;
        ensure!(
            auth.ready && auth.kind == "icloud",
            "请先完成 iCloud 登录；原有数据未删除"
        );
        let exe = binary()?;
        let pass = password(&state)?;
        let identity = check_config(&exe, &state, &pass, route, cancel)?;
        ensure!(
            auth.binding.starts_with(&format!("icloud:{identity}:")),
            "登录账号已改变，请重新授权并刷新连接"
        );
        let values = json_command(
            &exe,
            &state,
            &pass,
            route,
            &["lsjson", "twodrive:", "--no-mimetype", "--max-depth=1"],
            cancel,
        )?;
        let backend = Self {
            state,
            exe,
            pass,
            route: route.into(),
            binding: auth.binding,
            writable: false,
            created_times: times::CreationTimes::default(),
            _account_lock: account_lock,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        backend.entries("/", &values)?;
        Ok(backend)
    }

    #[cfg(test)]
    fn operation_lock(&self) -> anyhow::Result<File> {
        wait_for_operation(&self.state, Duration::from_secs(45), || {
            ensure!(!self.cancel.load(Ordering::Relaxed), "连接已取消");
            Ok(())
        })
    }

    fn query(&self, args: &[&str]) -> anyhow::Result<Value> {
        self.query_checked(args, &mut || Ok(()))
    }
    fn query_checked(
        &self,
        args: &[&str],
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Value> {
        let _lock = wait_for_operation(&self.state, Duration::from_secs(45), || {
            ensure!(!self.cancel.load(Ordering::Relaxed), "连接已取消");
            check()
        })?;
        let auth: Auth = self.state.read(MANIFEST)?;
        ensure!(
            auth.ready && auth.binding == self.binding,
            "登录状态已改变，拒绝旧连接"
        );
        let mut cmd = command(&self.exe, &self.state, &self.pass, &self.route);
        cmd.args(args);
        let mut bytes = Vec::new();
        stream(
            cmd,
            &mut bytes,
            |_| check(),
            JSON_LIMIT as u64,
            &self.cancel,
        )?;
        check()?;
        serde_json::from_slice(&bytes).context("网盘组件返回的元数据格式无效")
    }

    fn metadata_with(
        &self,
        path: &str,
        query: impl FnOnce(&[&str]) -> anyhow::Result<Value>,
    ) -> anyhow::Result<Option<MetadataEntry>> {
        valid_path(path)?;
        if path == "/" {
            return self
                .entry("/", &serde_json::json!({"ID":"root","IsDir":true,"Size":0}))
                .map(Some);
        }
        // Rooting an Fs at an existing directory makes lsjson --stat return
        // Rclone's synthetic root (no ID, generated timestamp). Resolve the
        // child from one shallow parent listing instead, retaining the real
        // object identity/version for both directories and files.
        let (parent, _) = path.rsplit_once('/').context("invalid cloud path")?;
        let parent = if parent.is_empty() { "/" } else { parent };
        let remote = format!("twodrive:{}", parent.trim_start_matches('/'));
        match query(&["lsjson", &remote, "--no-mimetype", "--max-depth=1"]) {
            Ok(value) => {
                let mut matches = self
                    .entries(parent, &value)?
                    .into_iter()
                    .filter(|entry| entry.path == path || entry.path.eq_ignore_ascii_case(path));
                let entry = matches.next();
                ensure!(
                    matches.next().is_none(),
                    "目录含大小写歧义，拒绝不确定的文件"
                );
                Ok(entry)
            }
            Err(e)
                if e.downcast_ref::<RemoteError>()
                    .is_some_and(|e| matches!(e.code, 3 | 4)) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn entry(&self, path: &str, value: &Value) -> anyhow::Result<MetadataEntry> {
        valid_path(path)?;
        let directory = value["IsDir"].as_bool().context("missing directory flag")?;
        let size = if directory {
            0
        } else {
            value["Size"].as_u64().context("invalid file size")?
        };
        let id = value["ID"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("网盘未返回稳定文件标识，拒绝不确定的文件")?;
        // This is a read snapshot, never an atomic write precondition.
        let version = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&serde_json::json!([
                id,
                value["ModTime"],
                size,
                value["Hashes"]
            ]))?)
        );
        let snapshot = Snapshot {
            binding: self.binding.clone(),
            id: id.into(),
            path: path.into(),
            size,
            version: version.clone(),
            directory,
        };
        let modified = value["ModTime"]
            .as_str()
            .and_then(|s| {
                time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
            })
            .map_or(0, |t| t.unix_timestamp());
        let entry = if directory {
            MetadataEntry::new_dir(snapshot.encode()?, path, modified, version)
        } else {
            MetadataEntry::new_file(snapshot.encode()?, path, size, modified, version)
        };
        self.with_created_time(entry, &snapshot.id)
    }
    fn entries(&self, path: &str, values: &Value) -> anyhow::Result<Vec<MetadataEntry>> {
        valid_path(path)?;
        let rows = values.as_array().context("invalid directory response")?;
        ensure!(rows.len() <= 100000, "目录项目过多，未返回不完整列表");
        let mut names = HashSet::new();
        rows.iter()
            .map(|row| {
                let name = row["Name"].as_str().context("missing name")?;
                ensure!(
                    !name.is_empty()
                        && !name.contains('/')
                        && row["Path"] == name
                        && names.insert(name),
                    "目录含重复或越界路径，未返回不完整列表"
                );
                self.entry(&format!("{}/{}", path.trim_end_matches('/'), name), row)
            })
            .collect()
    }
}

impl Provider for RcloneDrive {
    fn browse_cache_ttl(&self) -> Duration {
        Duration::from_secs(2)
    }
    fn storage_usage(
        &self,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Option<crate::provider::StorageUsage>> {
        quota::read(self, check)
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            upload: self.writable,
            mkdir: self.writable,
            rename: self.writable,
            delete: self.writable,
            conditional_upload: false,
        }
    }
    fn manual_upload_supported(&self) -> bool {
        self.writable
    }
    fn manual_upload(
        &self,
        path: &str,
        source: &Path,
        id: Option<&str>,
        version: Option<&str>,
        progress: Progress,
    ) -> anyhow::Result<MetadataEntry> {
        self.put_manual(path, source, id, version, progress)
    }
    fn list_directory(&self, path: &str) -> anyhow::Result<Vec<MetadataEntry>> {
        self.list_directory_checked(path, &mut || Ok(()))
    }
    fn list_directory_checked(
        &self,
        path: &str,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Vec<MetadataEntry>> {
        valid_path(path)?;
        let remote = format!("twodrive:{}", path.trim_start_matches('/'));
        let values = self.query_checked(
            &["lsjson", &remote, "--no-mimetype", "--max-depth=1"],
            check,
        )?;
        self.entries(path, &values)
    }
    fn upload_controlled(
        &self,
        _: &str,
        _: &Path,
        _: Option<&str>,
        _: Option<&str>,
        _: Progress,
    ) -> anyhow::Result<MetadataEntry> {
        bail!("iCloud 当前仅支持浏览和下载，未启用条件写入")
    }
}

impl CloudBackend for RcloneDrive {
    fn is_conflict_error(&self, _: &anyhow::Error) -> bool {
        false
    }
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        let mut out = Vec::new();
        let mut queue = VecDeque::from(["/".to_owned()]);
        let mut seen = HashSet::new();
        while let Some(path) = queue.pop_front() {
            ensure!(
                seen.insert(path.clone()) && seen.len() <= 10000,
                "目录遍历超出预算"
            );
            for entry in self.list_directory(&path)? {
                if entry.is_dir {
                    queue.push_back(entry.path.clone());
                }
                out.push(entry);
                ensure!(out.len() <= 100000, "文件索引超出预算，未发布不完整结果");
            }
        }
        Ok(out)
    }
    fn list_delta(&self, _: Option<&str>) -> anyhow::Result<DeltaResult> {
        bail!("iCloud 适配器未提供可靠增量流，请重新读取完整索引")
    }
    fn get_metadata_by_path(&self, path: &str) -> anyhow::Result<Option<MetadataEntry>> {
        self.metadata_with(path, |args| self.query(args))
    }
    fn get_metadata(&self, id: &str) -> anyhow::Result<Option<MetadataEntry>> {
        let snapshot = Snapshot::decode(id, &self.binding)?;
        let current = self.get_metadata_by_path(&snapshot.path)?;
        ensure!(
            current.as_ref().is_none_or(|m| m.remote_id == id),
            "远端文件已替换或更新，请刷新"
        );
        Ok(current)
    }
    fn download_to(
        &self,
        id: &str,
        writer: &mut dyn Write,
        progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        let snapshot = Snapshot::decode(id, &self.binding)?;
        ensure!(
            !snapshot.directory && self.get_metadata(id)?.is_some(),
            "文件已不存在或是目录"
        );
        let done = {
            let _lock = wait_for_operation(&self.state, Duration::from_secs(45), || {
                ensure!(!self.cancel.load(Ordering::Relaxed), "连接已取消");
                progress(0)
            })?;
            let mut cmd = command(&self.exe, &self.state, &self.pass, &self.route);
            cmd.args(["cat", &format!("twodrive:{}", &snapshot.path[1..])]);
            stream(cmd, writer, progress, snapshot.size, &self.cancel)?
        };
        ensure!(
            done == snapshot.size && self.get_metadata(id)?.is_some(),
            "下载期间文件变化，未发布下载"
        );
        Ok(done)
    }
    fn download(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        let snapshot = Snapshot::decode(id, &self.binding)?;
        ensure!(snapshot.size <= 16 * 1024 * 1024, "大文件请使用流式下载");
        let mut bytes = Vec::new();
        self.download_to(id, &mut bytes, &mut |_| Ok(()))?;
        Ok(bytes)
    }
    fn upload(&self, _: &str, _: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        bail!("iCloud 当前只读")
    }
    fn upload_with_etag(
        &self,
        _: &str,
        _: Vec<u8>,
        _: Option<&str>,
    ) -> anyhow::Result<MetadataEntry> {
        bail!("iCloud 当前只读")
    }
    fn upload_file_with_version(
        &self,
        _: &str,
        _: &Path,
        _: Option<&str>,
        _: Option<&str>,
        _: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        bail!("iCloud 当前只读")
    }
    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        self.mkdir_manual(path)
    }
    fn rename(&self, id: &str, path: &str) -> anyhow::Result<MetadataEntry> {
        self.rename_manual(id, path)
    }
    fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.delete_manual(id)
    }
}

fn encrypted_config(exe: &Path, state: &State, pass: &str) -> anyhow::Result<()> {
    let cancel = AtomicBool::new(false);
    if !state.dir.join(CONFIG).exists() {
        let mut init = command(exe, state, pass, "system");
        init.args(["config", "touch"]);
        stream(
            init,
            &mut std::io::sink(),
            |_| Ok(()),
            JSON_LIMIT as u64,
            &cancel,
        )?;
        let mut encryption = command(exe, state, pass, "system");
        encryption.args(["config", "encryption", "set"]);
        let mut child = OwnedChild(
            encryption
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        );
        child
            .0
            .stdin
            .take()
            .context("missing encryption pipe")?
            .write_all(format!("{pass}\n{pass}\n").as_bytes())?;
        ensure!(child.0.wait()?.success(), "创建加密登录配置失败");
    }
    ensure!(
        fs::read(state.dir.join(CONFIG))?.starts_with(HEADER),
        "拒绝使用未加密配置"
    );
    Ok(())
}

fn login_command(exe: &Path, state: &State, pass: &str, exists: bool) -> Command {
    let mut login = command(exe, state, pass, "system");
    if exists {
        login.args(["config", "update", "twodrive"]);
    } else {
        login.args(["config", "create", "twodrive", "iclouddrive"]);
    }
    login.args(["--all", "--no-output"]);
    login
}

/// Human-operated terminal helper. Password/2FA are entered directly into Rclone.
pub fn configure(state: State) -> anyhow::Result<()> {
    let lock = private_lock(&state, "auth.lock")?;
    lock.try_lock_exclusive()
        .context("请先断开此账号，再更新授权")?;
    ensure!(!gui::pending(&state), "请先完成或取消客户端中的登录");
    if state.dir.join(MANIFEST).exists() {
        let existing: Auth = state.read(MANIFEST)?;
        ensure!(existing.kind == "icloud", "拒绝修改其他提供者的状态");
    }
    let exe = binary()?;
    if !state.dir.join(KEY).exists() {
        ensure!(
            !state.dir.join(CONFIG).exists(),
            "已有配置缺少加密密钥，拒绝覆盖"
        );
        twodrive_core::private_file::write_secret(
            &state.dir.join(KEY),
            random_secret().as_bytes(),
        )?;
    }
    let pass = password(&state)?;
    encrypted_config(&exe, &state, &pass)?;
    let cancel = AtomicBool::new(false);
    state.save(
        MANIFEST,
        &Auth {
            kind: "icloud".into(),
            binding: random_secret(),
            ready: false,
        },
    )?;
    println!(
        "请亲自输入 Apple 账号、密码和双重认证验证码。不要把密码或验证码发给助手。\n授权有效期及高级数据保护由 Apple 决定；失败时不会删除已有凭据。\n"
    );
    let stored = json_command(&exe, &state, &pass, "system", &["config", "dump"], &cancel)?;
    let exists = stored["twodrive"].is_object();
    ensure!(
        !exists || stored["twodrive"]["type"] == "iclouddrive",
        "此目录包含其他提供者，拒绝修改其授权"
    );
    let mut login = login_command(&exe, &state, &pass, exists);
    ensure!(
        login.status()?.success(),
        "Apple 登录未完成；已有凭据保留，可重试"
    );
    let identity = check_config(&exe, &state, &pass, "system", &cancel)?;
    json_command(
        &exe,
        &state,
        &pass,
        "system",
        &["lsjson", "twodrive:", "--max-depth=1", "--no-mimetype"],
        &cancel,
    )?;
    state.save(
        MANIFEST,
        &Auth {
            kind: "icloud".into(),
            binding: format!("icloud:{identity}:{}", random_secret()),
            ready: true,
        },
    )?;
    println!("iCloud 登录已完成。请返回 TwoDrive 保存连接，然后选择“登录并使用”。");
    Ok(())
}

pub fn sign_out(state: State) -> anyhow::Result<()> {
    let lock = private_lock(&state, "auth.lock")?;
    lock.try_lock_exclusive().context("请先断开账号")?;
    gui::clear(&state)?;
    if state.dir.join(MANIFEST).exists() {
        let mut auth: Auth = state.read(MANIFEST)?;
        auth.ready = false;
        auth.binding = random_secret();
        state.save(MANIFEST, &auth)?;
    }
    // The encrypted blob becomes unusable; no remote data/cache is removed.
    for name in [KEY, CONFIG] {
        let path = state.dir.join(name);
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> (tempfile::TempDir, RcloneDrive) {
        let temp = tempfile::tempdir().unwrap();
        let state = State::new(temp.path().join("private")).unwrap();
        let lock = private_lock(&state, "auth.lock").unwrap();
        FileExt::try_lock_shared(&lock).unwrap();
        let backend = RcloneDrive {
            state,
            exe: PathBuf::from("missing-no-execution"),
            pass: "synthetic".into(),
            route: "direct".into(),
            binding: "icloud:synthetic:epoch-a".into(),
            writable: false,
            created_times: times::CreationTimes::default(),
            _account_lock: lock,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        (temp, backend)
    }
    #[test]
    fn directory_metadata_uses_real_parent_entry_instead_of_synthetic_fs_root() {
        let (_temp, backend) = fixture();
        let listed = json!({"ID":"stable-folder#v1","Name":"folder","Path":"folder",
            "Size":-1,"IsDir":true,"ModTime":"2000-01-01T00:00:00Z"});
        // Official Rclone StatJSON(remote="") emits a synthetic root without
        // an ID. NewFs roots an existing directory at that path, even though
        // its parent listing has a genuine iCloud directory ID and version.
        let expected = backend.entry("/folder", &listed).unwrap();
        let actual = backend
            .metadata_with("/folder", |args| {
                if args.contains(&"--stat") {
                    Ok(json!({"Path":"","Name":"","Size":-1,"IsDir":true,
                    "ModTime":"2026-10-29T00:00:00Z"}))
                } else {
                    assert_eq!(
                        args,
                        ["lsjson", "twodrive:", "--no-mimetype", "--max-depth=1"]
                    );
                    Ok(json!([listed]))
                }
            })
            .unwrap()
            .unwrap();
        assert_eq!(actual.remote_id, expected.remote_id);
        assert_eq!(actual.etag, expected.etag);
        assert!(actual.is_dir);
    }
    #[test]
    fn shallow_parent_lookup_keeps_nested_identity_and_rejects_partial_or_ambiguous_results() {
        let (_temp, backend) = fixture();
        let rows = json!([
            {"ID":"folder#v1","Name":"子目录","Path":"子目录","IsDir":true,"Size":-1},
            {"ID":"file#v2","Name":"File.txt","Path":"File.txt","IsDir":false,"Size":12}
        ]);
        let lookup = |path: &str, values: Value| {
            backend.metadata_with(path, |args| {
                assert_eq!(
                    args,
                    [
                        "lsjson",
                        "twodrive:parent",
                        "--no-mimetype",
                        "--max-depth=1"
                    ]
                );
                Ok(values)
            })
        };
        assert!(
            lookup("/parent/子目录", rows.clone())
                .unwrap()
                .unwrap()
                .is_dir
        );
        let file = lookup("/parent/File.txt", rows.clone()).unwrap().unwrap();
        assert!(!file.is_dir);
        assert_eq!(
            lookup("/parent/file.txt", rows.clone())
                .unwrap()
                .unwrap()
                .remote_id,
            file.remote_id
        );
        assert!(lookup("/parent/missing", rows.clone()).unwrap().is_none());
        let mut changed = rows.clone();
        changed[1]["ID"] = json!("different-file#v3");
        assert_ne!(
            lookup("/parent/File.txt", changed)
                .unwrap()
                .unwrap()
                .remote_id,
            file.remote_id
        );
        let mut malformed = rows.clone();
        malformed
            .as_array_mut()
            .unwrap()
            .push(json!({"IsDir":true}));
        assert!(lookup("/parent/File.txt", malformed).is_err());
        let mut ambiguous = rows;
        ambiguous.as_array_mut().unwrap().push(
            json!({"ID":"other","Name":"FILE.txt","Path":"FILE.txt","IsDir":false,"Size":12}),
        );
        assert!(lookup("/parent/file.txt", ambiguous).is_err());
        assert!(
            backend
                .metadata_with("/parent/child", |_| Err(anyhow::anyhow!(
                    "synthetic transport failure"
                )))
                .is_err()
        );
        assert!(
            backend
                .metadata_with("/", |_| panic!("root lookup must stay local"))
                .unwrap()
                .unwrap()
                .is_dir
        );
    }
    #[test]
    fn manual_writes_are_opt_in_and_never_claim_atomic_conditional_support() {
        let (_root, backend) = fixture();
        assert!(!backend.manual_upload_supported());
        assert!(backend.create_folder("/locked").is_err());
        let enabled = backend.with_writable(true);
        assert!(enabled.capabilities().upload && enabled.manual_upload_supported());
        assert!(!enabled.capabilities().conditional_upload && !enabled.capabilities().backup());
    }
    fn row() -> Value {
        json!({"ID":"opaque-cloud-id","Name":"中文.bin","Path":"中文.bin","Size":12,"IsDir":false,"ModTime":"2026-10-28T00:00:00Z"})
    }
    #[test]
    fn directory_wait_honors_request_cancel_without_cancelling_the_connection() {
        let (_temp, backend) = fixture();
        let held = private_lock(&backend.state, "operation.lock").unwrap();
        held.try_lock_exclusive().unwrap();
        let start = Instant::now();
        let error = backend
            .list_directory_checked("/", &mut || {
                ensure!(
                    start.elapsed() < Duration::from_millis(80),
                    "display read cancelled"
                );
                Ok(())
            })
            .unwrap_err();
        assert!(error.to_string().contains("display read cancelled"));
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(!backend.cancel.load(Ordering::Relaxed));
        let probe = private_lock(&backend.state, "operation.lock").unwrap();
        assert!(
            probe.try_lock_exclusive().is_err(),
            "cancellation must not release another operation's lock"
        );
    }
    #[test]
    fn stalled_component_read_is_reaped_on_request_check_failure() {
        #[cfg(windows)]
        let cmd = {
            let mut c = Command::new("powershell.exe");
            c.args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ]);
            c
        };
        #[cfg(not(windows))]
        let cmd = {
            let mut c = Command::new("sh");
            c.args(["-c", "exec sleep 30"]);
            c
        };
        let start = Instant::now();
        let error = stream(
            cmd,
            &mut Vec::new(),
            |_| {
                ensure!(
                    start.elapsed() < Duration::from_millis(150),
                    "display read cancelled"
                );
                Ok(())
            },
            JSON_LIMIT as u64,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.to_string().contains("display read cancelled"));
        assert!(start.elapsed() < Duration::from_secs(3));
    }
    #[test]
    fn operation_lock_waits_for_another_reader_then_acquires() {
        let (_temp, backend) = fixture();
        let held = private_lock(&backend.state, "operation.lock").unwrap();
        held.try_lock_exclusive().unwrap();
        let probe = private_lock(&backend.state, "operation.lock").unwrap();
        let error = probe.try_lock_exclusive().unwrap_err();
        assert_eq!(
            error.raw_os_error(),
            fs2::lock_contended_error().raw_os_error()
        );
        eprintln!(
            "local lock contention: kind={:?}, code={:?}",
            error.kind(),
            error.raw_os_error()
        );
        let (started, ready) = mpsc::channel();
        let (send, receive) = mpsc::channel();
        let worker = thread::spawn(move || {
            started.send(()).unwrap();
            let result = backend.operation_lock().map(|_lock| ());
            send.send(result).unwrap();
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let early = receive.recv_timeout(Duration::from_millis(120));
        drop(held);
        assert!(
            matches!(early, Err(mpsc::RecvTimeoutError::Timeout)),
            "a contended reader must wait instead of reporting account busy"
        );
        receive
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn operation_lock_wait_is_bounded_and_cancellation_does_not_release_owner() {
        let (_temp, backend) = fixture();
        let held = private_lock(&backend.state, "operation.lock").unwrap();
        held.try_lock_exclusive().unwrap();
        let start = Instant::now();
        let mut checks = 0;
        let error = wait_for_operation(&backend.state, Duration::from_secs(2), || {
            checks += 1;
            ensure!(checks < 3, "synthetic cancellation");
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("synthetic cancellation"));
        assert!(start.elapsed() < Duration::from_secs(1));
        let error =
            wait_for_operation(&backend.state, Duration::from_millis(60), || Ok(())).unwrap_err();
        assert!(error.to_string().contains("等待超时"));
        let probe = private_lock(&backend.state, "operation.lock").unwrap();
        assert!(
            probe.try_lock_exclusive().is_err(),
            "cancel must not unlock another operation"
        );
        drop(held);
        assert!(probe.try_lock_exclusive().is_ok());
    }
    #[test]
    fn read_snapshots_bind_object_version_path_and_login_generation() {
        let (_temp, backend) = fixture();
        let original = backend.entry("/中文.bin", &row()).unwrap();
        assert_ne!(original.remote_id, original.path);
        let mut changed = row();
        changed["ID"] = json!("replacement-cloud-id");
        assert_ne!(
            original.remote_id,
            backend.entry("/中文.bin", &changed).unwrap().remote_id
        );
        changed = row();
        changed["ModTime"] = json!("2026-10-28T00:00:01Z");
        assert_ne!(
            original.etag,
            backend.entry("/中文.bin", &changed).unwrap().etag
        );
        assert!(Snapshot::decode(&original.remote_id, "icloud:synthetic:epoch-b").is_err());
        let mut snapshot = Snapshot::decode(&original.remote_id, &backend.binding).unwrap();
        snapshot.path = "/../outside".into();
        assert!(Snapshot::decode(&snapshot.encode().unwrap(), &backend.binding).is_err());
    }
    #[test]
    fn known_directory_creation_time_changes_display_only_and_survives_rename() {
        let (_temp, backend) = fixture();
        let mut row = json!({"ID":"real-folder#v1", "IsDir":true,
            "ModTime":"2000-01-01T00:00:00Z"});
        let original = backend.entry("/folder", &row).unwrap();
        assert_eq!(original.created_unix, None);
        let created = backend
            .remember_created(original.clone(), 1800000000)
            .unwrap();
        assert_eq!(created.created_unix, Some(1800000000));
        assert_eq!(created.modified_unix, 1800000000);
        assert_eq!(created.remote_id, original.remote_id);
        assert_eq!(created.etag, original.etag);
        row["ID"] = json!("real-folder#v2");
        let renamed = backend.entry("/renamed", &row).unwrap();
        assert_eq!(renamed.created_unix, Some(1800000000));
        row["ModTime"] = json!("2026-10-28T00:00:01Z");
        assert_eq!(
            backend.entry("/renamed", &row).unwrap().modified_unix,
            1793145601
        );
        row["ID"] = json!("replacement-id#v2");
        assert_eq!(backend.entry("/renamed", &row).unwrap().created_unix, None);
        fs::write(backend.state.dir.join("created-times.json"), b"bad").unwrap();
        assert!(backend.entry("/renamed", &row).is_ok());
    }
    #[test]
    fn malformed_listing_cannot_become_a_partial_success() {
        let (_temp, backend) = fixture();
        assert_eq!(
            backend.entries("/folder", &json!([row()])).unwrap()[0].path,
            "/folder/中文.bin"
        );
        assert!(backend.entries("/", &json!([row(), row()])).is_err());
        let mut escaped = row();
        escaped["Path"] = json!("../中文.bin");
        assert!(backend.entries("/", &json!([row(), escaped])).is_err());
        let mut unknown_id = row();
        unknown_id["ID"] = json!("");
        assert!(backend.entries("/", &json!([unknown_id])).is_err());
    }
    #[test]
    fn unavailable_mutations_and_old_sessions_fail_before_component_execution() {
        let (_temp, backend) = fixture();
        assert!(!backend.capabilities().upload && !backend.capabilities().backup());
        assert!(backend.upload("/file", vec![]).is_err());
        assert!(backend.rename("id", "/new").is_err());
        assert!(backend.delete("id").is_err());
        backend
            .state
            .save(
                MANIFEST,
                &Auth {
                    kind: "icloud".into(),
                    binding: "different-generation".into(),
                    ready: true,
                },
            )
            .unwrap();
        assert!(
            backend
                .query(&["lsjson", "twodrive:"])
                .unwrap_err()
                .to_string()
                .contains("旧连接")
        );
        let exclusive = private_lock(&backend.state, "auth.lock").unwrap();
        assert!(
            exclusive.try_lock_exclusive().is_err(),
            "connected account must veto reconfiguration"
        );
    }
    #[test]
    #[ignore = "explicit official local Rclone executable required; no cloud access"]
    fn official_component_encryption_streaming_and_cancellation() {
        let exe = PathBuf::from(
            std::env::var_os("TWODRIVE_TEST_RCLONE")
                .expect("official test component path required"),
        );
        let temp = tempfile::tempdir().unwrap();
        let state = State::new(temp.path().join("private")).unwrap();
        let pass = random_secret();
        twodrive_core::private_file::write_secret(&state.dir.join(KEY), pass.as_bytes()).unwrap();
        assert_eq!(password(&state).unwrap(), pass);
        encrypted_config(&exe, &state, &pass).unwrap();
        let config = json_command(
            &exe,
            &state,
            &pass,
            "direct",
            &["config", "dump"],
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(config, json!({}));
        // Ask only for the first required field, without supplying any identity
        // or contacting Apple. The old missing --all form failed before asking.
        let mut prompt = login_command(&exe, &state, &pass, false);
        prompt.args(["--non-interactive", "--no-output=false"]);
        let mut question = Vec::new();
        stream(
            prompt,
            &mut question,
            |_| Ok(()),
            JSON_LIMIT as u64,
            &AtomicBool::new(false),
        )
        .unwrap();
        let question: Value = serde_json::from_slice(&question).unwrap();
        assert_eq!(question["Option"]["Name"], "service");
        assert_eq!(question["Option"]["Default"], "drive");
        assert_ne!(question["State"], "");
        let mut next = login_command(&exe, &state, &pass, true);
        next.args([
            "--non-interactive",
            "--no-output=false",
            "--continue",
            "--state",
            question["State"].as_str().unwrap(),
            "--result",
            "drive",
        ]);
        let mut question = Vec::new();
        stream(
            next,
            &mut question,
            |_| Ok(()),
            JSON_LIMIT as u64,
            &AtomicBool::new(false),
        )
        .unwrap();
        let question: Value = serde_json::from_slice(&question).unwrap();
        assert_eq!(question["Option"]["Name"], "apple_id");
        json_command(
            &exe,
            &state,
            &pass,
            "direct",
            &[
                "config",
                "create",
                "twodrive",
                "memory",
                "--non-interactive",
            ],
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(
            check_config(&exe, &state, &pass, "direct", &AtomicBool::new(false)).is_err(),
            "a different backend cannot masquerade as iCloud"
        );
        assert!(
            !String::from_utf8_lossy(&fs::read(state.dir.join(CONFIG)).unwrap()).contains("memory")
        );
        let file = temp.path().join("generated.bin");
        let bytes = vec![42u8; 2 * 1024 * 1024];
        fs::write(&file, &bytes).unwrap();
        let mut cmd = command(&exe, &state, &pass, "direct");
        cmd.arg("cat").arg(&file);
        let mut output = Vec::new();
        assert_eq!(
            stream(
                cmd,
                &mut output,
                |_| Ok(()),
                bytes.len() as u64,
                &AtomicBool::new(false)
            )
            .unwrap(),
            bytes.len() as u64
        );
        assert_eq!(output, bytes);
        let mut cmd = command(&exe, &state, &pass, "direct");
        cmd.arg("cat").arg(&file);
        let mut partial = Vec::new();
        assert!(
            stream(
                cmd,
                &mut partial,
                |done| {
                    ensure!(done == 0, "synthetic cancellation");
                    Ok(())
                },
                bytes.len() as u64,
                &AtomicBool::new(false)
            )
            .is_err()
        );
        assert!(partial.len() < bytes.len());
        assert_eq!(fs::read(file).unwrap(), bytes);
    }
}
