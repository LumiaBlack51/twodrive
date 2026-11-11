//! GUI challenge bridge. Raw Rclone state and credentials stay private.
use super::*;
use serde_json::json;

const ATTEMPT: &str = "gui-auth.secret";

#[derive(Serialize, Deserialize)]
struct Attempt {
    id: String,
    work: String,
    stage: String,
    field: String,
    created: u64,
    steps: u32,
    #[serde(default = "system_route")]
    route: String,
}

fn system_route() -> String {
    "system".into()
}

fn new_auth_work_state(state: &State) -> anyhow::Result<State> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("gui-auth-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Tempfile directories otherwise use the platform default (0777 before
        // umask). Set this at creation; do not relax State's private-dir check.
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let temporary = builder.tempdir_in(&state.dir)?;
    let work = State::new(temporary.path().to_path_buf())?;
    let _ = temporary.keep();
    Ok(work)
}

// RC errors also contain the entire request under `input`. Never forward the
// body or arbitrary error text: only classify the error into fixed messages.
fn rpc_error(status: u16, body: &[u8]) -> String {
    let response: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let error = response["error"]
        .as_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (code, message) = if status == 401 {
        ("AUTH_LOCAL", "本机登录组件认证失败，请重启客户端后重试")
    } else if error.contains("base64")
        || error.contains("input too short")
        || error.contains("revealing password")
    {
        (
            "AUTH_FORMAT",
            "客户端密码格式转换失败，请更新客户端；无需修改 Apple 密码",
        )
    } else if [
        "timeout",
        "timed out",
        "no such host",
        "connection refused",
        "proxyconnect",
        "tls handshake",
        "certificate",
    ]
    .iter()
    .any(|p| error.contains(p))
    {
        (
            "AUTH_NETWORK",
            "无法完成 Apple 网络连接，请检查所选网络路由后重试",
        )
    } else if error.contains("missing pcs cookies") || error.contains("requestpcs:") {
        (
            "AUTH_ADP",
            "Apple 网页数据授权未完成，请检查可信设备上的批准提示后重试",
        )
    } else if error.contains("locked")
        || error.contains("disabled")
        || error.contains("too many requests")
    {
        (
            "AUTH_SECURITY",
            "Apple 返回账号安全限制，请先在 Apple 官方账号页面检查",
        )
    } else if error.contains("invalid username/password")
        || error.contains("incorrect password")
        || error.contains("invalid credentials")
    {
        (
            "AUTH_DENIED",
            "Apple 未接受账号凭据，请核对普通账号密码后重试",
        )
    } else {
        ("AUTH_REMOTE", "登录组件未能完成认证；原账号配置已保留")
    };
    format!("{message}（{code}，组件 HTTP {status}）")
}

pub(super) fn pending(state: &State) -> bool {
    state.dir.join(ATTEMPT).exists()
}

fn read_attempt(state: &State) -> anyhow::Result<Attempt> {
    let bytes = twodrive_core::private_file::read_secret(&state.dir.join(ATTEMPT))?;
    serde_json::from_slice(&bytes).context("登录状态不可用，请重新登录")
}

fn work_state(state: &State, attempt: &Attempt) -> anyhow::Result<State> {
    ensure!(
        attempt.work.starts_with("gui-auth-")
            && !attempt.work.contains(['/', '\\', ':'])
            && attempt.work.len() < 100,
        "无效的私有登录目录"
    );
    let path = state.dir.join(&attempt.work);
    ensure!(
        !fs::symlink_metadata(&path)?.is_symlink(),
        "拒绝链接形式的登录目录"
    );
    let work = State::new(path)?;
    ensure!(
        work.dir.parent() == Some(state.dir.as_path()),
        "登录目录越界"
    );
    Ok(work)
}

pub(super) fn clear(state: &State) -> anyhow::Result<()> {
    if pending(state) {
        let attempt = read_attempt(state)?;
        let work = work_state(state, &attempt)?;
        // Only the validated, randomly named direct child created for this attempt.
        fs::remove_dir_all(&work.dir)?;
        fs::remove_file(state.dir.join(ATTEMPT))?;
    }
    Ok(())
}

