//! Composition boundary: only registered adapters know protocols, authentication,
//! connection startup, worker recipes and persistent account identity.
use super::{Args, Process};
use anyhow::{Context, ensure};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};
use twodrive_dev::{profile::Profile, provider::Backend};

#[path = "providers/dav.rs"]
mod dav;
#[path = "providers/google.rs"]
mod google;
#[path = "providers/onedrive.rs"]
mod onedrive;
#[path = "providers/paired.rs"]
mod paired;
#[path = "providers/smb.rs"]
mod smb;

pub struct Connection {
    pub backend: Backend,
    pub binding: String,
    pub account_name: String,
    pub worker: Value,
    pub gateway: Option<Process>,
}

pub fn catalog() -> Value {
    json!([
        dav::descriptor(),
        paired::descriptor(),
        google::descriptor(),
        onedrive::descriptor(),
        smb::descriptor()
    ])
}
fn descriptor(kind: &str) -> anyhow::Result<Value> {
    match kind {
        "dav" => Ok(dav::descriptor()),
        "peer" => Ok(paired::descriptor()),
        "google" => Ok(google::descriptor()),
        "onedrive" => Ok(onedrive::descriptor()),
        "smb" => Ok(smb::descriptor()),
        _ => anyhow::bail!("unsupported provider: {kind}"),
    }
}
pub fn prepare(p: &mut Profile, state: &twodrive_dev::state::State) -> anyhow::Result<()> {
    descriptor(&p.kind)?;
    match p.kind.as_str() {
        "onedrive" => onedrive::prepare(p, state),
        "smb" => smb::prepare(p, state),
        "peer" => paired::prepare(p, state),
        "google" => google::prepare(p, state),
        _ => Ok(()),
    }
}
pub fn action(
    kind: &str,
    action: &str,
    args: &Value,
    cancel: &AtomicBool,
) -> anyhow::Result<Value> {
    descriptor(kind)?;
    match kind {
        "peer" => paired::action(action, args, cancel),
        _ => anyhow::bail!("此提供者不支持该操作"),
    }
}
pub fn validate(p: &Profile) -> anyhow::Result<()> {
    descriptor(&p.kind)?;
    match p.kind.as_str() {
        "dav" => dav::validate(p),
        "peer" => paired::validate(p),
        "google" => google::validate(p),
        "onedrive" => onedrive::validate(p),
        "smb" => smb::validate(p),
        _ => unreachable!(),
    }
}
pub fn retain_secrets(p: &mut Profile, previous: Option<&Profile>, clear: bool) {
    if let Ok(d) = descriptor(&p.kind) {
        for field in d["fields"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["secret"] == true)
        {
            let key = field["key"].as_str().unwrap();
            if !clear
                && let Some(old) = previous
                && p.kind == old.kind
                && p.option(key).is_empty()
                && d["identity_fields"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|f| p.option(f.as_str().unwrap()) == old.option(f.as_str().unwrap()))
            {
                p.options.insert(key.into(), json!(old.option(key)));
            }
        }
    }
    if p.kind == "smb" {
        smb::retain(p, previous, clear);
    }
    // Provider schemas define the only persisted options, so account switches
    // cannot accidentally retain another provider's credentials or IPC metadata.
    if let Ok(d) = descriptor(&p.kind) {
        let fields: Vec<_> = d["fields"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|f| f["key"].as_str())
            .collect();
        p.options.retain(|key, _| fields.contains(&key.as_str()));
    } else {
        p.options.clear();
    }
}
pub fn public_profile(p: &Profile) -> Value {
    let mut value = json!({"id":p.id,"revision":p.revision,"name":p.name,"kind":p.kind,
        "writable":p.writable,"mount_path":p.mount_path,"network_route":p.network_route});
    if let Ok(d) = descriptor(&p.kind) {
        value["provider_label"] = d["label"].clone();
        value["account_label"] = d["account_label"].clone();
        value["icon"] = d["icon"].clone();
        let summary = p.option(d["summary_key"].as_str().unwrap_or_default());
        let account = p.option(d["account_key"].as_str().unwrap_or_default());
        value["summary"] = json!(if summary.is_empty() {
            d["label"].as_str().unwrap()
        } else {
            summary
        });
        value["account"] = json!(if account.is_empty() { &p.name } else { account });
        for field in d["fields"].as_array().unwrap() {
            let key = field["key"].as_str().unwrap();
            if field["secret"] == true {
                value[format!("{key}_saved")] = json!(!p.option(key).is_empty());
            } else if let Some(v) = p.options.get(key) {
                value[key] = v.clone();
            }
        }
        if p.kind == "smb" {
            value["password_saved"] = json!(Path::new(p.option("credential_state")).is_file());
        }
        let mut capabilities = d["capabilities"].clone();
        if !p.writable {
            for key in ["upload", "mkdir", "rename", "delete", "backup"] {
                capabilities[key] = json!(false);
            }
        }
        value["capabilities"] = capabilities;
    } else {
        value["provider_label"] = json!(&p.kind);
        value["summary"] = json!("此版本不支持此存储提供者");
        value["capabilities"] = json!({"backup":false});
    }
    value
}
pub fn connect(
    p: &Profile,
    args: &Args,
    cancel: &AtomicBool,
    network: Arc<twodrive_dev::network::Diagnostics>,
) -> anyhow::Result<Connection> {
    validate(p)?;
    match p.kind.as_str() {
        "dav" => dav::connect(p, cancel, network),
        "peer" => paired::connect(p, args, cancel, network),
        "google" => google::connect(p, cancel, network),
        "onedrive" => onedrive::connect(p, cancel, network),
        "smb" => smb::connect(p, cancel, network),
        _ => unreachable!(),
    }
}
pub fn restore(p: &Profile, recipe: &Value, paused: &Path) -> anyhow::Result<Backend> {
    ensure!(
        recipe["kind"].as_str() == Some(&p.kind)
            && recipe["revision"].as_str() == Some(&p.revision),
        "worker session belongs to another profile revision"
    );
    let backend = match p.kind.as_str() {
        "dav" => dav::restore(p, recipe)?,
        "peer" => paired::restore(p, recipe)?,
        "google" => google::restore(p, recipe)?,
        "onedrive" => onedrive::restore(p, recipe)?,
        "smb" => {
            anyhow::bail!("SMB requires explicit reconnect; persistent workers/mounts unavailable")
        }
        _ => anyhow::bail!("unsupported provider"),
    };
    Ok(backend.with_pause_file(paused.to_path_buf()))
}
pub fn session(connection: &Connection, p: &Profile) -> Value {
    json!({"kind":p.kind,"revision":p.revision,"binding":connection.binding,"account_name":connection.account_name,"recipe":connection.worker})
}
pub fn restore_session(
    p: &Profile,
    session: &Value,
    paused: &Path,
) -> anyhow::Result<(Backend, String)> {
    ensure!(
        session["kind"].as_str() == Some(&p.kind)
            && session["revision"].as_str() == Some(&p.revision),
        "worker session belongs to another profile revision"
    );
    let backend = restore(p, &session["recipe"], paused)?;
    Ok((
        backend,
        session["binding"]
            .as_str()
            .context("missing provider binding")?
            .into(),
    ))
}

