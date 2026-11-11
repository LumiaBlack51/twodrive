#[cfg(all(windows, feature = "native-sync"))]
mod enabled {
    // Explicit account-bound CFAPI root management.
    use anyhow::{Context, ensure};
    use serde_json::{Value, json};
    use std::{
        process::{Child, Command, Stdio},
        sync::atomic::{AtomicBool, Ordering},
        time::{Duration, Instant},
    };
    use twodrive_dev::{
        native_coordinator::{CreateNativeSession, NativeSession},
        profile::Profile,
        state::State,
    };
    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    struct Root {
        profile: String,
        revision: String,
        root: std::path::PathBuf,
        state: std::path::PathBuf,
        #[serde(default)]
        auto_start: bool,
    }
    #[derive(Default)]
    pub struct Runtime {
        child: Option<Child>,
    }
    impl Runtime {
        fn missing(path: &std::path::Path) -> anyhow::Result<bool> {
            match std::fs::symlink_metadata(path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
                Err(e) => Err(e.into()),
                Ok(_) => Ok(false),
            }
        }
        fn unowned_attempt(root: &Root) -> anyhow::Result<bool> {
            Ok(Self::missing(&root.state.join("native-owner.dat"))?
                && Self::missing(&root.state.join("root-receipt.json"))?
                && Self::missing(&root.state.join("desktop-ready.json"))?
                && Self::missing(&root.root)?
                && !Self::event_running(root))
        }
        fn prepare_attempt<T>(
            state: &State,
            root: &Root,
            operation: impl FnOnce() -> anyhow::Result<T>,
        ) -> anyhow::Result<T> {
            state.save("native-desktop.json", root)?;
            operation()
        }
        pub fn retain_profile_binding(
            &self,
            state: &State,
            previous: Option<&Profile>,
            proposed: &mut Profile,
        ) -> anyhow::Result<()> {
            let Some(root) = Self::configuration(state)? else {
                return Ok(());
            };
            if root.profile != proposed.id {
                return Ok(());
            }
            let previous = previous.context("原生目录所属连接缺失，保留原配置以便恢复")?;
            ensure!(
                previous.revision == root.revision,
                "原生目录配置修订不匹配，保留原配置以便恢复"
            );
            let mut old = previous.clone();
            old.writable = super::super::providers::editable(&old);
            let mut comparable = proposed.clone();
            // OneDrive's legacy "auto" is the same system route; normalize
            // this alias without changing account paths or opaque options.
            if old.kind == "onedrive" && old.network_route == "auto" {
                old.network_route = "system".into();
            }
            if comparable.kind == "onedrive" && comparable.network_route == "auto" {
                comparable.network_route = "system".into();
            }
            comparable.name = old.name.clone();
            comparable.revision = old.revision.clone();
            ensure!(
                serde_json::to_value(&old)? == serde_json::to_value(&comparable)?,
                "此连接已绑定原生目录；目前仅支持修改显示名称，账户、路径和网络配置须保持不变"
            );
            proposed.revision = previous.revision.clone();
            Ok(())
        }
        fn configuration(state: &State) -> anyhow::Result<Option<Root>> {
            if state.dir.join("native-desktop.json").exists() {
                Ok(Some(state.read("native-desktop.json")?))
            } else {
                Ok(None)
            }
        }
        fn event_running(root: &Root) -> bool {
            use sha2::{Digest, Sha256};
            use std::os::windows::ffi::OsStrExt;
            let Ok(path) = root.state.canonicalize() else {
                return false;
            };
            let name = format!(
                "Local\\TwoDriveDevNativeStop-{:x}",
                Sha256::digest(path.to_string_lossy().as_bytes())
            );
            let wide: Vec<u16> = std::ffi::OsStr::new(&name)
                .encode_wide()
                .chain(Some(0))
                .collect();
            unsafe {
                let event =
                    windows_sys::Win32::System::Threading::OpenEventW(0x00100000, 0, wide.as_ptr());
                if event.is_null() {
                    return false;
                }
                windows_sys::Win32::Foundation::CloseHandle(event);
            }
            true
        }
        pub fn status(&mut self, state: &State) -> anyhow::Result<Value> {
            let running = self
                .child
                .as_mut()
                .is_some_and(|p| p.try_wait().ok().flatten().is_none());
            let root = Self::configuration(state)?;
            let running = running || root.as_ref().is_some_and(Self::event_running);
            let configured = root
                .as_ref()
                .is_some_and(|r| r.state.join("desktop-ready.json").is_file());
            let progress = root.as_ref().and_then(|r| {
                State::new(r.state.clone())
                    .ok()?
                    .read::<Value>("native-status.json")
                    .ok()
            });
            let exclusions = root.as_ref().and_then(|r| {
                State::new(r.state.clone())
                    .ok()?
                    .read::<Value>("native-exclusions.json")
                    .ok()
            });
            let retry_prepare = root
                .as_ref()
                .is_some_and(|r| Self::unowned_attempt(r).unwrap_or(false));
            Ok(
                json!({"supported":true,"provider":"onedrive","configured":configured,"preparing":root.is_some() && !configured,"retry_prepare":retry_prepare,"running":running,"profile":root.as_ref().map(|r|&r.profile),"root":root.as_ref().map(|r|&r.root),"progress":progress,"exclusions":exclusions}),
            )
        }
        fn binary() -> anyhow::Result<std::path::PathBuf> {
            let path = std::env::current_exe()?
                .parent()
                .context("runtime directory missing")?
                .join("twodrive-native-dev.exe");
            ensure!(path.is_file(), "CFAPI 运行组件缺失");
            Ok(path)
        }
        pub fn request(
            &mut self,
            op: &str,
            args: &Value,
            profile: &Profile,
            state: &State,
            cancel: &AtomicBool,
        ) -> anyhow::Result<Value> {
            ensure!(
                profile.kind == "onedrive",
                "当前 CFAPI 原生同步支持 OneDrive"
            );
            if op == "native_stop" {
                if let Some(mut root) = Self::configuration(state)? {
                    ensure!(
                        root.profile == profile.id && root.revision == profile.revision,
                        "CFAPI directory belongs to another profile"
                    );
                    self.stop(state)?;
                    root.auto_start = false;
                    state.save("native-desktop.json", &root)?;
                }
                return self.status(state);
            }
            if op == "native_prepare" {
                let setup = State::new(state.dir.join("native-setup-owner"))?;
                let _setup_owner = setup.run_lock()?;
                if let Some(previous) = Self::configuration(state)? {
                    ensure!(
                        previous.profile == profile.id && previous.revision == profile.revision,
                        "CFAPI 目录属于其他连接或配置"
                    );
                    ensure!(
                        Self::unowned_attempt(&previous)?,
                        "CFAPI 目录已配置或已有恢复材料，请继续启动现有目录"
                    );
                }
                let root = std::path::PathBuf::from(
                    args["root"].as_str().context("请选择新的 CFAPI 目录")?,
                );
                ensure!(
                    root.is_absolute()
                        && !root.exists()
                        && root.parent().is_some_and(|p| p.is_dir()),
                    "CFAPI 目录必须是现有本机目录下尚不存在的子目录"
                );
                let parent = root.parent().unwrap().canonicalize()?;
                twodrive_local_files::native::verify_unregistered(&parent).map_err(|error| {
                    anyhow::anyhow!("CFAPI 目录预检失败，请选择其他本机目录：{error}")
                })?;
                let root = parent.join(root.file_name().context("CFAPI root name missing")?);
                let private = state.dir.canonicalize()?;
                let account =
                    std::path::PathBuf::from(profile.option("onedrive_state")).canonicalize()?;
                ensure!(
                    !root.starts_with(&private)
                        && !private.starts_with(&root)
                        && !root.starts_with(&account)
                        && !account.starts_with(&root),
                    "CFAPI 目录不能覆盖私有状态"
                );
                ensure!(
                    root != profile.mount_path,
                    "请选择独立 CFAPI 目录，保留现有挂载以便回退"
                );
                let provider = twodrive_dev::onedrive::OneDrive::open(&account, false)?;
                let mut check = || {
                    ensure!(!cancel.load(Ordering::Relaxed), "CFAPI 准备已取消");
                    Ok(())
                };
                let identity = provider.graph.readonly_identity(&mut check)?;
                let catalog_state =
                    State::new(state.dir.join(format!("native-catalogue-{}", profile.id)))?;
                let catalogue = catalog_state.dir.join("catalog.sqlite3");
                let db = twodrive_core::Database::new(&catalogue);
                db.init()?;
                db.init_cloud()?;
                let (cursor, replace, _) =
                    provider.graph.stage_delta(&db, &identity, &mut check)?;
                ensure!(
                    provider.graph.readonly_identity(&mut check)? == identity,
                    "OneDrive 账号已改变"
                );
                db.cloud_commit(&identity, &cursor, replace)?;
                // Never erase or reuse a failed attempt's private state.
                let native_state = state.dir.join(format!(
                    "native-cfapi-{}-{:032x}",
                    profile.id,
                    rand::random::<u128>()
                ));
                // Retain the desktop-to-sealed-state link before registration so
                // a lost IPC reply can resume the exact owner rather than adopt a root.
                let mut session = Self::prepare_attempt(
                    state,
                    &Root {
                        profile: profile.id.clone(),
                        revision: profile.revision.clone(),
                        root: root.clone(),
                        state: native_state.clone(),
                        auto_start: false,
                    },
                    || {
                        NativeSession::create_ordinary(CreateNativeSession {
                            account,
                            expected_owner: identity.account.clone(),
                            catalogue,
                            state: native_state.clone(),
                            root: root.clone(),
                        })
                    },
                )?;
                ensure!(session.owner_matches(&identity), "CFAPI 账号身份不匹配");
                session.coordinator.verify_bootstrap_inventory()?;
                session.coordinator.disconnect()?;
                State::new(native_state.clone())?.save("desktop-ready.json", &true)?;
                state.save(
                    "native-desktop.json",
                    &Root {
                        profile: profile.id.clone(),
                        revision: profile.revision.clone(),
                        root,
                        state: native_state,
                        auto_start: false,
                    },
                )?;
                return self.status(state);
            }
            ensure!(op == "native_start", "unknown native command");
            ensure!(self.status(state)?["running"] != true, "CFAPI 已在运行");
            let root = Self::configuration(state)?.context("请先准备 CFAPI 目录")?;
            ensure!(
                root.profile == profile.id && root.revision == profile.revision,
                "CFAPI 目录属于其他连接或配置"
            );
            let provider = twodrive_dev::onedrive::OneDrive::open(
                std::path::Path::new(profile.option("onedrive_state")),
                false,
            )?;
            let identity = provider.graph.readonly_identity(&mut || Ok(()))?;
            let mut session = NativeSession::resume(&root.state)?;
            ensure!(session.owner_matches(&identity), "CFAPI 登录账号已改变");
            let runtime_state = State::new(root.state.clone())?;
            let previously_ready =
                match std::fs::symlink_metadata(root.state.join("desktop-ready.json")) {
                    Ok(marker) => {
                        ensure!(
                            marker.is_file() && !marker.file_type().is_symlink(),
                            "invalid native readiness marker"
                        );
                        ensure!(
                            runtime_state.read::<bool>("desktop-ready.json")?,
                            "invalid native readiness marker"
                        );
                        true
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                    Err(error) => return Err(error.into()),
                };
            session.coordinator.verify_desktop_start(previously_ready)?;
            session.coordinator.disconnect()?;
            runtime_state.save("desktop-ready.json", &true)?;
            drop(session);
            let log = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(root.state.join("desktop-runtime.log"))?;
            let mut command = Command::new(Self::binary()?);
            command
                .args(["run", "--native-state"])
                .arg(&root.state)
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log);
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
            self.child = Some(command.spawn()?);
            let mut root = root;
            root.auto_start = true;
            state.save("native-desktop.json", &root)?;
            self.status(state)
        }
        pub fn resume_desired(
            &mut self,
            profiles: &[Profile],
            state: &State,
        ) -> anyhow::Result<()> {
            if let Some(root) = Self::configuration(state)?
                && root.auto_start
                && !Self::event_running(&root)
            {
                let profile = profiles
                    .iter()
                    .find(|p| p.id == root.profile && p.revision == root.revision)
                    .context("CFAPI profile changed")?;
                self.request(
                    "native_start",
                    &json!({}),
                    profile,
                    state,
                    &AtomicBool::new(false),
                )?;
            }
            Ok(())
        }
        pub fn stop(&mut self, state: &State) -> anyhow::Result<()> {
            let Some(root) = Self::configuration(state)? else {
                return Ok(());
            };
            let started = Instant::now();
            while !Self::event_running(&root) {
                let live = self
                    .child
                    .as_mut()
                    .is_some_and(|p| p.try_wait().ok().flatten().is_none());
                if !live {
                    self.child = None;
                    return Ok(());
                }
                ensure!(
                    started.elapsed() < Duration::from_secs(30),
                    "CFAPI is still starting; retry stop shortly"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
            use std::os::windows::process::CommandExt;
            let output = Command::new(Self::binary()?)
                .args(["stop", "--native-state"])
                .arg(&root.state)
                .creation_flags(0x08000000)
                .output()?;
            ensure!(
                output.status.success(),
                "CFAPI has not accepted stop; retry shortly"
            );
            let started = Instant::now();
            while Self::event_running(&root) {
                ensure!(
                    started.elapsed() < Duration::from_secs(45),
                    "CFAPI is stopping safely; retry shortly"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
            if let Some(child) = self.child.as_mut() {
                let _ = child.try_wait()?;
            }
            self.child = None;
            Ok(())
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        fn attempt_fixture() -> (tempfile::TempDir, State, Profile, Root) {
            let temp = tempfile::tempdir().unwrap();
            let state = State::new(temp.path().join("desktop")).unwrap();
            let mut profile = Profile {
                id: "one".into(),
                revision: "original-revision".into(),
                name: "OneDrive".into(),
                kind: "onedrive".into(),
                mount_path: temp.path().join("mount"),
                ..Profile::default()
            };
            profile.options.insert(
                "onedrive_state".into(),
                json!(temp.path().join("missing-account")),
            );
            let root = Root {
                profile: profile.id.clone(),
                revision: profile.revision.clone(),
                root: temp.path().join("native-root"),
                state: state.dir.join("native-attempt"),
                auto_start: false,
            };
            (temp, state, profile, root)
        }
        #[test]
        fn failed_preownership_attempt_is_retryable_without_erasing_diagnostics() {
            let (_temp, state, profile, root) = attempt_fixture();
            let failure: anyhow::Result<()> = Runtime::prepare_attempt(&state, &root, || {
                std::fs::create_dir(&root.state)?;
                std::fs::write(
                    root.state.join("diagnostic.txt"),
                    b"generated partial metadata",
                )?;
                anyhow::bail!("injected failure before owner seal")
            });
            assert!(failure.is_err());
            let mut runtime = Runtime::default();
            assert_eq!(runtime.status(&state).unwrap()["retry_prepare"], true);
            let error = runtime
                .request(
                    "native_prepare",
                    &json!({"root":root.root}),
                    &profile,
                    &state,
                    &AtomicBool::new(false),
                )
                .unwrap_err();
            // It must reach location/account validation rather than be stranded
            // at the old unconditional "already configured" guard.
            assert!(!error.to_string().contains("已配置"), "{error}");
            assert_eq!(
                std::fs::read(root.state.join("diagnostic.txt")).unwrap(),
                b"generated partial metadata"
            );
            assert!(!root.root.exists());
        }
        #[test]
        fn sealed_partial_or_existing_roots_cannot_be_reset_as_unowned() {
            for marker in [
                "native-owner.dat",
                "root-receipt.json",
                "desktop-ready.json",
            ] {
                let (_temp, state, profile, root) = attempt_fixture();
                std::fs::create_dir(&root.state).unwrap();
                std::fs::write(root.state.join(marker), b"generated ownership evidence").unwrap();
                state.save("native-desktop.json", &root).unwrap();
                let mut runtime = Runtime::default();
                assert_eq!(runtime.status(&state).unwrap()["retry_prepare"], false);
                assert!(
                    runtime
                        .request(
                            "native_prepare",
                            &json!({"root":root.root}),
                            &profile,
                            &state,
                            &AtomicBool::new(false)
                        )
                        .unwrap_err()
                        .to_string()
                        .contains("已有恢复材料")
                );
                assert_eq!(
                    std::fs::read(root.state.join(marker)).unwrap(),
                    b"generated ownership evidence"
                );
            }
            let (_temp, _state, _profile, root) = attempt_fixture();
            std::fs::create_dir(&root.root).unwrap();
            assert!(!Runtime::unowned_attempt(&root).unwrap());
        }
        #[test]
        fn stopped_root_name_edit_keeps_revision_and_rejects_binding_changes() {
            let (_temp, state, profile, root) = attempt_fixture();
            state.save("native-desktop.json", &root).unwrap();
            let runtime = Runtime::default();
            let mut renamed = profile.clone();
            renamed.name = "Renamed OneDrive".into();
            renamed.revision = "new-revision-must-not-escape".into();
            runtime
                .retain_profile_binding(&state, Some(&profile), &mut renamed)
                .unwrap();
            assert_eq!(renamed.revision, root.revision);
            assert_eq!(renamed.name, "Renamed OneDrive");
            // This is the exact identity guard used on the subsequent start.
            let saved = Runtime::configuration(&state).unwrap().unwrap();
            assert!(saved.profile == renamed.id && saved.revision == renamed.revision);
            for field in ["kind", "network", "mount", "account"] {
                let mut changed = renamed.clone();
                match field {
                    "kind" => changed.kind = "google".into(),
                    "network" => changed.network_route = "direct".into(),
                    "mount" => changed.mount_path = changed.mount_path.join("other"),
                    _ => {
                        changed.options.insert(
                            "onedrive_state".into(),
                            json!(state.dir.join("other-account")),
                        );
                    }
                }
                assert!(
                    runtime
                        .retain_profile_binding(&state, Some(&profile), &mut changed)
                        .is_err()
                );
            }
            assert_eq!(
                Runtime::configuration(&state).unwrap().unwrap().revision,
                root.revision
            );
        }
        #[test]
        #[ignore = "explicit read-only probe of an unsupported CFAPI parent; no root registration"]
        fn unsupported_cfapi_parent_is_rejected_before_account_access() {
            let parent = std::path::PathBuf::from(
                std::env::var_os("TWODRIVE_TEST_NATIVE_UNSUPPORTED_PARENT")
                    .expect("explicit unsupported parent required"),
            );
            let root = parent.join(format!(
                "twodrive-preflight-{:032x}",
                rand::random::<u128>()
            ));
            let private = tempfile::tempdir().unwrap();
            let state = State::new(private.path().to_path_buf()).unwrap();
            let profile = Profile {
                kind: "onedrive".into(),
                ..Profile::default()
            };
            let error = Runtime::default()
                .request(
                    "native_prepare",
                    &json!({"root":root}),
                    &profile,
                    &state,
                    &AtomicBool::new(false),
                )
                .unwrap_err();
            assert!(
                error.to_string().starts_with("CFAPI 目录预检失败"),
                "{error}"
            );
            assert!(!root.exists());
            assert!(!state.dir.join("native-desktop.json").exists());
            assert!(!state.dir.join("native-catalogue-").exists());
        }
    }
}
#[cfg(all(windows, feature = "native-sync"))]
pub use enabled::Runtime;
#[cfg(not(all(windows, feature = "native-sync")))]
#[derive(Default)]
pub struct Runtime {
    _private: (),
}
#[cfg(not(all(windows, feature = "native-sync")))]
impl Runtime {
    pub fn retain_profile_binding(
        &self,
        _: &super::State,
        _: Option<&super::Profile>,
        _: &mut super::Profile,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    pub fn status(&mut self, _: &super::State) -> anyhow::Result<serde_json::Value> {
        Ok(serde_json::json!({"supported":false}))
    }
    pub fn request(
        &mut self,
        _: &str,
        _: &serde_json::Value,
        _: &super::Profile,
        _: &super::State,
        _: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<serde_json::Value> {
        anyhow::bail!("CFAPI native-sync component unavailable")
    }
    pub fn stop(&mut self, _: &super::State) -> anyhow::Result<()> {
        Ok(())
    }
    pub fn resume_desired(&mut self, _: &[super::Profile], _: &super::State) -> anyhow::Result<()> {
        Ok(())
    }
}
