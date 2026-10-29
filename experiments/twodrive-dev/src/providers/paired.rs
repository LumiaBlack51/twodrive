//! The paired transport currently exposes a loopback DAV gateway. This detail
//! stays in the adapter; services use the same provider contract as any backend.
use super::{Args, Connection, Process};
use anyhow::{Context, ensure};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
    path::Path,
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
    profile::Profile,
    provider::Backend,
    state::State,
    webdav::{UploadMode, WebDavBackend},
};
pub fn descriptor() -> Value {
    json!({"id":"peer","label":"PC / NAS（设备共享）","account_label":"远程设备","icon":"devices",
    "identity_fields":["peer_state"],"protected_paths":["peer_state"],
    "help":"扫描局域网中的 TwoDrive 设备，或直接导入可信的配对邀请。跨网络加密连接。修改已有文件时自动仅传输变化的内容，必要时使用整文件上传。普通 NAS 也可以选择 WebDAV。",
    "actions":[{"id":"discover","label":"发现局域网设备"},{"id":"import","label":"导入连接文件","input":"file"}],
    "capabilities":{"mount":true,"upload":true,"conditional_upload":true,"mkdir":true,"rename":true,"delete":true,"backup":true},
    "fields":[{"key":"invitation_file","label":"配对邀请 JSON（首次连接需要）","type":"file","transient":true},
      {"key":"expected_device","label":"已选择的设备身份","advanced":true,"read_only":true},
      {"key":"peer_state","label":"本机配对目录（留空自动创建）","advanced":true},
      {"key":"full_upload","label":"始终整文件上传","type":"boolean","advanced":true}]})
}
pub fn prepare(p: &mut Profile, state: &State) -> anyhow::Result<()> {
    if p.option("peer_state").is_empty() {
        p.options.insert(
            "peer_state".into(),
            json!(state.dir.join("accounts").join(&p.id).join("peer")),
        );
    }
    validate(p)?;
    let imported = p.option("invitation_file").to_string();
    if !imported.is_empty() {
        use std::io::Read;
        ensure!(
            Path::new(&imported).is_absolute(),
            "请选择配对邀请文件的绝对路径"
        );
        let mut bytes = Vec::new();
        std::fs::File::open(&imported)?
            .take(65537)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 65536, "配对邀请文件过大");
        let invitation: twodrive_dev::state::Invitation =
            serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("配对邀请格式无效"))?;
        ensure!(
            invitation.version == twodrive_dev::state::VERSION
                && invitation.expires > twodrive_dev::state::now(),
            "配对邀请已过期或版本不支持，请向设备重新获取"
        );
        ensure!(
            p.option("expected_device").is_empty()
                || p.option("expected_device") == invitation.address.id.to_string(),
            "邀请不属于选中的设备，未配对"
        );
        let private = State::new(p.option("peer_state").into())?;
        private.save("pair-invitation.json", &invitation)?;
    }
    p.options.remove("invitation_file");
    Ok(())
}
pub fn validate(p: &Profile) -> anyhow::Result<()> {
    ensure!(
        Path::new(p.option("peer_state")).is_absolute(),
        "配对状态必须是绝对路径"
    );
    if !p.option("expected_device").is_empty() {
        let _: iroh::PublicKey = p
            .option("expected_device")
            .parse()
            .map_err(|_| anyhow::anyhow!("设备身份无效"))?;
    }
    Ok(())
}
pub fn action(action: &str, args: &Value, cancel: &AtomicBool) -> anyhow::Result<Value> {
    if action == "import" {
        ensure!(!cancel.load(Ordering::Relaxed), "导入已取消");
        let path = Path::new(args["file"].as_str().context("请选择连接文件")?);
        ensure!(path.is_absolute(), "请选择连接文件的绝对路径");
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(65537)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 65536, "连接文件过大");
        let value: Value = serde_json::from_slice(&bytes).context("连接文件格式无效")?;
        let invitation: twodrive_dev::state::Invitation =
            serde_json::from_value(value.clone()).context("连接文件不包含有效的配对邀请")?;
        ensure!(
            invitation.version == twodrive_dev::state::VERSION
                && invitation.expires > twodrive_dev::state::now(),
            "连接文件已过期或版本不支持，请向共享端重新导出"
        );
        let expected = args["expected_device"].as_str().unwrap_or_default();
        ensure!(
            expected.is_empty() || expected == invitation.address.id.to_string(),
            "连接文件不属于所选设备"
        );
        let mut name = "TwoDrive 设备共享";
        let mut writable = false;
        if let Some(hint) = value.get("connection") {
            ensure!(hint["schema"] == 1, "连接文件版本不支持");
            name = hint["name"].as_str().context("连接文件缺少共享名称")?;
            ensure!(
                !name.trim().is_empty() && name.len() <= 120 && !name.chars().any(char::is_control),
                "连接文件名称无效"
            );
            writable = hint["writable"].as_bool().context("连接文件权限格式无效")?;
        }
        return Ok(json!({"message":"连接信息已填好，保存并连接即可使用。",
            "expires":invitation.expires,"options":{"name":name,"writable":writable,
                "expected_device":invitation.address.id.to_string(),"invitation_file":path}}));
    }
    ensure!(action == "discover", "unsupported device action");
    let devices = twodrive_dev::discovery::scan(cancel)?;
    Ok(
        json!({"message":if devices.is_empty(){"未发现设备。确认对方已开启可发现共享；跨网络可直接导入邀请。"}else{"发现不等于已授权；选择设备后仍需导入它的配对邀请。"},"selection_message":"已选择设备，导入它的配对邀请后保存连接。","items":devices.into_iter().map(|d|json!({"label":d.name,"detail":d.fingerprint,"options":{"expected_device":d.id,"writable":d.writable}})).collect::<Vec<_>>() }),
    )
}
fn backend(p: &Profile, url: &str) -> anyhow::Result<Backend> {
    let credentials = State::new(p.option("peer_state").into())?.read("credentials.json")?;
    Ok(Backend::new(
        WebDavBackend::routed(url, Some(credentials), p.writable, "direct")?.with_upload_mode(
            if p.options.get("full_upload").is_some_and(|v| v == true) {
                UploadMode::Full
            } else {
                UploadMode::Auto
            },
        ),
    ))
}
pub fn restore(p: &Profile, recipe: &Value) -> anyhow::Result<Backend> {
    backend(
        p,
        recipe["gateway"]
            .as_str()
            .context("missing paired gateway")?,
    )
}
pub fn connect(
    p: &Profile,
    args: &Args,
    cancel: &AtomicBool,
    network: Arc<twodrive_dev::network::Diagnostics>,
) -> anyhow::Result<Connection> {
    let peer = State::new(p.option("peer_state").into())?;
    let ticket = peer.dir.join("pair-invitation.json");
    let remote: iroh::EndpointAddr = if ticket.exists() {
        peer.read::<twodrive_dev::state::Invitation>("pair-invitation.json")?
            .address
    } else {
        peer.read("remote.json").context("请先导入设备的配对邀请")?
    };
    ensure!(
        p.option("expected_device").is_empty()
            || p.option("expected_device") == remote.id.to_string(),
        "配对目录属于另一设备，未连接"
    );
    let mut cmd = Command::new(&args.peer_bin);
    cmd.arg("--state")
        .arg(&peer.dir)
        .args(["connect", "--listen", "127.0.0.1:0"])
        .args(["--network-route", &p.network_route]);
    if ticket.exists() {
        cmd.arg("--ticket-file").arg(&ticket);
    }
    let mut child: Process = super::super::spawn(cmd)?;
    super::super::drain_errors(&mut child, network);
    let stdout = child.0.stdout.take().context("gateway stdout")?;
    let (send, receive) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(url) = line.strip_prefix("WebDAV: ") {
                let _ = send.send(url.to_owned());
            }
        }
    });
    let start = Instant::now();
    let url = loop {
        ensure!(!cancel.load(Ordering::Relaxed), "连接已取消");
        ensure!(
            start.elapsed() < Duration::from_secs(60),
            "连接超时，请确认服务器和网络可用"
        );
        ensure!(
            child.0.try_wait()?.is_none(),
            "配对网关已退出，请检查配对状态、网络和占用情况"
        );
        if let Ok(url) = receive.recv_timeout(Duration::from_millis(100)) {
            break url;
        }
    };
    let paired: iroh::EndpointAddr = peer.read("remote.json")?;
    ensure!(paired.id == remote.id, "配对身份不匹配，未连接");
    if ticket.exists() {
        std::fs::remove_file(&ticket)?;
    }
    let backend = backend(p, &url)?;
    backend.list_directory("/")?;
    ensure!(!cancel.load(Ordering::Relaxed), "连接已取消");
    let account_name = p.name.clone();
    Ok(Connection {
        account_name,
        backend,
        binding: format!("peer:{}", remote.id),
        gateway: Some(child),
        worker: json!({"kind":p.kind,"revision":p.revision,"gateway":url}),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use twodrive_dev::state::{Invitation, VERSION, now};
    #[test]
    fn connection_preview_fills_public_hints_without_returning_the_invitation_secret() {
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path().join("private")).unwrap();
        let shared = root.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        state
            .save(
                "share.json",
                &twodrive_dev::state::Share {
                    root: shared,
                    writable: true,
                    address: iroh::EndpointAddr::new(state.identity().unwrap().public()),
                },
            )
            .unwrap();
        let file = root.path().join("connection.json");
        state.export_connection(&file, "Shared PC", false).unwrap();
        let bytes = std::fs::read(&file).unwrap();
        let invitation: Invitation = serde_json::from_slice(&bytes).unwrap();
        let args = json!({"file":file});
        let result = action("import", &args, &AtomicBool::new(false)).unwrap();
        assert_eq!(result["options"]["name"], "Shared PC");
        assert_eq!(result["options"]["writable"], true);
        assert_eq!(
            result["options"]["expected_device"],
            invitation.address.id.to_string()
        );
        assert!(!result.to_string().contains(&invitation.secret));
        let mut mismatch = args.clone();
        mismatch["expected_device"] = json!(iroh::SecretKey::generate().public().to_string());
        assert!(action("import", &mismatch, &AtomicBool::new(false)).is_err());
        let mut expired: Value = serde_json::from_slice(&bytes).unwrap();
        expired["expires"] = json!(now() - 1);
        std::fs::write(&file, serde_json::to_vec(&expired).unwrap()).unwrap();
        assert!(action("import", &args, &AtomicBool::new(false)).is_err());
    }
    #[test]
    fn imported_invitation_is_private_and_pinned_to_selected_device() {
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path().join("client")).unwrap();
        let id = iroh::SecretKey::generate().public();
        let mut invitation = Invitation {
            version: VERSION,
            address: iroh::EndpointAddr::new(id),
            secret: "synthetic-invitation".into(),
            expires: now() + 600,
        };
        let input = root.path().join("invite.json");
        std::fs::write(&input, serde_json::to_vec(&invitation).unwrap()).unwrap();
        let mut p = Profile {
            id: "paired-test".into(),
            kind: "peer".into(),
            ..Default::default()
        };
        p.options.insert("invitation_file".into(), json!(input));
        p.options.insert(
            "expected_device".into(),
            json!(iroh::SecretKey::generate().public().to_string()),
        );
        assert!(prepare(&mut p, &state).is_err());
        assert!(
            !Path::new(p.option("peer_state"))
                .join("pair-invitation.json")
                .exists()
        );
        p.options
            .insert("expected_device".into(), json!(id.to_string()));
        invitation.expires = now() - 1;
        std::fs::write(&input, serde_json::to_vec(&invitation).unwrap()).unwrap();
        assert!(prepare(&mut p, &state).is_err());
        invitation.expires = now() + 600;
        std::fs::write(&input, serde_json::to_vec(&invitation).unwrap()).unwrap();
        prepare(&mut p, &state).unwrap();
        validate(&p).unwrap();
        assert!(!p.options.contains_key("invitation_file"));
        let copy = Path::new(p.option("peer_state")).join("pair-invitation.json");
        let parsed: Invitation = serde_json::from_slice(&std::fs::read(&copy).unwrap()).unwrap();
        assert_eq!(parsed.address.id, id);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(copy).unwrap().permissions().mode() & 0o077,
                0
            );
        }
    }
}
