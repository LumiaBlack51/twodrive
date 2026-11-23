use crate::state::{State, now, random_secret};
use anyhow::{Context, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use fs2::FileExt;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use url::Url;

pub const SCOPE: &str = "https://www.googleapis.com/auth/drive";
const AUTH: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN: &str = "https://oauth2.googleapis.com/token";

#[derive(Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub client_id: String,
}
impl ClientConfig {
    pub fn import(path: &Path) -> anyhow::Result<Self> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .context("无法打开 Google 桌面应用凭据文件")?
            .take(65537)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 65536, "Google 凭据文件过大");
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("Google 凭据文件不是有效 JSON"))?;
        let installed = value
            .get("installed")
            .context("请使用 Google 桌面应用类型的凭据 JSON")?;
        // Imported endpoints are data, never authority for where to send secrets.
        let config: Self = serde_json::from_value(installed.clone())
            .map_err(|_| anyhow::anyhow!("Google 凭据缺少 client_id"))?;
        ensure!(
            config.client_id.ends_with(".apps.googleusercontent.com")
                && config.client_id.len() < 512
                && !config.client_id.contains(char::is_whitespace),
            "无效的 Google client_id"
        );
        Ok(config)
    }
    pub fn save(&self, state: &State) -> anyhow::Result<()> {
        twodrive_core::private_file::write_secret(
            &state.dir.join("google-client.json"),
            &serde_json::to_vec(self)?,
        )
    }
    pub fn load(state: &State) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(
            &twodrive_core::private_file::read_secret(&state.dir.join("google-client.json"))?,
        )?)
    }
}
impl Tokens {
    pub fn load(state: &State, config: &ClientConfig) -> anyhow::Result<Self> {
        let token: Self = serde_json::from_slice(&twodrive_core::private_file::read_secret(
            &state.dir.join("google-tokens.json"),
        )?)
        .map_err(|_| anyhow::anyhow!("Google 本地令牌不可读，请重新登录"))?;
        ensure!(
            token.client_id == config.client_id,
            "Google 令牌属于另一应用，请重新登录"
        );
        Ok(token)
    }
    fn save(&self, state: &State) -> anyhow::Result<()> {
        twodrive_core::private_file::write_secret(
            &state.dir.join("google-tokens.json"),
            &serde_json::to_vec(self)?,
        )
    }
}
pub fn client(route: &str) -> anyhow::Result<Client> {
    ensure!(
        matches!(route, "direct" | "system"),
        "invalid Google network route"
    );
    let builder = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120));
    Ok(if route == "direct" {
        builder.no_proxy()
    } else {
        builder
    }
    .build()?)
}
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}
fn authorization_url(config: &ClientConfig, redirect: &str, state: &str, verifier: &str) -> Url {
    let mut url = Url::parse(AUTH).unwrap();
    url.query_pairs_mut().extend_pairs([
        ("client_id", config.client_id.as_str()),
        ("redirect_uri", redirect),
        ("response_type", "code"),
        ("scope", SCOPE),
        ("state", state),
        ("code_challenge", &challenge(verifier)),
        ("code_challenge_method", "S256"),
        ("access_type", "offline"),
        ("prompt", "consent select_account"),
    ]);
    url
}
fn callback(request: &str, expected: &str) -> anyhow::Result<Option<String>> {
    let url = Url::parse(&format!("http://127.0.0.1{request}"))
        .map_err(|_| anyhow::anyhow!("无效的登录回调"))?;
    if url.path() != "/oauth2/callback" {
        return Ok(None);
    }
    let values: Vec<_> = url.query_pairs().collect();
    let get = |key: &str| -> anyhow::Result<Option<&str>> {
        let v: Vec<_> = values.iter().filter(|(k, _)| k == key).collect();
        ensure!(v.len() <= 1, "重复的登录回调参数");
        Ok(v.first().map(|(_, v)| v.as_ref()))
    };
    let actual = get("state")?.unwrap_or_default();
    if !bool::from(actual.as_bytes().ct_eq(expected.as_bytes())) {
        return Ok(None);
    }
    ensure!(
        get("error")?.is_none(),
        "Google 授权被拒绝，请重新登录并允许 Drive 访问"
    );
    let code = get("code")?.context("登录回调缺少授权码")?;
    ensure!(!code.is_empty() && code.len() < 8192, "无效的授权码");
    Ok(Some(code.into()))
}
fn open_browser(url: &Url) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    let mut command = Command::new("xdg-open");
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(windows)]
    let mut command = {
        let mut c = Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    command
        .arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("无法打开系统浏览器")?;
    Ok(())
}
#[derive(Debug, thiserror::Error)]
#[error("Google 授权已失效，请重新授权")]
pub struct ReauthorizationRequired;

