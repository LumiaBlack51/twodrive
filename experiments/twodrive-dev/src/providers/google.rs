use super::Connection;
use anyhow::{Context, ensure};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use twodrive_dev::{
    google::{
        GoogleDrive,
        auth::{self, ClientConfig, Tokens},
    },
    profile::Profile,
    provider::Backend,
    state::State,
};

pub fn descriptor() -> Value {
    json!({"id":"google","label":"Google Drive","account_label":"Google 账号","icon":"cloud",
        "account_key":"name","summary_key":"name","identity_fields":["google_state","client_file"],"protected_paths":["google_state","client_file"],"mount_startup_timeout_seconds":600,
        "capabilities":{"mount":true,"upload":true,"conditional_upload":true,"mkdir":true,"rename":true,"delete":true,"backup":true},
        "help":"连接时打开系统浏览器授权 Google Drive 读写权限。在线文档显示为网页快捷入口；删除移入回收站。",
        "fields":[{"key":"client_file","label":"Google 桌面应用凭据 JSON","required":true,"type":"file"},{"key":"google_state","label":"私有登录目录（留空自动创建）","advanced":true}]})
}
pub fn prepare(p: &mut Profile, state: &State) -> anyhow::Result<()> {
    if p.option("google_state").is_empty() {
        p.options.insert(
            "google_state".into(),
            json!(state.dir.join("accounts").join(&p.id).join("google")),
        );
    }
    Ok(())
}
fn state(p: &Profile) -> anyhow::Result<State> {
    State::new(PathBuf::from(p.option("google_state")))
}
pub fn validate(p: &Profile) -> anyhow::Result<()> {
    ensure!(
        Path::new(p.option("google_state")).is_absolute(),
        "Google 登录目录必须是绝对路径"
    );
    ensure!(
        Path::new(p.option("client_file")).is_absolute(),
        "Google 凭据路径必须是绝对路径"
    );
    ClientConfig::import(Path::new(p.option("client_file")))?;
    Ok(())
}
pub fn connect(
    p: &Profile,
    cancel: &AtomicBool,
    network: Arc<twodrive_dev::network::Diagnostics>,
    expected: Option<&str>,
) -> anyhow::Result<Connection> {
    open(p, cancel, network, true, expected)
}
pub fn resume(
    p: &Profile,
    cancel: &AtomicBool,
    network: Arc<twodrive_dev::network::Diagnostics>,
) -> anyhow::Result<Connection> {
    open(p, cancel, network, false, None)
}
fn open(
    p: &Profile,
    cancel: &AtomicBool,
    network: Arc<twodrive_dev::network::Diagnostics>,
    interactive: bool,
    expected: Option<&str>,
) -> anyhow::Result<Connection> {
    ensure!(!cancel.load(Ordering::Relaxed), "连接已取消");
    let state = state(p)?;
    let config = ClientConfig::import(Path::new(p.option("client_file")))?;
    let route = if p.network_route == "direct" {
        "direct"
    } else {
        "system"
    };
    let authorize = || {
        auth::login_checked(&state, &config, route, cancel, |token| {
            if let Some(expected) = expected {
                GoogleDrive::check_login_identity(route, token, expected)?;
            }
            Ok(())
        })
    };
    let connect = || GoogleDrive::connect(state.clone(), route, p.writable);
    let backend = connect_with_consent(
        Tokens::load(&state, &config).is_ok(),
        interactive,
        connect,
        authorize,
    )?;
    let binding = format!("google:{}", backend.account_id);
    ensure!(!cancel.load(Ordering::Relaxed), "连接已取消");
    network.record(&format!("network: Google Drive route={route}"));
    let account_name = backend.account_name.clone();
    Ok(Connection {
        account_name,
        backend: Backend::new(backend),
        binding: binding.clone(),
        worker: json!({"kind":"google","revision":p.revision,"route":route,"binding":binding}),
        gateway: None,
    })
}
fn connect_with_consent<T>(
    has_tokens: bool,
    interactive: bool,
    mut connect: impl FnMut() -> anyhow::Result<T>,
    authorize: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<T> {
    if !has_tokens {
        ensure!(interactive, "Google 登录已失效，请手动重新授权");
        authorize()?;
        connect()
    } else {
        match connect() {
            Err(error) if interactive && error.is::<auth::ReauthorizationRequired>() => {
                authorize()?;
                connect()
            }
            result => result,
        }
    }
}
pub fn restore(p: &Profile, recipe: &Value) -> anyhow::Result<Backend> {
    let route = recipe["route"].as_str().context("missing Google route")?;
    let backend = GoogleDrive::connect(state(p)?, route, p.writable)?;
    ensure!(
        recipe["binding"].as_str() == Some(format!("google:{}", backend.account_id).as_str()),
        "Google worker 登录账号已改变；保留原缓存，请重新连接"
    );
    Ok(Backend::new(backend))
}
pub fn sign_out(p: &Profile) -> anyhow::Result<()> {
    auth::logout(&state(p)?)
}

#[cfg(test)]
mod consent_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn a_rejected_grant_prompts_only_on_explicit_connect_and_retries_once() {
        for interactive in [true, false] {
            let calls = Cell::new(0);
            let prompts = Cell::new(0);
            let result = connect_with_consent(
                true,
                interactive,
                || {
                    let n = calls.replace(calls.get() + 1);
                    if n == 0 {
                        Err(auth::ReauthorizationRequired.into())
                    } else {
                        Ok("original account")
                    }
                },
                || {
                    prompts.set(prompts.get() + 1);
                    Ok(())
                },
            );
            assert_eq!(result.is_ok(), interactive);
            assert_eq!(calls.get(), if interactive { 2 } else { 1 });
            assert_eq!(prompts.get(), u32::from(interactive));
        }
    }

    #[test]
    fn cancellation_or_transport_failure_never_loops_into_consent() {
        let result = connect_with_consent::<()>(
            true,
            true,
            || Err(anyhow::anyhow!("network")),
            || panic!("unexpected consent"),
        );
        assert_eq!(result.unwrap_err().to_string(), "network");
        let result = connect_with_consent::<()>(
            false,
            true,
            || panic!("unexpected connect"),
            || anyhow::bail!("cancel"),
        );
        assert_eq!(result.unwrap_err().to_string(), "cancel");
    }
}
