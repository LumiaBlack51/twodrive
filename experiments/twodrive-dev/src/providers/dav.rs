use super::Connection;
use anyhow::ensure;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use twodrive_dev::{
    profile::Profile,
    provider::Backend,
    state::Credentials,
    webdav::{DavError, WebDavBackend},
};

pub fn descriptor() -> Value {
    json!({"id":"dav","label":"WebDAV / NAS","account_label":"账号","icon":"cloud",
    "account_key":"username","summary_key":"url","identity_fields":["url","username"],"protected_paths":[],"capabilities":{"mount":true,"upload":true,"conditional_upload":true,"mkdir":true,"rename":true,"delete":true,"backup":true},"fields":[{"key":"url","label":"WebDAV 根地址 · https://…","required":true},
        {"key":"username","label":"用户名"},{"key":"password","label":"密码","secret":true},{"key":"allow_private_http","label":"允许局域网 HTTP（连接不加密，须使用私有 IP）","type":"boolean","advanced":true}]})
}
fn credentials(p: &Profile) -> Option<Credentials> {
    (!p.option("username").is_empty()).then(|| Credentials {
        username: p.option("username").into(),
        password: p.option("password").into(),
    })
}
pub fn validate(p: &Profile) -> anyhow::Result<()> {
    WebDavBackend::new_with_options(
        p.option("url"),
        credentials(p),
        p.writable,
        p.options
            .get("allow_private_http")
            .is_some_and(|v| v == true),
    )?;
    Ok(())
}
pub fn restore(p: &Profile, recipe: &Value) -> anyhow::Result<Backend> {
    let route = recipe["route"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing provider route"))?;
    Ok(Backend::new(WebDavBackend::routed_with_options(
        p.option("url"),
        credentials(p),
        p.writable,
        route,
        p.options
            .get("allow_private_http")
            .is_some_and(|v| v == true),
    )?))
}
pub fn connect(
    p: &Profile,
    cancel: &AtomicBool,
    network: Arc<twodrive_dev::network::Diagnostics>,
) -> anyhow::Result<Connection> {
    ensure!(!cancel.load(Ordering::Relaxed), "连接已取消");
    let initial = if p.network_route == "system" {
        "system"
    } else {
        "direct"
    };
    let probe = |route| -> anyhow::Result<Backend> {
        let backend = Backend::new(WebDavBackend::routed_with_options(
            p.option("url"),
            credentials(p),
            p.writable,
            route,
            p.options
                .get("allow_private_http")
                .is_some_and(|v| v == true),
        )?);
        backend.list_directory("/")?;
        Ok(backend)
    };
    let (backend, route) = match probe(initial) {
        Ok(b) => (b, initial),
        Err(e) if p.network_route == "auto" && e.downcast_ref::<DavError>().is_none() => {
            ensure!(!cancel.load(Ordering::Relaxed), "连接已取消");
            let b = probe("system")?;
            network.record("network: route=system/proxy fallback; physical probe failed");
            (b, "system")
        }
        Err(e) => return Err(e),
    };
    ensure!(!cancel.load(Ordering::Relaxed), "连接已取消");
    network.record(&format!("network: WebDAV route={route}"));
    let account_name = if p.option("username").is_empty() {
        "匿名访问"
    } else {
        p.option("username")
    }
    .to_owned();
    Ok(Connection {
        account_name,
        backend,
        binding: format!("dav:{}:{}", p.option("url"), p.option("username")),
        worker: json!({"kind":p.kind,"revision":p.revision,"route":route}),
        gateway: None,
    })
}
