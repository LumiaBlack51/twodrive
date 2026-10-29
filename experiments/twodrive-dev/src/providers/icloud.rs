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
use twodrive_dev::{profile::Profile, provider::Backend, rclone_drive::RcloneDrive, state::State};

pub fn descriptor() -> Value {
    json!({"id":"icloud","label":"iCloud Drive","account_label":"Apple 账号","icon":"cloud",
        "account_key":"name","summary_key":"name","identity_fields":["icloud_state"],"protected_paths":["icloud_state"],
        "network_modes":["system","direct"],"write_recovery_field":"icloud_state","write_policy":"写入前核对版本并保留本机原文件副本；云端不提供原子版本覆盖，同时编辑仍可能冲突。",
        "capabilities":{"upload":true,"manual_upload":true,"conditional_upload":false,"mkdir":true,"rename":true,"delete":true,"backup":false},
        "help":"填写 Apple 账号和密码后点击登录；需要双重认证时会提示输入验证码。支持浏览、下载及解锁后的手动编辑；覆盖前保留本机恢复副本。",
        "login_action":"configure",
        "actions":[{"id":"configure","label":"登录 iCloud / 更新授权","input_fields":["icloud_state"],"credential_fields":["apple_id","password"]}],
        "fields":[
            {"key":"apple_id","label":"Apple 账号","transient":true,"omit_from_profile":true},
            {"key":"password","label":"密码","secret":true,"transient":true,"omit_from_profile":true},
            {"key":"icloud_state","label":"私有登录目录（自动创建）","advanced":true,"read_only":true}]})
}
pub fn prepare(p: &mut Profile, state: &State) -> anyhow::Result<()> {
    p.options.remove("apple_id");
    p.options.remove("password");
    if p.option("icloud_state").is_empty() {
        p.options.insert(
            "icloud_state".into(),
            json!(state.dir.join("accounts").join(&p.id).join("icloud")),
        );
    }
    Ok(())
}
pub fn login_required(p: &Profile) -> bool {
    let path = Path::new(p.option("icloud_state"));
    path.join("gui-auth.secret").exists()
        || std::fs::read(path.join("auth.json"))
            .ok()
            .and_then(|bytes| {
                serde_json::from_slice::<twodrive_dev::rclone_drive::Auth>(&bytes).ok()
            })
            .is_none_or(|auth| auth.kind != "icloud" || !auth.ready)
}
pub fn validate(p: &Profile) -> anyhow::Result<()> {
    ensure!(
        Path::new(p.option("icloud_state")).is_absolute(),
        "请先使用登录助手创建 iCloud 连接"
    );
    ensure!(
        matches!(p.network_route.as_str(), "system" | "direct"),
        "iCloud 连接请选择系统路由或仅直连"
    );
    Ok(())
}
pub fn connect(
    p: &Profile,
    cancel: &AtomicBool,
    network: Arc<twodrive_dev::network::Diagnostics>,
) -> anyhow::Result<Connection> {
    ensure!(!cancel.load(Ordering::Relaxed), "连接已取消");
    let backend = RcloneDrive::connect(
        State::new(PathBuf::from(p.option("icloud_state")))?,
        &p.network_route,
        cancel,
    )?;
    ensure!(!cancel.load(Ordering::Relaxed), "连接已取消");
    network.record(&format!("network: iCloud route={}", p.network_route));
    let binding = backend.binding.clone();
    Ok(Connection {
        backend: Backend::new(backend.with_writable(p.writable)),
        binding: binding.clone(),
        account_name: p.name.clone(),
        worker: json!({"kind":p.kind,"revision":p.revision,"route":p.network_route,"binding":binding}),
        gateway: None,
    })
}
pub fn restore(p: &Profile, recipe: &Value) -> anyhow::Result<Backend> {
    let route = recipe["route"].as_str().context("missing iCloud route")?;
    let backend = RcloneDrive::connect(
        State::new(PathBuf::from(p.option("icloud_state")))?,
        route,
        &AtomicBool::new(false),
    )?;
    ensure!(
        recipe["binding"].as_str() == Some(backend.binding.as_str()),
        "iCloud 登录已改变，保留原缓存，请重新连接"
    );
    Ok(Backend::new(backend.with_writable(p.writable)))
}
pub fn sign_out(p: &Profile) -> anyhow::Result<()> {
    twodrive_dev::rclone_drive::sign_out(State::new(PathBuf::from(p.option("icloud_state")))?)
}
pub fn action(
    state: &State,
    action: &str,
    args: &Value,
    cancel: &AtomicBool,
) -> anyhow::Result<Value> {
    ensure!(!cancel.load(Ordering::Relaxed), "已取消");
    twodrive_dev::rclone_drive::binary()?;
    let requested = args["icloud_state"].as_str().unwrap_or_default();
    ensure!(
        action == "configure" || !requested.is_empty(),
        "登录步骤缺少账号目录"
    );
    let path = if requested.is_empty() {
        state
            .dir
            .join("accounts")
            .join(format!("icloud-{}", twodrive_dev::state::random_secret()))
    } else {
        PathBuf::from(requested)
    };
    ensure!(path.is_absolute(), "登录目录必须是绝对路径");
    let private = State::new(path)?;
    twodrive_dev::rclone_drive::gui_auth(private, action, args, cancel)
}
