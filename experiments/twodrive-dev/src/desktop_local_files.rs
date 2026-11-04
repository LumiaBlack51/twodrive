//! Provider-neutral local-file IPC. Never registers roots, copies or hydrates.
use anyhow::{Context, ensure};
use serde_json::{Value, json};
use std::path::Path;
#[cfg(not(target_os = "linux"))]
use twodrive_local_files::migration;
use twodrive_local_files::migration::SourceKind;

#[cfg(target_os = "linux")]
#[path = "desktop_local_linux.rs"]
mod linux;

pub fn capabilities() -> Value {
    json!({"schema_version":1,"metadata_preflight":cfg!(any(windows,target_os="linux")),
        "native_mount":false,"import":false,"cloud_writes":false})
}

#[derive(Default)]
pub struct NativeFixture {
    #[cfg(all(windows, feature = "native-local-fixture"))]
    service: Option<twodrive_dev::native_local_fixture::FixtureService>,
}
impl NativeFixture {
    pub fn start(root: &Path) -> anyhow::Result<Self> {
        #[cfg(all(windows, feature = "native-local-fixture"))]
        {
            Ok(Self {
                service: Some(
                    twodrive_dev::native_local_fixture::FixtureService::start_visual(
                        root.to_owned(),
                    )?,
                ),
            })
        }
        #[cfg(not(all(windows, feature = "native-local-fixture")))]
        {
            let _ = root;
            anyhow::bail!("native generated fixture is not compiled in this engine")
        }
    }
    pub fn capabilities(&self) -> Value {
        let mut caps = capabilities();
        #[cfg(all(windows, feature = "native-local-fixture"))]
        let active = self
            .service
            .as_ref()
            .is_some_and(|service| service.active());
        #[cfg(not(all(windows, feature = "native-local-fixture")))]
        let active = false;
        #[cfg(all(windows, feature = "native-local-fixture"))]
        let unavailable = self
            .service
            .as_ref()
            .is_some_and(|service| service.unavailable());
        #[cfg(not(all(windows, feature = "native-local-fixture")))]
        let unavailable = false;
        caps["item_status"] = json!({"schema_version":1,"supported":active,"scope":"generated_fixture_only",
            "read":active,"pin":active,"offline_test":active,"reconnect":active,"shell_registration":active,
            "native_copy_dehydrate":active,"owner_unavailable":unavailable,"cache_evict":false,"write":false,"rename":false,"delete":false,"conflict_resolution":false,"persistent_root_resume":false});
        caps
    }
    pub fn request(&self, op: &str, args: &Value) -> anyhow::Result<Value> {
        ensure!(
            args["schema_version"] == 1,
            "unsupported native item schema"
        );
        let command = match op {
            "local_items" => "items",
            "local_read" => "read",
            "local_dehydrate" => "dehydrate",
            "local_pin" => "pin",
            "local_unpin" => "unpin",
            "local_offline" => "offline",
            "local_reconnect" => "reconnect",
            "local_stop" => "stop",
            _ => anyhow::bail!("unsupported native operation"),
        };
        let id = if matches!(command, "read" | "dehydrate" | "pin" | "unpin") {
            args["id"].as_str().context("missing item identity")?
        } else {
            ""
        };
        let value = if command == "offline" {
            args["offline"]
                .as_bool()
                .context("missing offline test state")?
        } else {
            false
        };
        #[cfg(all(windows, feature = "native-local-fixture"))]
        {
            self.service
                .as_ref()
                .context("no native generated fixture is active")?
                .request(command, id, value)
        }
        #[cfg(not(all(windows, feature = "native-local-fixture")))]
        {
            let _ = (command, id, value);
            anyhow::bail!("native fixture unavailable")
        }
    }
}

pub fn preflight(args: &Value) -> anyhow::Result<Value> {
    ensure!(
        cfg!(any(windows, target_os = "linux")),
        "local metadata preflight unavailable on this platform"
    );
    ensure!(
        args["schema_version"] == 1,
        "unsupported local-file schema version"
    );
    let kind: SourceKind =
        serde_json::from_value(args["source_kind"].clone()).context("invalid source provenance")?;
    let path = args["source_path"]
        .as_str()
        .context("missing source path")?;
    ensure!(
        !path.contains('\0') && path.len() <= 32767 && Path::new(path).is_absolute(),
        "source path must be absolute and bounded"
    );
    #[cfg(not(target_os = "linux"))]
    let inspected = migration::preflight(Path::new(path), kind);
    #[cfg(target_os = "linux")]
    let inspected = linux::inspect(Path::new(path), kind);
    let decision = inspected.context("local metadata could not be inspected")?;
    // No path, filenames, content, raw attributes or account identity echoed.
    Ok(
        json!({"schema_version":1,"decision":decision,"approval_required":true,
        "content_read":false,"hydration_requested":false,"import_performed":false,
        "cloud_upload_completion":"unverified"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_local_metadata_preflight_returns_only_decision_without_reading_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private-generated-name.txt");
        std::fs::write(&path, b"generated preflight content").unwrap();
        let report = preflight(
            &json!({"schema_version":1,"source_path":path,"source_kind":"ordinary_directory"}),
        )
        .unwrap();
        assert_eq!(report["decision"], "full_file_needs_approved_plan");
        assert_eq!(report["content_read"], false);
        assert_eq!(report["hydration_requested"], false);
        assert_eq!(report["import_performed"], false);
        assert!(capabilities()["metadata_preflight"].as_bool().unwrap());
        assert!(!report.to_string().contains("private-generated-name"));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"generated preflight content"
        );
    }
    #[test]
    fn native_mount_is_not_implied_by_preflight() {
        assert_eq!(capabilities()["native_mount"], false);
        assert_eq!(capabilities()["import"], false);
        assert_eq!(
            capabilities()["metadata_preflight"],
            cfg!(any(windows, target_os = "linux"))
        );
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    #[test]
    fn non_windows_preflight_is_explicitly_unsupported() {
        assert!(preflight(&json!({"schema_version":1})).is_err());
    }
    #[cfg(windows)]
    #[test]
    fn generated_file_has_redacted_metadata_only_ipc_result() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("generated-local-file.txt");
        std::fs::write(&path, b"generated IPC fixture").unwrap();
        let report = preflight(
            &json!({"schema_version":1,"source_path":path,"source_kind":"ordinary_directory"}),
        )
        .unwrap();
        assert_eq!(report["decision"], "full_file_needs_approved_plan");
        assert_eq!(report["content_read"], false);
        assert!(!report.to_string().contains("generated-local-file"));
        assert_eq!(std::fs::read(path).unwrap(), b"generated IPC fixture");
        assert!(preflight(&json!({"schema_version":2})).is_err());
    }
    #[cfg(windows)]
    #[test]
    fn cache_and_unknown_are_refused_without_even_opening_the_path() {
        let path = std::env::temp_dir().join("nonexistent-generated-file");
        for (kind, decision) in [
            ("private_cache", "unsupported_cache"),
            ("virtual_mount", "unsupported_virtual_mount"),
            ("unknown", "unknown_provenance"),
        ] {
            assert_eq!(
                preflight(&json!({"schema_version":1,"source_path":path,"source_kind":kind}))
                    .unwrap()["decision"],
                decision
            );
        }
    }
}
