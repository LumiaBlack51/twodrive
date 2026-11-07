use super::Connection;
use anyhow::{Context, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};
use twodrive_dev::{
    profile::Profile,
    provider::Backend,
    smb::{Settings, SmbBackend},
    state::State,
};

const IDENTITY: &[&str] = &[
    "server",
    "port",
    "share",
    "root",
    "username",
    "domain",
    "encrypt",
    "schema_version",
];
pub fn descriptor() -> Value {
    json!({"id":"smb","label":"SMB2/3（只读）","account_label":"账号","icon":"storage",
    "account_key":"username","summary_key":"server","identity_fields":IDENTITY,
    "protected_paths":["credential_state"],"network_modes":["system"],
        "network_labels":{"system":"使用系统网络路由（不使用 HTTP 代理）"},
    "help":"手动连接共享；默认要求 SMB3 加密，始终要求签名。仅浏览和下载，不支持链接、DFS、挂载或自动发现。需安装独立 SMB Python 组件，见 SMB 指南。",
    "capabilities":{"upload":false,"conditional_upload":false,"mkdir":false,"rename":false,"delete":false,"backup":false,"mount":false},
    "fields":[
        {"key":"server","label":"服务器（主机名或 IP）","required":true},
        {"key":"port","label":"端口（留空为 445）"},
        {"key":"share","label":"共享名","required":true},
        {"key":"root","label":"共享内根目录（留空为 /）"},
        {"key":"username","label":"用户名","required":true},
        {"key":"domain","label":"域 / 工作组（可留空）"},
        {"key":"password","label":"密码","secret":true,"transient":true},
        {"key":"signed_only","label":"仅签名，允许 SMB2（不加密内容，显式选择）","type":"boolean","advanced":true},
        {"key":"encrypt","internal":true},
        {"key":"schema_version","internal":true},
        {"key":"credential_state","internal":true,"secret":true}
    ]})
}
fn normalize(p: &mut Profile) {
    for (key, value) in [("port", "445"), ("root", "/")] {
        if p.option(key).is_empty() {
            p.options.insert(key.into(), json!(value));
        }
    }
    let encrypt = p.options.get("signed_only") != Some(&json!(true));
    p.options.insert("encrypt".into(), json!(encrypt));
    p.options.insert("schema_version".into(), json!("1"));
    p.writable = false;
}
fn scope(p: &Profile) -> String {
    let identity: Vec<_> = IDENTITY.iter().map(|k| (k, p.options.get(*k))).collect();
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&json!([p.id, p.revision, identity])).unwrap())
    )
}
fn settings(p: &Profile, password: String) -> anyhow::Result<Settings> {
    ensure!(
        p.option("schema_version") == "1",
        "SMB configuration version unsupported"
    );
    let s = Settings {
        server: p.option("server").into(),
        port: p.option("port").parse().context("SMB port invalid")?,
        share: p.option("share").into(),
        root: p.option("root").into(),
        username: p.option("username").into(),
        domain: p.option("domain").into(),
        password,
        encrypt: p.options.get("encrypt") != Some(&json!(false)),
    };
    s.validate()?;
    Ok(s)
}
fn read_password(p: &Profile) -> anyhow::Result<String> {
    let path = Path::new(p.option("credential_state"));
    ensure!(
        path.is_absolute(),
        "SMB private credential unavailable; enter password"
    );
    let private = State::new(
        path.parent()
            .context("SMB credential parent unavailable")?
            .into(),
    )?;
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && !meta.is_symlink(),
        "SMB credential must be a private regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            meta.permissions().mode() & 0o077 == 0,
            "SMB credential must have mode 0600"
        );
    }
    let secret: Value = private.read(
        path.file_name()
            .and_then(|n| n.to_str())
            .context("SMB credential filename invalid")?,
    )?;
    ensure!(
        secret["scope"].as_str() == Some(scope(p).as_str()),
        "SMB credential belongs to another identity/revision"
    );
    Ok(secret["password"]
        .as_str()
        .context("SMB credential unavailable")?
        .into())
}
pub fn retain(p: &mut Profile, previous: Option<&Profile>, clear: bool) {
    normalize(p);
    // References supplied by IPC are never authoritative. Only a previously
    // saved, scope-checked credential can be retained for unchanged identity.
    p.options.remove("credential_state");
    if !clear
        && p.option("password").is_empty()
        && let Some(old) = previous.filter(|old| {
            old.kind == "smb"
                && old.id == p.id
                && IDENTITY
                    .iter()
                    .all(|key| p.options.get(*key) == old.options.get(*key))
        })
        && let Ok(password) = read_password(old)
    {
        p.options.insert("password".into(), json!(password));
    }
}
pub fn prepare(p: &mut Profile, state: &State) -> anyhow::Result<()> {
    normalize(p);
    ensure!(
        p.network_route == "system",
        "SMB uses OS routing; select system route"
    );
    // Validate before writing a new secret. Never persist the transient input in profiles.json.
    settings(p, p.option("password").into())?;
    ensure!(
        !p.id.is_empty()
            && p.id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
        "SMB profile id invalid"
    );
    let private = State::new(
        state
            .dir
            .join("accounts")
            .join(&p.id)
            .join("smb")
            .join(scope(p)),
    )?;
    private.save(
        "credential.json",
        &json!({"scope":scope(p),"password":p.option("password")}),
    )?;
    p.options.remove("password");
    p.options.insert(
        "credential_state".into(),
        json!(private.dir.join("credential.json")),
    );
    Ok(())
}
pub fn validate(p: &Profile) -> anyhow::Result<()> {
    ensure!(
        !p.writable && p.network_route == "system",
        "SMB is read-only with OS routing"
    );
    settings(p, read_password(p)?)?;
    Ok(())
}
pub fn connect(
    p: &Profile,
    cancel: &AtomicBool,
    _: Arc<twodrive_dev::network::Diagnostics>,
) -> anyhow::Result<Connection> {
    let backend = SmbBackend::new(settings(p, read_password(p)?)?)?;
    backend.probe(cancel)?;
    // New explicit connection => new namespace; never replay old observations/cache.
    let binding = format!("smb:{}:{}", scope(p), twodrive_dev::state::random_secret());
    Ok(Connection {
        backend: Backend::new(backend),
        binding,
        account_name: p.option("username").into(),
        worker: json!({"kind":"smb","revision":p.revision}),
        gateway: None,
    })
}
pub fn sign_out(p: &Profile) -> anyhow::Result<()> {
    read_password(p)?; // check scope and private path before removal
    std::fs::remove_file(p.option("credential_state"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::providers::{public_profile, retain_secrets};
    fn profile() -> Profile {
        serde_json::from_value(json!({"id":"smb-test","revision":"r1","kind":"smb",
            "name":"Synthetic","server":"127.0.0.1","share":"synthetic","username":"test",
            "network_route":"system","password":"synthetic-private-password","writable":true}))
        .unwrap()
    }
    #[test]
    fn credentials_are_private_scoped_redacted_and_revision_bound() {
        let tmp = tempfile::tempdir().unwrap();
        let state = State::new(tmp.path().join("state")).unwrap();
        let mut p = profile();
        retain(&mut p, None, false);
        prepare(&mut p, &state).unwrap();
        validate(&p).unwrap();
        assert!(!p.writable && p.option("password").is_empty());
        assert!(
            !serde_json::to_string(&p)
                .unwrap()
                .contains("synthetic-private-password")
        );
        let public = public_profile(&p);
        assert_eq!(public["password_saved"], true);
        assert!(public.get("credential_state").is_none());
        assert!(!public.to_string().contains("synthetic-private-password"));
        let mut edit = p.clone();
        edit.revision = "r2".into();
        assert!(validate(&edit).is_err());
        retain_secrets(&mut edit, Some(&p), false);
        prepare(&mut edit, &state).unwrap();
        assert_ne!(
            p.option("credential_state"),
            edit.option("credential_state")
        );
        validate(&edit).unwrap();
        for (key, value) in [
            ("server", "localhost"),
            ("port", "1445"),
            ("share", "other"),
            ("root", "/other"),
            ("username", "other"),
            ("domain", "other"),
            ("schema_version", "2"),
        ] {
            let mut changed = p.clone();
            changed.revision = "r3".into();
            // Existing saved configuration of another schema must never donate secrets.
            let mut old = p.clone();
            if key == "schema_version" {
                old.options.insert(key.into(), json!(value));
            } else {
                changed.options.insert(key.into(), json!(value));
            }
            retain_secrets(&mut changed, Some(&old), false);
            assert!(
                prepare(&mut changed, &state).is_err(),
                "inherited another identity credential"
            );
        }
        let mut changed = p.clone();
        changed.options.insert("signed_only".into(), json!(true));
        retain_secrets(&mut changed, Some(&p), false);
        assert!(prepare(&mut changed, &state).is_err());
        let mut changed = p.clone();
        changed.revision = "cleared".into();
        retain_secrets(&mut changed, Some(&p), true);
        assert!(prepare(&mut changed, &state).is_err());
        let b = json!({"kind":"smb","revision":p.revision,"recipe":{"kind":"smb","revision":p.revision}});
        assert!(crate::desktop::providers::restore_session(&p, &b, Path::new("/unused")).is_err());
        sign_out(&p).unwrap();
        assert!(validate(&p).is_err());
        validate(&edit).unwrap();
    }
}
