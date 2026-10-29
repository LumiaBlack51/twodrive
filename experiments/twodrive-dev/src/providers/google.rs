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
) -> anyhow::Result<Connection> {
    open(p, cancel, network, true)
}
pub fn resume(
    p: &Profile,
    cancel: &AtomicBool,
    network: Arc<twodrive_dev::network::Diagnostics>,
) -> anyhow::Result<Connection> {
    open(p, cancel, network, false)
}
fn open(
    p: &Profile,
    cancel: &AtomicBool,
    network: Arc<twodrive_dev::network::Diagnostics>,
    interactive: bool,
) -> anyhow::Result<Connection> {
    ensure!(!cancel.load(Ordering::Relaxed), "连接已取消");
    let state = state(p)?;
    let config = ClientConfig::import(Path::new(p.option("client_file")))?;
    let route = if p.network_route == "direct" {
        "direct"
    } else {
        "system"
    };
    if Tokens::load(&state, &config).is_err() {
        ensure!(interactive, "Google 登录已失效，请手动重新授权");
        auth::login(&state, &config, route, cancel)?;
    }
    let backend = GoogleDrive::connect(state, route, p.writable)?;
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
