//! Explicit desktop hosting, separate from the active storage connection.
use super::{Args, Process, drain_errors, spawn, valid_id};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use twodrive_dev::{
    network::Diagnostics,
    state::{Share, State},
};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    id: String,
    name: String,
    root: PathBuf,
    writable: bool,
    discoverable: bool,
    network: String,
}
fn load(state: &State) -> anyhow::Result<Settings> {
    if state.dir.join("hosting.json").exists() {
        state.read("hosting.json")
    } else {
        Ok(Settings::default())
    }
}
fn host_state(state: &State, config: &Settings) -> anyhow::Result<State> {
    ensure!(valid_id(&config.id), "共享身份配置无效，请重新启动共享");
    State::new(state.dir.join("hosting").join(&config.id))
}
fn prepare(state: &State, value: &Value) -> anyhow::Result<Settings> {
    let mut next: Settings = serde_json::from_value(value.clone())?;
    ensure!(
        !next.name.trim().is_empty()
            && next.name.len() <= 120
            && !next.name.chars().any(char::is_control),
        "请填写有效的共享名称"
    );
    ensure!(next.root.is_absolute(), "请选择共享文件夹");
    next.root = next
        .root
        .canonicalize()
        .context("共享文件夹不存在或不可访问")?;
    ensure!(next.root.is_dir(), "共享位置必须是文件夹");
    ensure!(
        !state.dir.starts_with(&next.root) && !next.root.starts_with(&state.dir),
        "共享文件夹不能包含或位于 TwoDrive 私有配置目录中"
    );
    ensure!(
        matches!(next.network.as_str(), "auto" | "direct" | "system" | "lan"),
        "网络连接方式无效"
    );
    let previous = load(state)?;
    // Existing authorizations must never follow a different directory or rights.
    next.id = if previous.root == next.root
        && previous.writable == next.writable
        && valid_id(&previous.id)
    {
        previous.id
    } else {
        format!(
            "s-{}",
            twodrive_dev::state::random_secret().replace('_', "-")
        )
    };
    Ok(next)
}

#[derive(Default)]
pub struct Hosting {
    child: Option<Process>,
    failure: Option<String>,
    network: Arc<Diagnostics>,
}
impl Hosting {
    pub fn stop(&mut self) {
        self.child.take();
        self.failure = None;
    }

    pub fn status(&mut self, state: &State) -> anyhow::Result<Value> {
        if self
            .child
            .as_mut()
            .is_some_and(|p| p.0.try_wait().ok().flatten().is_some())
        {
            self.child.take();
            self.failure = Some("共享进程已退出，请检查网络后重新启动".into());
        }
        let config = load(state)?;
        let public = json!({"name":config.name,"root":config.root,"writable":config.writable,
            "discoverable":config.discoverable,"network":config.network});
        let mut peers = Vec::new();
        let mut device = None;
        if valid_id(&config.id) {
            let private = host_state(state, &config)?;
            if private.dir.join("share.json").exists() {
                let share: Share = private.read("share.json")?;
                device = Some(share.address.id.to_string());
                peers = private.control(|c| Ok(c.peers.iter().cloned().collect::<Vec<_>>()))?;
            }
        }
        Ok(
            json!({"supported":true,"running":self.child.is_some(),"settings":public,
            "device":device,"peers":peers,"error":self.failure,"network":self.network.snapshot()}),
        )
    }