/// Start a short-lived, authenticated control server on an OS-assigned loopback
/// port. JSON credentials travel in the HTTP body, never argv or a plaintext file.
fn config_rpc(
    exe: &Path,
    state: &State,
    pass: &str,
    route: &str,
    method: &str,
    payload: Value,
    cancel: &AtomicBool,
) -> anyhow::Result<Value> {
    ensure!(
        matches!(method, "config/create" | "config/update" | "core/obscure"),
        "invalid auth method"
    );
    let control_secret = random_secret();
    let mut cmd = command(exe, state, pass, route);
    cmd.args([
        "rcd",
        "--rc-addr=127.0.0.1:0",
        "--log-level=NOTICE",
        "--rc-serve=false",
        "--rc-web-gui=false",
    ])
    .env("RCLONE_RC_USER", "twodrive")
    .env("RCLONE_RC_PASS", &control_secret);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    let mut child = OwnedChild(
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let mut stderr = child
        .0
        .stderr
        .take()
        .context("missing control startup pipe")?;
    let (port_send, port_receive) = mpsc::sync_channel(1);
    let log_reader = thread::spawn(move || {
        let mut bytes = [0u8; 512];
        let mut startup = String::new();
        let mut announced = false;
        while let Ok(n) = stderr.read(&mut bytes) {
            if n == 0 {
                break;
            }
            if announced {
                continue;
            }
            startup.push_str(&String::from_utf8_lossy(&bytes[..n]));
            let marker = "Serving remote control on http://127.0.0.1:";
            if let Some(start) = startup.find(marker) {
                let rest = &startup[start + marker.len()..];
                if let Some(end) = rest.find('/')
                    && let Ok(port) = rest[..end].parse::<u16>()
                    && port != 0
                {
                    let _ = port_send.try_send(port);
                    announced = true;
                    startup.clear();
                }
            }
            if startup.len() > 4096 {
                break;
            }
        }
    });
    let result = (|| -> anyhow::Result<Value> {
        let start = Instant::now();
        let port = loop {
            ensure!(!cancel.load(Ordering::Relaxed), "登录已取消");
            ensure!(
                start.elapsed() < Duration::from_secs(8),
                "无法启动本机登录组件，请重试"
            );
            ensure!(child.0.try_wait()?.is_none(), "本机登录组件提前退出");
            match port_receive.recv_timeout(Duration::from_millis(50)) {
                Ok(port) => break port,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => bail!("无法读取登录组件启动状态"),
            }
        };
        let address = format!("http://127.0.0.1:{port}/{method}");
        let (send, receive) = mpsc::sync_channel(1);
        let request = thread::spawn(move || {
            let response = (|| -> anyhow::Result<Value> {
                let client = reqwest::blocking::Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .connect_timeout(Duration::from_secs(2))
                    .timeout(Duration::from_secs(80))
                    .build()?;
                let response = client
                    .post(address)
                    .basic_auth("twodrive", Some(control_secret))
                    .json(&payload)
                    .send()
                    .context("本机登录通信失败，请重试")?;
                let status = response.status();
                let mut bytes = Vec::new();
                response.take(65537).read_to_end(&mut bytes)?;
                ensure!(bytes.len() <= 65536, "登录响应超过限制");
                ensure!(
                    status.is_success(),
                    "{}",
                    rpc_error(status.as_u16(), &bytes)
                );
                serde_json::from_slice(&bytes).context("登录响应无效，请重试")
            })();
            let _ = send.send(response);
        });
        let response = loop {
            if cancel.load(Ordering::Relaxed) || start.elapsed() >= Duration::from_secs(90) {
                let _ = child.0.kill();
                break Err(anyhow::anyhow!("登录已取消或超时，请重试"));
            }
            match receive.recv_timeout(Duration::from_millis(50)) {
                Ok(response) => break response,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => break Err(anyhow::anyhow!("登录通信已中断，请重试")),
            }
        };
        let _ = child.0.kill();
        let _ = request.join();
        response
    })();
    let _ = child.0.kill();
    let _ = child.0.wait();
    let _ = log_reader.join();
    result
}

fn public_question(response: &Value) -> anyhow::Result<Value> {
    let field = response["Option"]["Name"]
        .as_str()
        .context("登录步骤缺少输入项")?;
    let (label, help, secret, alternatives) = match field {
        "apple_id" => (
            "Apple 账号",
            "请输入你的 Apple 账号（邮箱或电话号码）。",
            false,
            json!([]),
        ),
        "password" => (
            "账号密码",
            "请输入该 Apple 账号的密码。密码会遮挡显示。",
            true,
            json!([]),
        ),
        "config_2fa" => (
            "双重认证验证码",
            "请输入可信设备上收到的验证码。",
            true,
            json!([{"label":"改用短信验证码","value":"sms"}]),
        ),
        _ => bail!("此账号需要额外的认证步骤，当前图形登录尚未支持；原登录配置已保留"),
    };
    Ok(
        json!({"title":"登录 iCloud Drive", "label":label,"help":help,"secret":secret,
        "required":true,"alternatives":alternatives,
        "message":if response["Error"].as_str().is_some_and(|e| !e.is_empty()) {"验证未完成，请检查输入后重试。"} else {""}}),
    )
}

fn config_reply(
    exe: &Path,
    state: &State,
    pass: &str,
    route: &str,
    reply: (&str, &str, &str),
    cancel: &AtomicBool,
) -> anyhow::Result<Value> {
    let (stage, field, answer) = reply;
    // `opt.result` bypasses UpdateRemote's password handling. The *all-set
    // state stores it verbatim; it must already have Rclone's obscure format.
    // Do this even if the user's password happens to look like encoded data.
    let converted;
    let answer = if field == "password" {
        converted = config_rpc(
            exe,
            state,
            pass,
            route,
            "core/obscure",
            json!({"clear":answer}),
            cancel,
        )?;
        converted["obscured"]
            .as_str()
            .filter(|value| !value.is_empty())
            .context("本机密码格式转换失败")?
    } else {
        answer
    };
    config_rpc(
        exe,
        state,
        pass,
        route,
        "config/update",
        json!({"name":"twodrive","parameters":{},"opt":{"all":true,"nonInteractive":true,
            "continue":true,"state":stage,"result":answer}}),
        cancel,
    )
}

fn inline_answer<'a>(field: &str, action: &str, args: &'a Value) -> Option<&'a str> {
    if action != "configure" || !matches!(field, "apple_id" | "password") {
        return None;
    }
    args[field].as_str().filter(|value| !value.is_empty())
}

