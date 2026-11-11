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
    baidu::{BaiduDrive, auth},
    network::Diagnostics,
    profile::Profile,
    provider::{Backend, valid_path},
    state::State,
};

pub fn descriptor() -> Value {
    json!({"id":"baidu","mount_backend":"readonly_bridge","label":"百度网盘","account_label":"百度账号","icon":"cloud",
        "identity_fields":["baidu_state","root_path"],"protected_paths":["baidu_state"],
        "network_modes":["system","direct"],"write_recovery_field":"baidu_state","write_policy":"覆盖前核对版本并保留本机恢复副本；云端不提供原子版本覆盖，同时编辑仍可能冲突。","login_action":"configure",
        "help":"使用你自己的百度开放平台应用，在百度官方网页授权。解锁后可手动编辑；可见及可写文件受应用权限和授权根目录限制。AppKey、SecretKey 是应用凭据，无需在此输入百度账号密码。",
        "capabilities":{"mount":true,"upload":true,"manual_upload":true,"conditional_upload":false,"mkdir":true,"rename":true,"delete":true,"backup":false},
        "actions":[{"id":"configure","label":"百度官方网页登录 / 更新授权","input_fields":["baidu_state","root_path"],"credential_fields":["app_key","secret_key"]}],
        "fields":[{"key":"app_key","label":"应用 AppKey / API Key","transient":true,"omit_from_profile":true},
            {"key":"secret_key","label":"应用 SecretKey","secret":true,"transient":true,"omit_from_profile":true},
            {"key":"root_path","label":"授权根目录（默认 /，应用目录授权请填写对应路径）"},
            {"key":"baidu_state","label":"私有登录目录（自动创建）","advanced":true,"read_only":true}]})
}
pub fn prepare(p: &mut Profile, state: &State) -> anyhow::Result<()> {
    p.options.remove("app_key");
    p.options.remove("secret_key");
    if p.option("baidu_state").is_empty() {
        p.options.insert(
            "baidu_state".into(),
            json!(state.dir.join("accounts").join(&p.id).join("baidu")),
        );
    }
    if p.option("root_path").is_empty() {
        p.options.insert("root_path".into(), json!("/"));
    }
    Ok(())
}
pub fn validate(p: &Profile) -> anyhow::Result<()> {
    ensure!(
        Path::new(p.option("baidu_state")).is_absolute(),
        "百度登录目录必须是绝对路径"
    );
    valid_path(p.option("root_path"))?;
    ensure!(
        matches!(p.network_route.as_str(), "system" | "direct"),
        "百度请选择系统路由或仅直连"
    );
    Ok(())
}
pub fn login_required(p: &Profile) -> bool {
    auth::login_required(Path::new(p.option("baidu_state")))
}
pub fn connect(
    p: &Profile,
    cancel: &AtomicBool,
    network: Arc<Diagnostics>,
) -> anyhow::Result<Connection> {
    let backend = BaiduDrive::connect(
        State::new(PathBuf::from(p.option("baidu_state")))?,
        p.option("root_path"),
        &p.network_route,
        &mut || {
            ensure!(!cancel.load(Ordering::Relaxed), "百度连接已取消");
            Ok(())
        },
    )?;
    network.record("network: Baidu official API");
    let binding = backend.binding.clone();
    let account_name = backend.account_name.clone();
    Ok(Connection {
        backend: Backend::new(backend.with_writable(p.writable)),
        binding: binding.clone(),
        account_name,
        worker: json!({"kind":p.kind,"revision":p.revision,"binding":binding,"route":p.network_route}),
        gateway: None,
    })
}
pub fn restore(p: &Profile, recipe: &Value) -> anyhow::Result<Backend> {
    let backend = BaiduDrive::connect(
        State::new(PathBuf::from(p.option("baidu_state")))?,
        p.option("root_path"),
        recipe["route"].as_str().context("missing Baidu route")?,
        &mut || Ok(()),
    )?;
    ensure!(
        recipe["binding"].as_str() == Some(backend.binding.as_str()),
        "百度账号、存储根或缓存身份已改变，旧缓存保留，请重新连接"
    );
    Ok(Backend::new(backend.with_writable(p.writable)))
}
pub fn resume(
    p: &Profile,
    session: &Value,
    cancel: &AtomicBool,
    network: Arc<Diagnostics>,
) -> anyhow::Result<Connection> {
    let connection = connect(p, cancel, network)?;
    ensure!(
        session["binding"].as_str() == Some(connection.binding.as_str()),
        "百度账号、存储根或缓存身份已更新，请手动重新连接；旧缓存保留，不会自动重放"
    );
    Ok(connection)
}
pub fn sign_out(p: &Profile) -> anyhow::Result<()> {
    auth::logout(&State::new(PathBuf::from(p.option("baidu_state")))?)
}
pub fn action(
    state: &State,
    action: &str,
    args: &Value,
    cancel: &AtomicBool,
) -> anyhow::Result<Value> {
    let requested = args["baidu_state"].as_str().unwrap_or_default();
    ensure!(
        action == "configure" || !requested.is_empty(),
        "百度登录步骤缺少私有目录"
    );
    let path = if requested.is_empty() {
        state
            .dir
            .join("accounts")
            .join(format!("baidu-{}", twodrive_dev::state::random_secret()))
    } else {
        PathBuf::from(requested)
    };
    ensure!(path.is_absolute(), "百度登录目录必须是绝对路径");
    auth::action(State::new(path)?, action, args, cancel)
}
