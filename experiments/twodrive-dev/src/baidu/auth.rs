use super::{
    drive::BaiduDrive,
    http::{Api, errno},
};
use crate::{
    provider::valid_path,
    state::{State, now, random_secret},
};
use anyhow::{Context, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use subtle::ConstantTimeEq;

pub(super) const SESSION: &str = "baidu-session.secret";
const PENDING: &str = "baidu-login.secret";
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Session {
    pub app_key: String,
    pub secret_key: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub uk: u64,
    pub name: String,
    pub generation: String,
    #[serde(default)]
    pub identity_pending: bool,
}
#[derive(Serialize, Deserialize)]
struct Pending {
    id: String,
    app_key: String,
    secret_key: String,
    root: String,
    route: String,
    device_code: String,
    user_code: String,
    verification_url: String,
    expires_at: u64,
    next_poll: u64,
    interval: u64,
}
pub(super) fn lock(state: &State, name: &str, exclusive: bool) -> anyhow::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state.dir.join(name))?;
    let result = if exclusive {
        FileExt::try_lock_exclusive(&file)
    } else {
        FileExt::try_lock_shared(&file)
    };
    result.context(if name == "token.lock" {
        "百度授权正在续期，请稍后重试"
    } else {
        "百度账号正在使用，请先断开该账号再授权或退出登录"
    })?;
    Ok(file)
}
pub(super) fn load(state: &State) -> anyhow::Result<Session> {
    let bytes = twodrive_core::private_file::read_secret(&state.dir.join(SESSION))
        .map_err(|_| anyhow::anyhow!("请先在连接表单完成百度官方授权"))?;
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("百度本地授权不可读，请重新授权"))
}
pub(super) fn save(state: &State, session: &Session) -> anyhow::Result<()> {
    twodrive_core::private_file::write_secret(
        &state.dir.join(SESSION),
        &serde_json::to_vec(session)?,
    )
}
pub fn login_required(path: &Path) -> bool {
    !path.join(SESSION).is_file() || path.join(PENDING).exists()
}
pub fn logout(state: &State) -> anyhow::Result<()> {
    let _guard = lock(state, "auth.lock", true)?;
    for name in [SESSION, PENDING] {
        if state.dir.join(name).exists() {
            std::fs::remove_file(state.dir.join(name))?;
        }
    }
    Ok(())
}
pub(super) fn token_response(
    value: &Value,
    previous: Option<&Session>,
) -> anyhow::Result<(String, String, u64)> {
    if let Some(code) = value["error"].as_str() {
        let message = match code {
            "authorization_pending" => "百度网页授权尚未完成",
            "slow_down" => "百度要求稍后再查询授权",
            "expired_token" | "expired_device_code" => "百度授权码已过期，请重新登录",
            "access_denied" | "authorization_declined" => "百度授权被拒绝",
            "invalid_client" | "invalid_client_secret" => "百度应用凭据无效或尚未获准接入",
            _ => "百度授权未完成，请核对应用资格和授权范围",
        };
        anyhow::bail!("{message}");
    }
    let access = value["access_token"]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 16384)
        .context("百度未返回有效访问令牌")?;
    let refresh = value["refresh_token"]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 16384)
        .or_else(|| previous.map(|p| p.refresh_token.as_str()))
        .context("百度未返回刷新令牌")?;
    let expires = value["expires_in"]
        .as_u64()
        .filter(|n| *n > 0 && *n <= 366 * 86400)
        .context("百度令牌有效期无效")?;
    let scope = value["scope"].as_str().context("百度授权缺少权限范围")?;
    ensure!(
        scope.split([',', ' ']).any(|s| s == "netdisk"),
        "百度应用没有网盘权限，请检查开放平台授权范围"
    );
    Ok((access.into(), refresh.into(), now().saturating_add(expires)))
}
pub(super) fn access(
    state: &State,
    api: &Api,
    binding: &str,
    failed_token: Option<&str>,
    check: &mut dyn FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<String> {
    check()?;
    let start = std::time::Instant::now();
    let guard = loop {
        match lock(state, "token.lock", true) {
            Ok(g) => break g,
            Err(_) if start.elapsed() < Duration::from_secs(5) => {
                super::http::delay(Duration::from_millis(50), check)?;
            }
            Err(e) => return Err(e),
        }
    };
    let mut session = load(state)?;
    ensure!(
        session.generation == binding,
        "百度登录账号已改变，拒绝旧连接"
    );
    if failed_token.is_some_and(|t| t == session.access_token)
        || session.expires_at <= now().saturating_add(60)
    {
        check()?;
        let value = api.oauth(
            "/oauth/2.0/token",
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", &session.refresh_token),
                ("client_id", &session.app_key),
                ("client_secret", &session.secret_key),
            ],
        )?;
        let (access, refresh, expires) = token_response(&value, Some(&session))?;
        session.access_token = access;
        session.refresh_token = refresh;
        session.expires_at = expires;
        // Persist rotation before another fallible/cancellable network call.
        // Every reader must finish identity verification before using it.
        session.identity_pending = true;
        save(state, &session)?;
    }
    if session.identity_pending {
        let identity = api.get(
            "/rest/2.0/xpan/nas",
            &session.access_token,
            &[("method", "uinfo")],
            check,
        )?;
        errno(&identity)?;
        if identity["uk"]
            .as_u64()
            .or_else(|| identity["uk"].as_str()?.parse().ok())
            != Some(session.uk)
        {
            session.generation = random_secret();
            save(state, &session)?;
            anyhow::bail!("百度刷新授权返回了其他账号，旧连接已拒绝使用");
        }
        session.identity_pending = false;
        save(state, &session)?;
    }
    drop(guard);
    check()?;
    Ok(session.access_token)
}
fn write_pending(state: &State, pending: &Pending) -> anyhow::Result<()> {
    twodrive_core::private_file::write_secret(
        &state.dir.join(PENDING),
        &serde_json::to_vec(pending)?,
    )
}
fn challenge(state: &State, pending: &mut Pending, message: Option<&str>) -> anyhow::Result<Value> {
    pending.id = random_secret();
    write_pending(state, pending)?;
    Ok(
        json!({"auth":{"title":"百度网盘官方授权","confirmation":true,"submit_label":"我已授权，继续",
        "help":"请在已打开的百度官方网页登录，输入下方设备授权码并同意授权，再回到这里继续。无需在 TwoDrive 输入百度账号密码。",
        "display_code":pending.user_code,"verification_url":pending.verification_url,"message":message,
        "attempt":pending.id,"reply_action":"auth_reply","cancel_action":"auth_cancel",
        "context":{"baidu_state":state.dir},"alternatives":[{"label":"重新打开授权网页","value":"open"}]}}),
    )
}
fn open_browser(url: &str) -> anyhow::Result<()> {
    use std::process::{Command, Stdio};
    let url = url::Url::parse(url).context("百度授权地址无效")?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("openapi.baidu.com")
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none(),
        "拒绝非官方百度授权地址"
    );
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("rundll32.exe");
        c.arg("url.dll,FileProtocolHandler");
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x08000000);
        c
    };
    #[cfg(target_os = "linux")]
    let mut cmd = Command::new("xdg-open");
    #[cfg(target_os = "macos")]
    let mut cmd = Command::new("open");
    cmd.arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("无法打开授权网页，请手动打开界面中的官方地址")?;
    Ok(())
}
pub fn action(
    state: State,
    action: &str,
    args: &Value,
    cancel: &AtomicBool,
) -> anyhow::Result<Value> {
    action_with(&state, action, args, cancel, None)
}
pub(super) fn action_with(
    state: &State,
    action: &str,
    args: &Value,
    cancel: &AtomicBool,
    local: Option<Api>,
) -> anyhow::Result<Value> {
    let mut check = || {
        ensure!(!cancel.load(Ordering::Relaxed), "百度授权已取消");
        Ok(())
    };
    check()?;
    let _guard = lock(state, "auth.lock", true)?;
    if action == "configure" {
        let app = args["app_key"].as_str().unwrap_or_default().trim();
        let secret = args["secret_key"].as_str().unwrap_or_default().trim();
        ensure!(
            !app.is_empty() && !secret.is_empty(),
            "请先填写你自己的百度应用 AppKey 和 SecretKey；无需填写百度账号密码"
        );
        ensure!(
            [app, secret].iter().all(|s| s.len() <= 512
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')),
            "百度应用凭据格式无效"
        );
        let root = args["root_path"]
            .as_str()
            .filter(|v| !v.is_empty())
            .unwrap_or("/");
        valid_path(root)?;
        let route = args["network_route"].as_str().unwrap_or("system");
        let api = local.unwrap_or(Api::new(route)?);
        let value = api.oauth(
            "/oauth/2.0/device/code",
            &[
                ("response_type", "device_code"),
                ("client_id", app),
                ("scope", "basic,netdisk"),
            ],
        )?;
        if value["error"].is_string() {
            token_response(&value, None)?;
        }
        let device = value["device_code"]
            .as_str()
            .filter(|v| !v.is_empty() && v.len() < 4096)
            .context("百度未返回设备授权码")?;
        let user = value["user_code"]
            .as_str()
            .filter(|v| {
                !v.is_empty()
                    && v.len() <= 64
                    && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            })
            .context("百度设备显示码无效")?;
        let verify = value["verification_url"]
            .as_str()
            .context("百度未返回授权网页")?;
        let parsed = url::Url::parse(verify).context("百度授权地址无效")?;
        ensure!(
            parsed.scheme() == "https"
                && parsed.host_str() == Some("openapi.baidu.com")
                && parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.port_or_known_default() == Some(443),
            "拒绝非官方百度授权网页"
        );
        let expires = value["expires_in"]
            .as_u64()
            .filter(|v| *v > 0 && *v <= 3600)
            .context("百度设备授权有效期无效")?;
        let mut pending = Pending {
            id: String::new(),
            app_key: app.into(),
            secret_key: secret.into(),
            root: root.into(),
            route: route.into(),
            device_code: device.into(),
            user_code: user.into(),
            verification_url: verify.into(),
            expires_at: now() + expires,
            next_poll: 0,
            interval: value["interval"].as_u64().unwrap_or(5).clamp(1, 60),
        };
        let answer = challenge(state, &mut pending, None)?;
        #[cfg(test)]
        if api.test_origin.is_some() {
            return Ok(answer);
        }
        // Browser failure leaves the official URL/code visible in the GUI.
        let _ = open_browser(verify);
        return Ok(answer);
    }
    let bytes = twodrive_core::private_file::read_secret(&state.dir.join(PENDING))
        .context("百度登录步骤已失效，请重新开始")?;
    let mut pending: Pending = serde_json::from_slice(&bytes).context("百度登录步骤不可读")?;
    let attempt = args["attempt"].as_str().context("缺少百度登录步骤")?;
    ensure!(
        bool::from(pending.id.as_bytes().ct_eq(attempt.as_bytes())),
        "百度登录步骤已过期，旧步骤不能操作新登录"
    );
    if action == "auth_cancel" {
        std::fs::remove_file(state.dir.join(PENDING))?;
        return Ok(json!({"cancelled":true,"message":"已取消百度登录，原授权保留"}));
    }
    ensure!(action == "auth_reply", "无效的百度登录动作");
    ensure!(
        pending.expires_at > now(),
        "百度设备码已过期，请取消后重新授权"
    );
    let api = local.unwrap_or(Api::new(&pending.route)?);
    if args["answer"] == "open" {
        #[cfg(test)]
        if api.test_origin.is_some() {
            return challenge(state, &mut pending, None);
        }
        open_browser(&pending.verification_url)?;
        return challenge(state, &mut pending, None);
    }
    if now() < pending.next_poll {
        return challenge(
            state,
            &mut pending,
            Some("请先在百度网页完成授权，稍后点击继续"),
        );
    }
    pending.next_poll = now() + pending.interval;
    write_pending(state, &pending)?;
    let value = api.oauth(
        "/oauth/2.0/token",
        &[
            ("grant_type", "device_token"),
            ("code", &pending.device_code),
            ("client_id", &pending.app_key),
            ("client_secret", &pending.secret_key),
        ],
    )?;
    if matches!(
        value["error"].as_str(),
        Some("authorization_pending" | "slow_down")
    ) {
        if value["error"] == "slow_down" {
            pending.interval = (pending.interval + 5).min(60);
            pending.next_poll = now() + pending.interval;
        }
        return challenge(
            state,
            &mut pending,
            Some("百度尚未确认授权，请在官方网页完成后再继续"),
        );
    }
    let (access, refresh, expires) = token_response(&value, None)?;
    let user = api.get(
        "/rest/2.0/xpan/nas",
        &access,
        &[("method", "uinfo")],
        &mut check,
    )?;
    errno(&user)?;
    let uk = user["uk"]
        .as_u64()
        .filter(|v| *v > 0)
        .context("百度未返回稳定账号标识")?;
    let name = user["netdisk_name"]
        .as_str()
        .filter(|v| !v.is_empty())
        .or_else(|| user["baidu_name"].as_str())
        .unwrap_or("百度网盘账号")
        .to_owned();
    let session = Session {
        app_key: pending.app_key,
        secret_key: pending.secret_key,
        access_token: access,
        refresh_token: refresh,
        expires_at: expires,
        uk,
        name,
        generation: random_secret(),
        identity_pending: false,
    };
    // Validate the requested authorization root before replacing any old login.
    BaiduDrive::validate_root(&api, &session, &pending.root, &mut check)?;
    check()?;
    save(state, &session)?;
    std::fs::remove_file(state.dir.join(PENDING))?;
    Ok(
        json!({"authenticated":true,"message":"百度官方授权已完成，可保存并连接；当前只读", "options":{"baidu_state":state.dir,"root_path":pending.root}}),
    )
}