pub fn action(
    state: State,
    action: &str,
    args: &Value,
    cancel: &AtomicBool,
) -> anyhow::Result<Value> {
    action_with_binary(state, action, args, cancel, binary()?)
}

fn action_with_binary(
    state: State,
    action: &str,
    args: &Value,
    cancel: &AtomicBool,
    exe: PathBuf,
) -> anyhow::Result<Value> {
    let lock = private_lock(&state, "auth.lock")?;
    lock.try_lock_exclusive()
        .context("请先断开此账号，再登录或更新授权")?;
    let result = run_action(state.clone(), action, args, cancel, exe);
    if result.is_err() && action == "configure" {
        let _ = clear(&state);
    }
    result
}

fn run_action(
    state: State,
    action: &str,
    args: &Value,
    cancel: &AtomicBool,
    exe: PathBuf,
) -> anyhow::Result<Value> {
    ensure!(!cancel.load(Ordering::Relaxed), "登录已取消");
    let mut attempt;
    let mut response;
    if action == "configure" {
        let account = args["apple_id"].as_str().unwrap_or_default();
        let secret = args["password"].as_str().unwrap_or_default();
        let route = args["network_route"].as_str().unwrap_or("system");
        ensure!(
            matches!(route, "system" | "direct"),
            "请选择系统路由或仅直连"
        );
        ensure!(
            account.len() <= 320 && secret.len() <= 4096 && account.is_empty() == secret.is_empty(),
            "请填写 Apple 账号和密码后再登录"
        );
        if state.dir.join(MANIFEST).exists() {
            let auth: Auth = state.read(MANIFEST)?;
            ensure!(auth.kind == "icloud", "拒绝修改其他提供者的状态");
        }
        clear(&state)?;
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
        let work = new_auth_work_state(&state)?;
        if state.dir.join(CONFIG).exists() {
            work.write_bytes(CONFIG, &fs::read(state.dir.join(CONFIG))?)?;
        }
        encrypted_config(&exe, &work, &pass)?;
        let config = json_command(&exe, &work, &pass, "system", &["config", "dump"], cancel)?;
        let exists = config["twodrive"].is_object();
        ensure!(
            !exists || config["twodrive"]["type"] == "iclouddrive",
            "拒绝更改其他提供者的配置"
        );
        attempt = Attempt {
            id: random_secret(),
            work: work.dir.file_name().unwrap().to_string_lossy().into(),
            stage: String::new(),
            field: String::new(),
            created: crate::state::now(),
            steps: 0,
            route: route.into(),
        };
        twodrive_core::private_file::write_secret(
            &state.dir.join(ATTEMPT),
            &serde_json::to_vec(&attempt)?,
        )?;
        response = config_rpc(
            &exe,
            &work,
            &pass,
            &attempt.route,
            if exists {
                "config/update"
            } else {
                "config/create"
            },
            json!({"name":"twodrive","type":"iclouddrive","parameters":{},"opt":{"all":true,"nonInteractive":true}}),
            cancel,
        )?;
    } else {
        attempt = read_attempt(&state).context("登录已结束，请重新开始")?;
        ensure!(
            args["attempt"].as_str() == Some(attempt.id.as_str()),
            "登录步骤已过期，请重新开始"
        );
        if action == "auth_cancel" {
            clear(&state)?;
            return Ok(
                json!({"message":"已取消登录，原账号配置保留。","options":{"icloud_state":state.dir,"writable":false}}),
            );
        }
        ensure!(action == "auth_reply", "unsupported auth action");
        ensure!(
            crate::state::now().saturating_sub(attempt.created) < 900 && attempt.steps < 32,
            "登录步骤已过期，请重新开始"
        );
        let answer = args["answer"].as_str().context("请填写登录信息")?;
        ensure!(
            !answer.is_empty() && answer.len() <= 4096,
            "请输入有效的登录信息"
        );
        let work = work_state(&state, &attempt)?;
        response = config_reply(
            &exe,
            &work,
            &password(&state)?,
            &attempt.route,
            (&attempt.stage, &attempt.field, answer),
            cancel,
        )?;
    }
    let work = work_state(&state, &attempt)?;
    let pass = password(&state)?;
    for _ in 0..8 {
        let stage = response["State"].as_str().unwrap_or_default();
        if stage.is_empty() {
            ensure!(
                response["error"].is_null()
                    && response["Error"].as_str().unwrap_or_default().is_empty(),
                "认证未完成，请重新登录"
            );
            let identity = check_config(&exe, &work, &pass, &attempt.route, cancel)?;
            json_command(
                &exe,
                &work,
                &pass,
                &attempt.route,
                &["lsjson", "twodrive:", "--max-depth=1", "--no-mimetype"],
                cancel,
            )?;
            state.write_bytes(CONFIG, &fs::read(work.dir.join(CONFIG))?)?;
            state.save(
                MANIFEST,
                &Auth {
                    kind: "icloud".into(),
                    binding: format!("icloud:{identity}:{}", random_secret()),
                    ready: true,
                },
            )?;
            clear(&state)?;
            return Ok(
                json!({"authenticated":true,"message":"iCloud 登录成功。保存连接后即可浏览文件。",
                "options":{"icloud_state":state.dir,"writable":false}}),
            );
        }
        let field = response["Option"]["Name"]
            .as_str()
            .context("未知的登录步骤")?;
        let automatic = match field {
            "service" => Some("drive"),
            "config_fs_advanced" => Some("false"),
            "description" => Some(""),
            _ => inline_answer(field, action, args),
        };
        if let Some(answer) = automatic {
            response = config_reply(
                &exe,
                &work,
                &pass,
                &attempt.route,
                (stage, field, answer),
                cancel,
            )?;
            continue;
        }
        let mut question = public_question(&response)?;
        attempt.id = random_secret();
        attempt.stage = stage.into();
        attempt.field = field.into();
        attempt.steps += 1;
        twodrive_core::private_file::write_secret(
            &state.dir.join(ATTEMPT),
            &serde_json::to_vec(&attempt)?,
        )?;
        question["attempt"] = json!(attempt.id);
        question["reply_action"] = json!("auth_reply");
        question["cancel_action"] = json!("auth_cancel");
        question["context"] = json!({"icloud_state":state.dir});
        return Ok(json!({"auth":question,"options":{"icloud_state":state.dir,"writable":false}}));
    }
    bail!("登录步骤超出预算，请重试")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(unix)]
    fn gui_auth_work_directory_is_private_and_can_be_reopened() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let account = State::new(root.path().join("account")).unwrap();
        let work = new_auth_work_state(&account).unwrap();
        assert_eq!(work.dir.parent(), Some(account.dir.as_path()));
        assert_eq!(
            fs::metadata(&work.dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        State::new(work.dir).unwrap();
    }

    #[test]
    #[ignore = "official Rclone required; stops before post-config and never contacts Apple"]
    fn official_password_reply_uses_rclone_format_before_authentication() {
        let exe = PathBuf::from(std::env::var_os("TWODRIVE_TEST_RCLONE").unwrap());
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path().join("account")).unwrap();
        let pass = random_secret();
        let cancel = AtomicBool::new(false);
        encrypted_config(&exe, &state, &pass).unwrap();
        let mut response = config_rpc(&exe, &state, &pass, "direct", "config/create",
            json!({"name":"twodrive","type":"iclouddrive","parameters":{},"opt":{"all":true,"nonInteractive":true}}),
            &cancel).unwrap();
        let synthetic = " space-sensitive-test! ";
        for (field, answer) in [
            ("service", "drive"),
            ("apple_id", "synthetic@example.invalid"),
            ("password", synthetic),
        ] {
            assert_eq!(response["Option"]["Name"], field);
            response = config_reply(
                &exe,
                &state,
                &pass,
                "direct",
                (response["State"].as_str().unwrap(), field, answer),
                &cancel,
            )
            .unwrap();
        }
        // Do NOT answer this question: post-config would initiate Apple sign-in.
        assert_eq!(response["Option"]["Name"], "config_fs_advanced");
        let config =
            json_command(&exe, &state, &pass, "system", &["config", "dump"], &cancel).unwrap();
        let stored = config["twodrive"]["password"].as_str().unwrap();
        assert!(
            stored != synthetic,
            "password continuation must not store clear input"
        );
        let decoded = URL_SAFE_NO_PAD
            .decode(stored)
            .expect("Rclone password encoding");
        assert_eq!(decoded.len(), 16 + synthetic.len());
        assert!(
            fs::read(state.dir.join(CONFIG))
                .unwrap()
                .starts_with(HEADER)
        );
    }

    #[test]
    fn rpc_errors_are_classified_without_forwarding_request_or_error_body() {
        for (error, expected) in [
            (
                "base64 decode failed when revealing password",
                "AUTH_FORMAT",
            ),
            ("proxyconnect tcp: connection refused", "AUTH_NETWORK"),
            ("Missing PCS cookies from the request", "AUTH_ADP"),
            ("account locked", "AUTH_SECURITY"),
            ("invalid username/password", "AUTH_DENIED"),
            (
                "unrecognized response with synthetic-sensitive-text",
                "AUTH_REMOTE",
            ),
        ] {
            let body = json!({"error":error,"input":{"password":"synthetic-sensitive-text","opt":{"result":"private-answer"}}});
            let message = rpc_error(500, &serde_json::to_vec(&body).unwrap());
            assert!(message.contains(expected));
            assert!(
                !message.contains("synthetic-sensitive-text")
                    && !message.contains("private-answer")
            );
        }
        assert!(rpc_error(401, b"not-json").contains("AUTH_LOCAL"));
        assert!(rpc_error(500, b"not-json").contains("AUTH_REMOTE"));
    }
    #[test]
    fn public_challenge_never_exposes_raw_state_or_password_defaults() {
        let raw = json!({"State":"private-state-token","Option":{"Name":"password","Default":"synthetic-password"}});
        let question = public_question(&raw).unwrap();
        assert_eq!(question["secret"], true);
        let text = question.to_string();
        assert!(!text.contains("private-state-token") && !text.contains("synthetic-password"));
        assert!(public_question(&json!({"Option":{"Name":"unknown"}})).is_err());
    }

    #[test]
    fn inline_answers_are_explicit_and_only_used_for_initial_credentials() {
        let args = json!({"apple_id":"user@example.invalid","password":" space-sensitive ","unknown":"no"});
        assert_eq!(
            inline_answer("password", "configure", &args),
            Some(" space-sensitive ")
        );
        assert_eq!(
            inline_answer("apple_id", "configure", &args),
            Some("user@example.invalid")
        );
        assert_eq!(inline_answer("password", "auth_reply", &args), None);
        assert_eq!(inline_answer("unknown", "configure", &args), None);
    }

    #[test]
    #[ignore = "official Rclone required; asks metadata questions only, never contacts Apple"]
    fn official_gui_questions_cancel_and_preserve_previous_login() {
        let exe = std::env::var_os("TWODRIVE_TEST_RCLONE").unwrap();
        // binary() also has a development override; test the internal RPC with
        // the explicit official component, independent of that global override.
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path().join("account")).unwrap();
        let pass = random_secret();
        encrypted_config(Path::new(&exe), &state, &pass).unwrap();
        let response = config_rpc(Path::new(&exe), &state, &pass, "system", "config/create",
            json!({"name":"twodrive","type":"iclouddrive","parameters":{},"opt":{"all":true,"nonInteractive":true}}),
            &AtomicBool::new(false)).unwrap();
        assert_eq!(response["Option"]["Name"], "service");
        let response = config_rpc(Path::new(&exe), &state, &pass, "system", "config/update",
            json!({"name":"twodrive","parameters":{},"opt":{"all":true,"nonInteractive":true,"continue":true,
                "state":response["State"],"result":"drive"}}), &AtomicBool::new(false)).unwrap();
        assert_eq!(response["Option"]["Name"], "apple_id");
        twodrive_core::private_file::write_secret(&state.dir.join(KEY), pass.as_bytes()).unwrap();
        state
            .save(
                MANIFEST,
                &Auth {
                    kind: "icloud".into(),
                    binding: "previous-generation".into(),
                    ready: true,
                },
            )
            .unwrap();
        let previous_config = fs::read(state.dir.join(CONFIG)).unwrap();
        let previous_auth = fs::read(state.dir.join(MANIFEST)).unwrap();
        let cancel = AtomicBool::new(false);
        let first = action_with_binary(
            state.clone(),
            "configure",
            &json!({}),
            &cancel,
            PathBuf::from(&exe),
        )
        .unwrap();
        assert_eq!(first["auth"]["label"], "Apple 账号");
        assert!(pending(&state));
        let second = action_with_binary(
            state.clone(),
            "auth_reply",
            &json!({"attempt":first["auth"]["attempt"],
            "answer":"synthetic@example.invalid"}),
            &cancel,
            PathBuf::from(&exe),
        )
        .unwrap();
        assert_eq!(second["auth"]["label"], "账号密码");
        assert_eq!(second["auth"]["secret"], true);
        assert_ne!(first["auth"]["attempt"], second["auth"]["attempt"]);
        assert!(
            action_with_binary(
                state.clone(),
                "auth_reply",
                &json!({"attempt":first["auth"]["attempt"],
            "answer":"stale"}),
                &cancel,
                PathBuf::from(&exe)
            )
            .is_err()
        );
        action_with_binary(
            state.clone(),
            "auth_cancel",
            &json!({"attempt":second["auth"]["attempt"]}),
            &cancel,
            PathBuf::from(&exe),
        )
        .unwrap();
        assert!(!pending(&state));
        assert_eq!(fs::read(state.dir.join(CONFIG)).unwrap(), previous_config);
        assert_eq!(fs::read(state.dir.join(MANIFEST)).unwrap(), previous_auth);
        assert!(!state.dir.read_dir().unwrap().any(|p| {
            p.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("gui-auth-")
        }));
    }
}
