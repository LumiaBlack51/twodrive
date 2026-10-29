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
use twodrive_core::TokenStore;
use twodrive_dev::{
    network::Diagnostics,
    onedrive::{OneDrive, paths},
    profile::Profile,
    provider::Backend,
    state::State,
};

pub fn descriptor() -> Value {
    json!({"id":"onedrive","label":"OneDrive","account_label":"Microsoft 账号","icon":"cloud",
        "identity_fields":["onedrive_state"],"network_modes":["system"],"protected_paths":["onedrive_state"],"mount_startup_timeout_seconds":600,
        "help":"保存后点击连接，在系统浏览器登录 Microsoft 账号。支持个人/组织 OneDrive；使用独立登录数据。",
        "capabilities":{"mount":true,"upload":true,"conditional_upload":true,"mkdir":true,"rename":true,"delete":true,"backup":true},
        "fields":[{"key":"onedrive_state","label":"私有登录目录（留空自动创建）","advanced":true}]})
}
pub fn prepare(p: &mut Profile, state: &State) -> anyhow::Result<()> {
    if p.network_route == "auto" {
        p.network_route = "system".into();
    }
    if p.option("onedrive_state").is_empty() {
        p.options.insert(
            "onedrive_state".into(),
            json!(state.dir.join("accounts").join(&p.id).join("onedrive")),
        );
    }
    Ok(())
}
pub fn validate(p: &Profile) -> anyhow::Result<()> {
    ensure!(
        Path::new(p.option("onedrive_state")).is_absolute(),
        "OneDrive 登录目录必须是绝对路径"
    );
    ensure!(
        p.network_route == "system",
        "OneDrive 当前只支持系统路由和代理"
    );
    Ok(())
}
fn directory(p: &Profile) -> PathBuf {
    PathBuf::from(p.option("onedrive_state"))
}
fn binding(backend: &OneDrive, cancel: &AtomicBool) -> anyhow::Result<String> {
    let identity = backend.graph.readonly_identity(&mut || {
        ensure!(!cancel.load(Ordering::Relaxed), "连接已取消");
        Ok(())
    })?;
    Ok(format!("onedrive:{}:{}", identity.account, identity.drive))
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
    network: Arc<Diagnostics>,
) -> anyhow::Result<Connection> {
    open(p, cancel, network, false)
}
fn open(
    p: &Profile,
    cancel: &AtomicBool,
    network: Arc<Diagnostics>,
    interactive: bool,
) -> anyhow::Result<Connection> {
    let paths = paths(&directory(p))?;
    if TokenStore::new(&paths.token_path).load()?.is_none() {
        ensure!(interactive, "OneDrive 登录已失效，请手动重新授权");
        twodrive_backend::GraphBackend::login_cancellable_quiet(&paths, cancel)?;
    }
    let backend = OneDrive::open(&directory(p), p.writable)?;
    let binding = binding(&backend, cancel)?;
    // A display-only probe must not make an otherwise usable drive unavailable.
    let account_name = backend
        .graph
        .readonly_account_name(&mut || {
            ensure!(!cancel.load(Ordering::Relaxed), "连接已取消");
            Ok(())
        })
        .unwrap_or_default();
    ensure!(!cancel.load(Ordering::Relaxed), "连接已取消");
    network.record("network: OneDrive route=system");
    Ok(Connection {
        account_name,
        backend: Backend::new(backend),
        binding: binding.clone(),
        worker: json!({"kind":"onedrive","revision":p.revision,"binding":binding}),
        gateway: None,
    })
}
pub fn restore(p: &Profile, recipe: &Value) -> anyhow::Result<Backend> {
    let backend = OneDrive::open(&directory(p), p.writable)?;
    ensure!(
        recipe["binding"]
            .as_str()
            .context("missing OneDrive binding")?
            == binding(&backend, &AtomicBool::new(false))?,
        "OneDrive 登录账号已改变，原缓存保留；请重新连接"
    );
    Ok(Backend::new(backend))
}
pub fn sign_out(p: &Profile) -> anyhow::Result<()> {
    OneDrive::logout(&directory(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_with_missing_token_never_enters_browser_authorization() {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("private");
        let p: Profile = serde_json::from_value(
            json!({"kind":"onedrive","onedrive_state":private,"network_route":"system"}),
        )
        .unwrap();
        let error = resume(&p, &AtomicBool::new(false), Arc::default())
            .err()
            .unwrap();
        assert!(error.to_string().contains("请手动重新授权"));
        assert!(!paths(&private).unwrap().token_path.exists());
    }
}