    pub fn start(
        &mut self,
        state: &State,
        args: &Args,
        value: &Value,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Value> {
        ensure!(self.child.is_none(), "请先停止共享再修改设置");
        let config = prepare(state, value)?;
        let private = host_state(state, &config)?;
        let mut command = Command::new(&args.peer_bin);
        command
            .arg("--state")
            .arg(&private.dir)
            .arg("serve")
            .arg("--root")
            .arg(&config.root)
            .args([
                "--device-name",
                &config.name,
                "--network-route",
                if config.network == "lan" {
                    "system"
                } else {
                    &config.network
                },
            ]);
        if config.writable {
            command.arg("--write");
        }
        if config.discoverable {
            command.arg("--discoverable");
        }
        if config.network == "lan" {
            command.arg("--no-relay");
        }
        let network = Arc::new(Diagnostics::default());
        let mut child = spawn(command)?;
        drain_errors(&mut child, network.clone());
        let stdout = child
            .0
            .stdout
            .take()
            .context("sharing stdout unavailable")?;
        let (send, receive) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if line.starts_with("Serving device=") {
                    let _ = send.send(());
                }
            }
        });
        let start = Instant::now();
        loop {
            ensure!(!cancel.load(Ordering::Relaxed), "共享启动已取消");
            ensure!(
                start.elapsed() < Duration::from_secs(60),
                "共享启动超时，请检查网络设置"
            );
            ensure!(
                child.0.try_wait()?.is_none(),
                "共享启动失败，请检查文件夹和网络设置"
            );
            if receive.recv_timeout(Duration::from_millis(100)).is_ok() {
                break;
            }
        }
        state.save("hosting.json", &config)?;
        self.child = Some(child);
        self.network = network;
        self.failure = None;
        self.status(state)
    }

    pub fn export(&mut self, state: &State, output: &Path, replace: bool) -> anyhow::Result<Value> {
        ensure!(
            self.status(state)?["running"] == true,
            "请先启动共享，再导出连接文件"
        );
        ensure!(
            output.is_absolute() && output.file_name().is_some(),
            "请选择连接文件保存位置"
        );
        let parent = output.parent().context("请选择保存位置")?.canonicalize()?;
        ensure!(
            !parent.starts_with(&state.dir),
            "连接文件不能替换 TwoDrive 私有配置"
        );
        let config = load(state)?;
        let private = host_state(state, &config)?;
        let expires = private.export_connection(output, &config.name, replace)?;
        Ok(json!({"expires":expires,"name":config.name}))
    }

    pub fn revoke(&mut self, state: &State, device: &str) -> anyhow::Result<Value> {
        let _: iroh::PublicKey = device.parse().context("设备身份无效")?;
        let private = host_state(state, &load(state)?)?;
        private.control(|c| {
            c.peers.remove(device);
            Ok(())
        })?;
        self.status(state)
    }
}

/// Serve only the TDLink hosting protocol. The client engine has its own lock
/// and never reads hosting.json or starts a sharing process.
pub(super) fn run(args: &Args, state: &State) -> anyhow::Result<()> {
    use super::{Request, string};
    use std::io::{Read, Write};

    let _lock = State::new(state.dir.join("tdlink-server"))?.run_lock()?;
    let mut hosting = Hosting::default();
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    loop {
        let mut line = String::new();
        if input.by_ref().take(1024 * 1024 + 1).read_line(&mut line)? == 0 {
            break;
        }
        ensure!(line.len() <= 1024 * 1024, "IPC line exceeds limit");
        let request: Request = serde_json::from_str(&line)?;
        let a = &request.args;
        let result = (|| -> anyhow::Result<Value> {
            match request.op.as_str() {
                "status" => Ok(json!({"sharing": hosting.status(state)?})),
                "share_start" => hosting.start(state, args, a, &AtomicBool::new(false)),
                "share_stop" => {
                    hosting.stop();
                    hosting.status(state)
                }
                "share_export" => {
                    hosting.export(state, Path::new(string(a, "output")?), a["replace"] == true)
                }
                "share_revoke" => hosting.revoke(state, string(a, "device")?),
                "quit" => {
                    hosting.stop();
                    Ok(json!({}))
                }
                _ => anyhow::bail!("unsupported TDLink operation"),
            }
        })();
        let reply = match result {
            Ok(result) => json!({"id":request.id,"ok":true,"result":result}),
            Err(error) => json!({"id":request.id,"ok":false,"error":error.to_string()}),
        };
        writeln!(output, "{reply}")?;
        output.flush()?;
        if request.op == "quit" {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changed_scope_gets_new_identity_and_private_paths_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let state = State::new(temp.path().join("private")).unwrap();
        let root = temp.path().join("share");
        std::fs::create_dir(&root).unwrap();
        let mut input = json!({"root":root,"name":"My PC","network":"lan"});
        let first = prepare(&state, &input).unwrap();
        state.save("hosting.json", &first).unwrap();
        input["name"] = json!("Renamed PC");
        assert_eq!(prepare(&state, &input).unwrap().id, first.id);
        input["writable"] = json!(true);
        assert_ne!(prepare(&state, &input).unwrap().id, first.id);
        input["root"] = json!(state.dir);
        assert!(prepare(&state, &input).is_err());
        input["root"] = json!(temp.path());
        assert!(prepare(&state, &input).is_err());
    }
}