pub fn protected_paths(p: &Profile) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let d = descriptor(&p.kind)?;
    d["protected_paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| {
            let path = std::path::PathBuf::from(p.option(key.as_str().unwrap()));
            path.canonicalize()
                .context("provider identity directory is unavailable")
        })
        .collect()
}

pub fn mount_startup_timeout(p: &Profile) -> std::time::Duration {
    let seconds = descriptor(&p.kind)
        .ok()
        .and_then(|d| d["mount_startup_timeout_seconds"].as_u64())
        .unwrap_or(60)
        .clamp(1, 600);
    std::time::Duration::from_secs(seconds)
}
pub fn sign_out(p: &Profile) -> anyhow::Result<()> {
    match p.kind.as_str() {
        "google" => google::sign_out(p)?,
        "onedrive" => onedrive::sign_out(p)?,
        "smb" => smb::sign_out(p)?,
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_profiles_roundtrip_and_redact_secrets() {
        let legacy = json!({"id":"p","kind":"dav","url":"https://example.invalid/dav/","username":"account","password":"disposable-secret","name":"Example","writable":true,"mount_path":"/tmp/mount","network_route":"auto"});
        let p: Profile = serde_json::from_value(legacy.clone()).unwrap();
        let saved = serde_json::to_value(&p).unwrap();
        for key in ["url", "username", "password", "kind", "mount_path"] {
            assert_eq!(saved[key], legacy[key]);
        }
        let public = public_profile(&p);
        assert!(public.get("password").is_none());
        assert_eq!(public["password_saved"], true);
        assert_eq!(public["provider_label"], "WebDAV / NAS");
        let mut edit = p.clone();
        edit.options.insert("password".into(), json!(""));
        retain_secrets(&mut edit, Some(&p), false);
        assert_eq!(edit.option("password"), "disposable-secret");
        edit.options
            .insert("username".into(), json!("different-account"));
        edit.options.insert("password".into(), json!(""));
        retain_secrets(&mut edit, Some(&p), false);
        assert!(edit.option("password").is_empty());
        edit.kind = "peer".into();
        edit.options.insert("peer_state".into(), json!("/tmp/pair"));
        retain_secrets(&mut edit, Some(&p), false);
        assert!(edit.option("password").is_empty());
        assert!(edit.option("url").is_empty());
        assert!(
            restore_session(
                &p,
                &json!({"kind":"dav","revision":"stale"}),
                Path::new("/tmp/pause")
            )
            .is_err()
        );
    }
    #[test]
    fn unknown_provider_never_exposes_opaque_options() {
        let p: Profile =
            serde_json::from_value(json!({"kind":"future","password":"secret","token":"secret"}))
                .unwrap();
        let public = public_profile(&p);
        assert!(public.get("password").is_none());
        assert!(public.get("token").is_none());
        assert!(validate(&p).is_err());
    }
}
