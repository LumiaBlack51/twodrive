//! One short-lived authenticated component per same-directory file rename.
//! Fresh checks share one Fs session; the canonical config remains serialized.
use super::*;
use std::os::unix::fs::PermissionsExt;

struct Session {
    child: OwnedChild,
    client: reqwest::blocking::Client,
    url: String,
    username: String,
    password: String,
}
impl Session {
    fn start(mut command: Command) -> anyhow::Result<Self> {
        let reservation = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = reservation.local_addr()?;
        let username = random_secret();
        let password = random_secret();
        command
            .args([
                "rcd",
                "--ignore-existing",
                "--rc-addr",
                &address.to_string(),
            ])
            .env("RCLONE_RC_USER", &username)
            .env("RCLONE_RC_PASS", &password)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        drop(reservation);
        let mut session = Self {
            child: OwnedChild(command.spawn()?),
            client: reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(35))
                .build()?,
            url: format!("http://{address}/"),
            username,
            password,
        };
        let start = Instant::now();
        loop {
            ensure!(session.child.0.try_wait()?.is_none(), "改名会话启动失败");
            if let Ok(value) = session.call_timeout(
                "core/pid",
                serde_json::json!({}),
                Duration::from_millis(200),
            ) {
                ensure!(
                    value["pid"].as_u64() == Some(session.child.0.id() as u64),
                    "改名会话身份不符"
                );
                break;
            }
            ensure!(start.elapsed() < Duration::from_secs(5), "改名会话启动超时");
            thread::sleep(Duration::from_millis(20));
        }
        Ok(session)
    }
    fn call_timeout(
        &self,
        endpoint: &str,
        args: Value,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        let response = self
            .client
            .post(format!("{}{endpoint}", self.url))
            .timeout(timeout)
            .basic_auth(&self.username, Some(&self.password))
            .json(&args)
            .send()
            .map_err(|_| anyhow::anyhow!("改名访问组件请求未完成；请刷新确认结果"))?;
        ensure!(
            response.status().is_success(),
            "改名访问组件未确认操作；请刷新确认结果"
        );
        let mut bytes = Vec::new();
        response
            .take(JSON_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= JSON_LIMIT, "改名会话响应超过预算");
        serde_json::from_slice(&bytes).context("改名会话返回格式无效")
    }
    fn call(&self, endpoint: &str, args: Value) -> anyhow::Result<Value> {
        self.call_timeout(endpoint, args, Duration::from_secs(35))
    }
}
impl RcloneDrive {
    pub(super) fn rename_in_session(
        &self,
        expected: &Snapshot,
        to: &str,
        parent: &str,
    ) -> anyhow::Result<MetadataEntry> {
        self.check_read_identity()?;
        let started = Instant::now();
        let mut timing = serde_json::json!({"started_unix_ms":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis()});
        // Serialize with canonical downloads/mutations. Independent display
        // read views retain their nonblocking isolation and compare-save guard.
        let _operation = wait_for_operation(&self.state, Duration::from_secs(45), || {
            self.check_read_identity()
        })?;
        timing["account_wait_ms"] = serde_json::json!(started.elapsed().as_millis());
        let before = super::read_view::encrypted_bytes(&self.state)?;
        let temporary = tempfile::Builder::new()
            .prefix("rename-session-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(&self.state.dir)?;
        let view = State::new(temporary.path().to_owned())?;
        view.write_bytes(CONFIG, &before)?;
        let step = Instant::now();
        let session = Session::start(command(&self.exe, &view, &self.pass, &self.route))?;
        timing["component_start_ms"] = serde_json::json!(step.elapsed().as_millis());
        let opts = serde_json::json!({"noMimeType":true});
        let result = (|| {
            let step = Instant::now();
            let value=session.call("operations/list",serde_json::json!({"fs":"twodrive:","remote":parent.trim_start_matches('/'),"opt":opts}));
            timing["preflight_ms"] = serde_json::json!(step.elapsed().as_millis());
            let value = value?;
            let entries = self.entries(parent, &value["list"])?;
            let id = expected.encode()?;
            ensure!(
                entries
                    .iter()
                    .any(|m| m.path == expected.path && m.remote_id == id),
                "源文件不存在或版本已改变"
            );
            ensure!(entries.iter().all(|m| m.path != to), "目标已存在，拒绝覆盖");
            self.check_read_identity()?;
            let step = Instant::now();
            let moved=session.call("operations/movefile",serde_json::json!({"srcFs":"twodrive:","dstFs":"twodrive:","srcRemote":expected.path.trim_start_matches('/'),"dstRemote":to.trim_start_matches('/')}));
            timing["move_ms"] = serde_json::json!(step.elapsed().as_millis());
            moved?;
            self.check_read_identity()?;
            let step = Instant::now();
            let value=session.call("operations/list",serde_json::json!({"fs":"twodrive:","remote":parent.trim_start_matches('/'),"opt":opts}));
            timing["confirmation_ms"] = serde_json::json!(step.elapsed().as_millis());
            let value = value?;
            let entries = self.entries(parent, &value["list"])?;
            ensure!(
                entries.iter().all(|m| m.path != expected.path),
                "移动尚未确认，源名称仍存在"
            );
            let next = entries
                .into_iter()
                .find(|m| m.path == to)
                .context("移动后未找到目标，请刷新确认")?;
            let actual = Snapshot::decode(&next.remote_id, &self.binding)?;
            ensure!(
                !next.is_dir && next.size == expected.size && actual.id == expected.id,
                "改名结果身份未确认，请刷新检查"
            );
            Ok(next)
        })();
        // Reap the child before publishing refreshed config, so no late save
        // can race with the conditional replacement or private-dir cleanup.
        drop(session);
        self.check_read_identity()?;
        let publication = (|| -> anyhow::Result<()> {
            if super::read_view::encrypted_bytes(&view)? != before {
                let identity =
                    check_config(&self.exe, &view, &self.pass, &self.route, &self.cancel)?;
                ensure!(
                    self.binding.starts_with(&format!("icloud:{identity}:")),
                    "改名会话账号身份已改变"
                );
                let after = super::read_view::encrypted_bytes(&view)?;
                self.check_read_identity()?;
                if super::read_view::encrypted_bytes(&self.state)? == before {
                    self.state.write_bytes(CONFIG, &after)?;
                }
            }
            Ok(())
        })();
        timing["total_ms"] = serde_json::json!(started.elapsed().as_millis());
        timing["remote_confirmed"] = serde_json::json!(result.is_ok());
        timing["config_refresh_ok"] = serde_json::json!(publication.is_ok());
        // Best-effort private timings contain no paths, object IDs or errors.
        // A diagnostic write failure must never turn a confirmed rename into
        // a second cloud mutation or a misleading retry.
        let _ = self.state.save("last-rename-timing.json", &timing);
        publication?;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires pinned official Rclone; disposable local filesystem only"]
    fn official_authenticated_session_lists_moves_and_stats_without_cloud_access() {
        let exe = PathBuf::from(std::env::var_os("TWODRIVE_TEST_RCLONE").unwrap());
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path().join("state")).unwrap();
        fs::write(root.path().join("before.txt"), b"synthetic bytes").unwrap();
        let mut cmd = command(&exe, &state, "synthetic", "direct");
        cmd.arg("--config=/dev/null");
        let session = Session::start(cmd).unwrap();
        assert!(
            session
                .client
                .post(format!("{}core/pid", session.url))
                .json(&serde_json::json!({}))
                .send()
                .unwrap()
                .status()
                .is_client_error()
        );
        let fs = root.path().to_string_lossy();
        let list = session
            .call(
                "operations/list",
                serde_json::json!({"fs":fs,"remote":"","opt":{"noMimeType":true}}),
            )
            .unwrap();
        assert!(
            list["list"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["Name"] == "before.txt")
        );
        session.call("operations/movefile",serde_json::json!({"srcFs":fs,"dstFs":fs,"srcRemote":"before.txt","dstRemote":"after.txt"})).unwrap();
        let next = session
            .call(
                "operations/stat",
                serde_json::json!({"fs":fs,"remote":"after.txt","opt":{"noMimeType":true}}),
            )
            .unwrap();
        assert_eq!(next["item"]["Size"], 15);
        fs::write(
            root.path().join("occupied.txt"),
            b"preserve existing target",
        )
        .unwrap();
        session.call("operations/movefile",serde_json::json!({"srcFs":fs,"dstFs":fs,"srcRemote":"after.txt","dstRemote":"occupied.txt"})).unwrap();
        assert_eq!(
            fs::read(root.path().join("occupied.txt")).unwrap(),
            b"preserve existing target",
            "RC must inherit create-only/no-overwrite configuration"
        );
        assert!(
            root.path().join("after.txt").exists(),
            "an existing destination must leave the source in place"
        );
        let pid = session.child.0.id();
        drop(session);
        assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
        assert!(!root.path().join("before.txt").exists());
        assert_eq!(
            fs::read(root.path().join("after.txt")).unwrap(),
            b"synthetic bytes"
        );
    }
}