fn token_error(status: u16, body: &[u8], refreshing: bool) -> anyhow::Error {
    let error = serde_json::from_slice::<Value>(body).unwrap_or_default();
    match error["error"].as_str() {
        Some("invalid_grant") if refreshing => ReauthorizationRequired.into(),
        Some("invalid_client" | "unauthorized_client") => {
            anyhow::anyhow!("Google 应用凭据无效，请检查连接设置")
        }
        _ => anyhow::anyhow!("Google 令牌请求失败（HTTP {status}）"),
    }
}
fn decode_token(
    response: reqwest::blocking::Response,
    config: &ClientConfig,
    previous: Option<&Tokens>,
) -> anyhow::Result<Tokens> {
    let status = response.status();
    let mut bytes = Vec::new();
    response.take(65537).read_to_end(&mut bytes)?;
    if !status.is_success() {
        return Err(token_error(status.as_u16(), &bytes, previous.is_some()));
    }
    ensure!(bytes.len() <= 65536, "Google 令牌响应过大");
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("无效的 Google 令牌响应"))?;
    ensure!(
        value["token_type"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("Bearer")),
        "Google 返回不支持的令牌类型"
    );
    if let Some(scope) = value["scope"].as_str() {
        ensure!(
            scope.split_whitespace().any(|s| s == SCOPE),
            "未授予 Google Drive 读写权限，请重新登录并勾选授权"
        );
    }
    let access = value["access_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("Google 未返回访问令牌")?;
    let refresh = value["refresh_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| previous.map(|t| t.refresh_token.as_str()))
        .context("Google 未返回离线令牌，请重新授权")?;
    let expires = value["expires_in"]
        .as_u64()
        .filter(|s| *s > 0)
        .context("Google 未返回令牌有效期")?;
    Ok(Tokens {
        access_token: access.into(),
        refresh_token: refresh.into(),
        expires_at: now().saturating_add(expires),
        client_id: config.client_id.clone(),
    })
}
pub fn login(
    state: &State,
    config: &ClientConfig,
    route: &str,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    login_checked(state, config, route, cancel, |_| Ok(()))
}

pub fn login_checked(
    state: &State,
    config: &ClientConfig,
    route: &str,
    cancel: &AtomicBool,
    check_identity: impl FnOnce(&str) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let _lock = lock(state, "google-auth.lock")?;
    let server = tiny_http::Server::http("127.0.0.1:0")
        .map_err(|_| anyhow::anyhow!("无法启动本机登录回调"))?;
    let redirect = format!(
        "http://127.0.0.1:{}/oauth2/callback",
        server.server_addr().to_ip().unwrap().port()
    );
    let expected = random_secret();
    let verifier = random_secret();
    let url = authorization_url(config, &redirect, &expected, &verifier);
    open_browser(&url)?;
    let deadline = Instant::now() + Duration::from_secs(600);
    let code = loop {
        ensure!(!cancel.load(Ordering::Relaxed), "Google 登录已取消");
        ensure!(Instant::now() < deadline, "Google 登录等待超时，请重试");
        let Some(request) = server.recv_timeout(Duration::from_millis(200))? else {
            continue;
        };
        if request.method() != &tiny_http::Method::Get || request.url().len() > 16384 {
            let _ = request.respond(
                tiny_http::Response::from_string("Invalid callback").with_status_code(400),
            );
            continue;
        }
        match callback(request.url(), &expected) {
            Ok(Some(code)) => {
                let _ = request.respond(tiny_http::Response::from_string("TwoDrive: authorization received. Return to TwoDrive to check sign-in status.")
                    .with_header(tiny_http::Header::from_bytes("Cache-Control", "no-store").unwrap()));
                break code;
            }
            Ok(None) => {
                let _ = request.respond(
                    tiny_http::Response::from_string("Invalid callback").with_status_code(400),
                );
            }
            Err(e) => {
                let _ = request.respond(
                    tiny_http::Response::from_string(
                        "TwoDrive: authorization declined or invalid.",
                    )
                    .with_status_code(400),
                );
                return Err(e);
            }
        }
    };
    ensure!(!cancel.load(Ordering::Relaxed), "Google 登录已取消");
    let response = client(route)?
        .post(TOKEN)
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("code", &code),
            ("code_verifier", &verifier),
            ("redirect_uri", &redirect),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .map_err(|_| anyhow::anyhow!("Google 令牌请求连接失败，请检查网络"))?;
    let tokens = decode_token(response, config, None)?;
    check_identity(&tokens.access_token)?;
    ensure!(!cancel.load(Ordering::Relaxed), "Google 登录已取消");
    config.save(state)?;
    tokens.save(state)
}
pub fn access_token(state: &State, client: &Client) -> anyhow::Result<String> {
    // Across desktop, mount and backup workers, reload under a shared process lock.
    let _lock = lock(state, "google-auth.lock")?;
    let config = ClientConfig::load(state)?;
    let token = Tokens::load(state, &config)?;
    if token.expires_at > now() + 60 {
        return Ok(token.access_token);
    }
    let response = client
        .post(TOKEN)
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("refresh_token", token.refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .map_err(|_| anyhow::anyhow!("Google 刷新令牌连接失败，请检查网络"))?;
    let refreshed = decode_token(response, &config, Some(&token))?;
    refreshed.save(state)?;
    Ok(refreshed.access_token)
}
pub(crate) fn lock(state: &State, name: &str) -> anyhow::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(state.dir.join(name))?;
    file.lock_exclusive()?;
    Ok(file)
}
pub fn logout(state: &State) -> anyhow::Result<()> {
    let _lock = lock(state, "google-auth.lock")?;
    for name in ["google-tokens.json", "google-client.json"] {
        let path = state.dir.join(name);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pkce_and_callback_bind_the_attempt() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(
            callback("/oauth2/callback?state=good&code=c", "good").unwrap(),
            Some("c".into())
        );
        assert!(
            callback("/oauth2/callback?state=bad&code=c", "good")
                .unwrap()
                .is_none()
        );
        assert!(
            callback("/else?state=good&code=c", "good")
                .unwrap()
                .is_none()
        );
        assert!(callback("/oauth2/callback?state=good&state=bad&code=c", "good").is_err());
        let e = callback(
            "/oauth2/callback?state=good&error=secret-untrusted-text",
            "good",
        )
        .unwrap_err();
        assert!(!e.to_string().contains("secret-untrusted-text"));
        let c = ClientConfig {
            client_id: "example.apps.googleusercontent.com".into(),
            client_secret: "secret".into(),
        };
        let u = authorization_url(&c, "http://127.0.0.1:123/oauth2/callback", "s", "v");
        assert!(!u.as_str().contains("secret"));
        assert!(u.query_pairs().any(|(k, v)| k == "scope" && v == SCOPE));
    }
    #[test]
    fn imported_endpoints_cannot_redirect_secrets_and_tokens_bind_client() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("input.json");
        std::fs::write(&file, r#"{"installed":{"client_id":"example.apps.googleusercontent.com","client_secret":"synthetic","token_uri":"https://evil.invalid"}}"#).unwrap();
        let config = ClientConfig::import(&file).unwrap();
        let state = State::new(dir.path().join("private")).unwrap();
        config.save(&state).unwrap();
        Tokens {
            client_id: config.client_id.clone(),
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: now() + 3600,
        }
        .save(&state)
        .unwrap();
        assert!(Tokens::load(&state, &config).is_ok());
        let other = ClientConfig {
            client_id: "other.apps.googleusercontent.com".into(),
            client_secret: String::new(),
        };
        assert!(Tokens::load(&state, &other).is_err());
        logout(&state).unwrap();
        assert!(!state.dir.join("google-tokens.json").exists());
    }
}

#[cfg(test)]
mod refresh_errors {
    use super::*;
    #[test]
    fn only_a_rejected_refresh_grant_requires_consent() {
        let body = br#"{"error":"invalid_grant","error_description":"private detail"}"#;
        assert!(token_error(400, body, true).is::<ReauthorizationRequired>());
        assert!(!token_error(400, body, false).is::<ReauthorizationRequired>());
        for body in [
            br#"{"error":"invalid_client"}"#.as_slice(),
            b"temporarily unavailable",
        ] {
            assert!(!token_error(400, body, true).is::<ReauthorizationRequired>());
        }
        assert!(
            !token_error(400, body, false)
                .to_string()
                .contains("private detail")
        );
    }
}
