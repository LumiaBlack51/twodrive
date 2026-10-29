//! Retain independent account sessions; selecting a view never tears down another.
use super::*;

pub(super) struct Parked {
    profile: Profile,
    backend: Option<Backend>,
    gateway: Option<Process>,
    mounted: Option<Process>,
    backup: Option<Process>,
    network: Arc<Diagnostics>,
}

pub(super) struct FileContext {
    pub profile: Profile,
    pub backend: Backend,
    pub state: State,
}
pub(super) struct Job {
    profile: String,
    kind: String,
    cancel: Arc<AtomicBool>,
    handle: thread::JoinHandle<()>,
}
pub(super) type Jobs = HashMap<u64, Job>;

pub(super) fn aggregate_accounts(accounts: &[Value]) -> Value {
    let mut active = Vec::new();
    let mut waiting = Vec::new();
    let mut recent = Vec::new();
    let mut issues = Vec::new();
    let mut pending = 0_u64;
    let mut syncing = false;
    let mut fallback = false;
    for account in accounts {
        let owner = |mut item: Value| {
            item["profile"] = account["profile"].clone();
            item["revision"] = account["revision"].clone();
            item["account"] = account["name"].clone();
            item
        };
        for (source, kind) in [
            (&account["backup"]["active"], 0),
            (&account["mount_activity"]["active"], 0),
            (&account["cache"]["waiting"], 1),
            (&account["backup"]["recent"], 2),
        ] {
            if let Some(items) = source.as_array() {
                let destination = match kind {
                    0 => &mut active,
                    1 => &mut waiting,
                    _ => &mut recent,
                };
                destination.extend(items.iter().filter(|v| v.is_object()).cloned().map(owner));
            }
        }
        let count = account["cache"]["pending"].as_u64().unwrap_or(0);
        pending = pending.saturating_add(count);
        syncing |= count > 0
            || matches!(
                account["backup"]["state"].as_str(),
                Some("starting" | "scanning")
            );
        fallback |= account["route_fallback"] == true;
        for (failed, message) in [
            (
                account["mount_failed"] == true,
                "挂载进程已退出，请卸载后重试",
            ),
            (account["gateway_failed"] == true, "连接进程已退出，请重连"),
            (
                account["cache"]["write_error"] == true
                    || account["cache"]["recovery_files"].as_u64().unwrap_or(0) > 0,
                "挂载写入需要处理；编辑内容保留在本机，请打开该网盘的写入恢复目录",
            ),
            (
                account["backup_failed"] == true || account["backup"]["state"] == "error",
                "备份需要处理，请查看该网盘的备份状态",
            ),
            (
                account["cache"]["deletes_rejected"].as_u64().unwrap_or(0) > 0,
                "云端删除被拒绝，任务已保留",
            ),
        ] {
            if failed {
                issues.push(owner(json!({"message":message})));
            }
        }
    }
    json!({"syncing":syncing || !active.is_empty(),"error":!issues.is_empty(),
        "pending":pending,"active":active,"waiting":waiting,"recent":recent,"issues":issues,"route_fallback":fallback})
}

impl App {
    pub(super) fn mount_profile(
        &mut self,
        args: &Value,
        mount: bool,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Value> {
        let id = string(args, "profile")?;
        let profile = if self.connected.as_ref().is_some_and(|p| p.id == id) {
            self.connected.as_ref().unwrap()
        } else {
            &self.inactive.get(id).context("挂载所属连接已关闭")?.profile
        };
        ensure!(
            args["revision"].as_str() == Some(&profile.revision),
            "挂载所属连接或配置已改变，请刷新后重试"
        );
        #[cfg(any(windows, all(target_os = "linux", feature = "mount")))]
        native_mount::require_owner(profile, args)?;
        if mount {
            self.file_context(args)?;
        }
        let previous = self.connected.as_ref().map(|p| p.id.clone());
        let change = previous.as_deref() != Some(id);
        if change {
            self.park_active();
            ensure!(self.restore_active(id), "挂载所属连接已关闭");
        }
        let result = if mount {
            self.mount_current(cancel)
        } else {
            self.unmount(true)
        };
        if change {
            self.park_active();
            if let Some(previous) = previous {
                self.restore_active(&previous);
            }
        }
        result?;
        self.status()
    }

    pub(super) fn account_states(&mut self) -> anyhow::Result<Vec<Value>> {
        let mut states = Vec::new();
        for p in self.config.profiles.clone() {
            let (connected, mounted, mount_failed, backup_failed, gateway_failed, fallback) =
                if self
                    .connected
                    .as_ref()
                    .is_some_and(|active| active.id == p.id)
                {
                    let gateway_failed = self
                        .gateway
                        .as_mut()
                        .is_some_and(|c| c.0.try_wait().ok().flatten().is_some());
                    if gateway_failed {
                        self.backend = None;
                    }
                    (
                        self.backend.is_some(),
                        self.mounted.is_some(),
                        self.mounted
                            .as_mut()
                            .is_some_and(|c| c.0.try_wait().ok().flatten().is_some()),
                        self.backup
                            .as_mut()
                            .is_some_and(|c| c.0.try_wait().ok().flatten().is_some()),
                        gateway_failed,
                        self.network.fallback(),
                    )
                } else if let Some(saved) = self.inactive.get_mut(&p.id) {
                    let gateway_failed = saved
                        .gateway
                        .as_mut()
                        .is_some_and(|c| c.0.try_wait().ok().flatten().is_some());
                    if gateway_failed {
                        saved.backend = None;
                    }
                    (
                        saved.backend.is_some(),
                        saved.mounted.is_some(),
                        saved
                            .mounted
                            .as_mut()
                            .is_some_and(|c| c.0.try_wait().ok().flatten().is_some()),
                        saved
                            .backup
                            .as_mut()
                            .is_some_and(|c| c.0.try_wait().ok().flatten().is_some()),
                        gateway_failed,
                        saved.network.fallback(),
                    )
                } else {
                    continue;
                };
            let activity: Value = fs::read(self.mount_state(&p).join("activity.json"))
                .ok()
                .and_then(|s| serde_json::from_slice(&s).ok())
                .unwrap_or(json!({"active":[]}));
            let writable = if self
                .connected
                .as_ref()
                .is_some_and(|active| active.id == p.id)
            {
                self.backend.as_ref()
            } else {
                self.inactive
                    .get(&p.id)
                    .and_then(|saved| saved.backend.as_ref())
            }
            .is_some_and(|backend| backend.manual_upload_supported());
            states.push(json!({"profile":p.id,"revision":p.revision,"name":p.name,
                "account_name":self.account_name(&p),"connected":connected,"mounted":mounted,
                "mount_path":p.mount_path,"mount_readonly":!writable,"writable":writable,"mount_failed":mount_failed,"backup_failed":backup_failed,
                "gateway_failed":gateway_failed,"route_fallback":fallback,
                "cache":self.cache(&p,false)?,"backup":backup::report(&self.state,&p)?,"mount_activity":activity}));
        }
        Ok(states)
    }
    pub(super) fn refresh_inactive(&mut self) {
        for saved in self.inactive.values_mut() {
            if saved
                .gateway
                .as_mut()
                .is_some_and(|p| p.0.try_wait().ok().flatten().is_some())
            {
                saved.backend = None;
            }
        }
    }
    fn finish_quota_jobs(&mut self, profile: Option<&str>) {
        let ids: Vec<_> = self
            .jobs
            .iter()
            .filter(|(_, job)| {
                matches!(job.kind.as_str(), "quota" | "list")
                    && profile.is_none_or(|p| p == job.profile)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(job) = self.jobs.remove(&id) {
                job.cancel.store(true, Ordering::Relaxed);
                let _ = job.handle.join();
            }
        }
    }
    pub(super) fn online(&self, id: &str) -> bool {
        if self.connected.as_ref().is_some_and(|p| p.id == id) {
            self.backend.is_some()
        } else {
            self.inactive.get(id).is_some_and(|c| c.backend.is_some())
        }
    }
    pub(super) fn park_active(&mut self) {
        if let Some(profile) = self.connected.take() {
            let saved = Parked {
                profile,
                backend: self.backend.take(),
                gateway: self.gateway.take(),
                mounted: self.mounted.take(),
                backup: self.backup.take(),
                network: std::mem::take(&mut self.network),
            };
            self.inactive.insert(saved.profile.id.clone(), saved);
        }
    }
    pub(super) fn restore_active(&mut self, id: &str) -> bool {
        if let Some(saved) = self.inactive.remove(id) {
            self.connected = Some(saved.profile);
            self.backend = saved.backend;
            self.gateway = saved.gateway;
            self.mounted = saved.mounted;
            self.backup = saved.backup;
            self.network = saved.network;
            true
        } else {
            false
        }
    }
    pub(super) fn select_profile(&mut self, args: &Value) -> anyhow::Result<Value> {
        let id = string(args, "id")?;
        let profile = self.profile(id)?;
        ensure!(
            args["revision"].as_str() == Some(profile.revision.as_str()),
            "连接配置已改变，请刷新后重试"
        );
        self.refresh_inactive();
        self.select_online(id)?.context("账号已断开，请先连接")?;
        // Selection never invokes provider authentication, enumeration or mount.
        Ok(json!({"retained":true,"status":self.status()?}))
    }
    pub(super) fn select_online(&mut self, id: &str) -> anyhow::Result<Option<Value>> {
        if !self.online(id) {
            return Ok(None);
        }
        if self.connected.as_ref().is_none_or(|p| p.id != id) {
            self.park_active();
            ensure!(self.restore_active(id), "连接状态已改变");
        }
        self.config.selected = Some(id.into());
        self.state.save("profiles.json", &self.config)?;
        Ok(Some(
            json!({"retained":true,"revision":self.connected.as_ref().unwrap().revision}),
        ))
    }
    pub(super) fn file_context(&self, args: &Value) -> anyhow::Result<FileContext> {
        let id = string(args, "profile")?;
        let (profile, backend) = if self.connected.as_ref().is_some_and(|p| p.id == id) {
            (self.connected.as_ref().unwrap(), self.backend.as_ref())
        } else {
            let saved = self
                .inactive
                .get(id)
                .context("文件操作所属连接已断开，请重新连接")?;
            (&saved.profile, saved.backend.as_ref())
        };
        ensure!(
            args["revision"].as_str() == Some(profile.revision.as_str()),
            "连接配置已更新，请刷新后重新选择文件"
        );
        Ok(FileContext {
            profile: profile.clone(),
            backend: backend.context("请先连接服务器")?.clone(),
            state: self.state.clone(),
        })
    }
    pub(super) fn disconnect_profile(&mut self, id: &str) -> anyhow::Result<()> {
        self.finish_quota_jobs(Some(id));
        ensure!(
            !self
                .jobs
                .values()
                .any(|job| job.profile == id && !job.handle.is_finished()),
            "此账号仍有操作正在执行，请先完成或取消"
        );
        if self.connected.as_ref().is_some_and(|p| p.id == id) {
            return self.disconnect();
        }
        if !self.inactive.contains_key(id) {
            return Ok(());
        }
        let previous = self.connected.as_ref().map(|p| p.id.clone());
        self.park_active();
        self.restore_active(id);
        let result = self.disconnect();
        if result.is_err() {
            self.park_active();
        }
        if let Some(previous) = previous {
            self.restore_active(&previous);
        }
        result
    }
    pub(super) fn disconnect_all(&mut self) -> anyhow::Result<()> {
        self.finish_quota_jobs(None);
        ensure!(
            self.jobs.values().all(|job| job.handle.is_finished()),
            "仍有传输正在执行，请先完成或取消"
        );
        let ids: Vec<_> = self
            .inactive
            .keys()
            .cloned()
            .chain(self.connected.iter().map(|p| p.id.clone()))
            .collect();
        // Validate pending mount work before beginning teardown of any account.
        for id in &ids {
            let mounted = if self.connected.as_ref().is_some_and(|p| &p.id == id) {
                self.mounted.is_some()
            } else {
                self.inactive.get(id).is_some_and(|s| s.mounted.is_some())
            };
            if mounted && providers::editable(&self.profile(id)?) {
                ensure!(
                    self.cache(&self.profile(id)?, false)?["pending"] == 0,
                    "挂载仍有未上传或冲突的数据，请先完成同步；缓存已保留"
                );
            }
        }
        for id in ids {
            self.disconnect_profile(&id)?;
        }
        Ok(())
    }
    pub(super) fn reap_jobs(&mut self) {
        let done: Vec<_> = self
            .jobs
            .iter()
            .filter(|(_, job)| job.handle.is_finished())
            .map(|(id, _)| *id)
            .collect();
        for id in done {
            if let Some(job) = self.jobs.remove(&id) {
                let _ = job.handle.join();
            }
        }
    }
    pub(super) fn start_file_job(
        &mut self,
        req: Request,
        cancel: Arc<AtomicBool>,
        out: Output,
        cancellations: Cancellations,
        notifications: bool,
    ) -> anyhow::Result<()> {
        self.reap_jobs();
        ensure!(self.jobs.len() < 8, "同时执行的操作过多，请稍后重试");
        let context = self.file_context(&req.args)?;
        let profile = context.profile.id.clone();
        let id = req.id;
        let kind = req.op.clone();
        let job_cancel = cancel.clone();
        let job = thread::spawn(move || {
            let observable = notifications && !matches!(req.op.as_str(), "quota" | "list");
            if observable {
                emit(
                    &out,
                    json!({"id":id,"event":"operation","op":req.op,"profile":context.profile.id,"running":true}),
                );
            }
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                context.execute(&req, &cancel, &out)
            }))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("文件操作意外中断，已有文件未覆盖，请重试")));
            if observable {
                emit(
                    &out,
                    json!({"id":id,"event":"operation","op":req.op,"profile":context.profile.id,"running":false}),
                );
            }
            emit(
                &out,
                match result {
                    Ok(value) => json!({"id":id,"ok":true,"result":value}),
                    Err(error) => json!({"id":id,"ok":false,"error":error.to_string()}),
                },
            );
            cancellations.lock().unwrap().remove(&id);
        });
        self.jobs.insert(
            id,
            Job {
                profile,
                kind,
                cancel: job_cancel,
                handle: job,
            },
        );
        Ok(())
    }
    pub(super) fn stop_jobs(&mut self, cancellations: &Cancellations) {
        for id in self.jobs.keys() {
            if let Some(cancel) = cancellations.lock().unwrap().get(id) {
                cancel.store(true, Ordering::Relaxed);
            }
        }
        for (_, job) in self.jobs.drain() {
            let _ = job.handle.join();
        }
    }
    pub(super) fn cleanup_inactive(&mut self) {
        let _ = self.unmount(false);
        self.park_active();
        let ids: Vec<_> = self.inactive.keys().cloned().collect();
        for id in ids {
            self.restore_active(&id);
            let _ = self.unmount(false);
            self.connected.take();
            self.backend.take();
            self.gateway.take();
            self.backup.take();
        }
    }
}

impl FileContext {
    pub(super) fn execute(
        &self,
        req: &Request,
        cancel: &Arc<AtomicBool>,
        out: &Output,
    ) -> anyhow::Result<Value> {
        ensure!(!cancel.load(Ordering::Relaxed), "操作已取消");
        let a = &req.args;
        if req.op == "quota" {
            let quota = self.backend.storage_usage(&mut || {
                ensure!(!cancel.load(Ordering::Relaxed), "操作已取消");
                Ok(())
            })?;
            return Ok(match quota {
                Some(usage) => json!({"state":"ready","usage":usage}),
                None => json!({"state":"unsupported"}),
            });
        }
        if req.op != "list" {
            ensure!(
                !self.state.dir.join("paused").exists(),
                "同步已暂停，请先恢复"
            );
        }
        let path = string(a, "path")?;
        match req.op.as_str() {
            "list" => Ok(
                json!({"path":path,"entries":self.backend.list_directory_checked(path, &mut || {
                    ensure!(!cancel.load(Ordering::Relaxed), "目录刷新已取消");
                    Ok(())
                })?.into_iter().map(metadata).collect::<Vec<_>>()}),
            ),
            "mkdir" => Ok(metadata(self.backend.create_folder(path)?)),
            "rename" => Ok(metadata(self.backend.rename_path(path, string(a, "to")?)?)),
            "delete" => {
                ensure!(path != "/", "不能删除共享根目录");
                self.backend.delete_path(path)?;
                Ok(json!({}))
            }
            "upload" => {
                let out = out.clone();
                let id = req.id;
                let cancel = cancel.clone();
                let last = Arc::new(Mutex::new(Instant::now() - Duration::from_secs(1)));
                let progress = Arc::new(move |done, total| {
                    ensure!(
                        !cancel.load(Ordering::Relaxed),
                        "上传已取消；若服务器已接收，请刷新确认结果"
                    );
                    let mut last = last.lock().unwrap();
                    if done == total || last.elapsed() >= Duration::from_millis(150) {
                        emit(
                            &out,
                            json!({"id":id,"event":"progress","done":done,"total":total}),
                        );
                        *last = Instant::now();
                    }
                    Ok(())
                });
                Ok(metadata(self.backend.manual_upload_path(
                    path,
                    Path::new(string(a, "source")?),
                    a["etag"].as_str(),
                    progress,
                )?))
            }
            "download" => {
                let output = PathBuf::from(string(a, "output")?);
                ensure!(output.is_absolute(), "下载目标必须是绝对路径");
                ensure!(!output.exists(), "目标文件已存在，请选择另一个名称");
                let mut temp =
                    tempfile::NamedTempFile::new_in(output.parent().context("invalid output")?)?;
                let mut last = Instant::now() - Duration::from_secs(1);
                let total = a["size"].as_u64().unwrap_or(0);
                let done = self
                    .backend
                    .download_path_to(path, &mut temp, &mut |done| {
                        ensure!(!cancel.load(Ordering::Relaxed), "下载已取消");
                        if last.elapsed() >= Duration::from_millis(150) {
                            emit(
                                out,
                                json!({"id":req.id,"event":"progress","done":done,"total":total}),
                            );
                            last = Instant::now();
                        }
                        Ok(())
                    })?;
                ensure!(!cancel.load(Ordering::Relaxed), "下载已取消");
                temp.flush()?;
                temp.as_file().sync_all()?;
                temp.persist_noclobber(output)?;
                Ok(json!({"bytes":done}))
            }
            _ => anyhow::bail!("unknown operation"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_provider::OpaqueProvider;
    use super::*;
    fn fixture(root: &Path) -> App {
        let state = State::new(root.join("private")).unwrap();
        let a = Profile {
            id: "a".into(),
            revision: "rev-a".into(),
            name: "First".into(),
            ..Default::default()
        };
        let b = Profile {
            id: "b".into(),
            revision: "rev-b".into(),
            name: "Second".into(),
            ..Default::default()
        };
        let one = OpaqueProvider::default();
        one.upload("/same.txt", b"account-one".to_vec()).unwrap();
        let two = OpaqueProvider::default();
        two.upload("/same.txt", b"account-two".to_vec()).unwrap();
        let mut app = App {
            args: Args {
                state: state.dir.clone(),
                peer_bin: PathBuf::new(),
                launcher: PathBuf::new(),
                mount_worker: None,
                backup_worker: None,
                native_local_fixture_root: None,
            },
            state,
            config: Config {
                profiles: vec![a.clone(), b.clone()],
                selected: Some(a.id.clone()),
            },
            connected: Some(a),
            backend: Some(Backend::new(one)),
            gateway: None,
            mounted: None,
            backup: None,
            network: Arc::default(),
            hosting: sharing::Hosting::default(),
            native_fixture: local_files::NativeFixture::default(),
            inactive: HashMap::new(),
            jobs: HashMap::new(),
        };
        app.park_active();
        app.connected = Some(b);
        app.backend = Some(Backend::new(two));
        app.config.selected = Some("b".into());
        app
    }
    fn args(id: &str, revision: &str) -> Value {
        json!({"profile":id,"revision":revision,"path":"/same.txt"})
    }
    #[test]
    fn parked_account_failure_and_activity_are_visible_without_selecting_it() {
        let root = tempfile::tempdir().unwrap();
        let mut app = fixture(root.path());
        let p = app.profile("a").unwrap();
        backup::report(&app.state, &p).unwrap();
        State::new(app.state.dir.join("backup-a")).unwrap().save("report.json", &json!({
            "state":"error","active":[{"path":"/same.txt","kind":"upload","profile":"wrong-owner"}],
            "recent":[],"completed_scan":false
        })).unwrap();
        let status = app.status().unwrap();
        assert_eq!(status["profile"], "b");
        assert_eq!(app.config.selected.as_deref(), Some("b"));
        assert_eq!(status["overall"]["error"], true);
        assert_eq!(status["overall"]["syncing"], true);
        assert_eq!(status["overall"]["issues"][0]["profile"], "a");
        assert_eq!(status["overall"]["active"][0]["profile"], "a");
        assert_eq!(status["overall"]["active"][0]["revision"], "rev-a");
        assert_eq!(status["overall"]["active"][0]["account"], "First");
        assert_eq!(status["accounts"].as_array().unwrap().len(), 2);
        assert!(app.online("a") && app.online("b"));
    }

    #[test]
    fn aggregate_pending_and_errors_keep_equal_paths_separate_between_accounts() {
        let summary = aggregate_accounts(&[
            json!({"profile":"a","revision":"ra","name":"First","cache":{"pending":2,"waiting":[{"path":"/same"}]},"backup":{"state":"scanning"}}),
            json!({"profile":"b","revision":"rb","name":"Second","cache":{"pending":3,"waiting":[{"path":"/same"}],"deletes_rejected":1},"mount_failed":true,"route_fallback":true}),
        ]);
        assert_eq!(summary["pending"], 5);
        assert_eq!(summary["syncing"], true);
        assert_eq!(summary["error"], true);
        assert_eq!(summary["route_fallback"], true);
        assert_eq!(summary["waiting"][0]["profile"], "a");
        assert_eq!(summary["waiting"][1]["profile"], "b");
        assert_eq!(summary["issues"].as_array().unwrap().len(), 2);
    }
    #[test]
    fn selecting_retained_connections_keeps_identity_data_and_other_connection_online() {
        let root = tempfile::tempdir().unwrap();
        let mut app = fixture(root.path());
        let cancel = AtomicBool::new(false);
        assert_eq!(app.connect("a", &cancel).unwrap()["retained"], true);
        assert!(app.online("a") && app.online("b"));
        for (id, revision, bytes) in [
            ("a", "rev-a", b"account-one"),
            ("b", "rev-b", b"account-two"),
        ] {
            let ctx = app.file_context(&args(id, revision)).unwrap();
            let mut data = Vec::new();
            ctx.backend
                .download_path_to("/same.txt", &mut data, &mut |_| Ok(()))
                .unwrap();
            assert_eq!(data, bytes);
        }
        assert!(app.file_context(&args("a", "rev-b")).is_err());
        assert!(app.connect("missing", &cancel).is_err());
        assert_eq!(app.connected.as_ref().unwrap().id, "a");
        app.disconnect_profile("b").unwrap();
        assert!(app.online("a"));
        assert!(!app.online("b"));
        assert_eq!(app.public_profiles()["profiles"][0]["connected"], true);
    }
    #[test]
    fn explicit_selection_reuses_online_sessions_and_rejects_offline_or_stale_identity() {
        let root = tempfile::tempdir().unwrap();
        let mut app = fixture(root.path());
        for (id, revision) in [("a", "rev-a"), ("b", "rev-b"), ("a", "rev-a")] {
            let result = app
                .select_profile(&json!({"id":id,"revision":revision}))
                .unwrap();
            assert_eq!(result["retained"], true);
            assert_eq!(result["status"]["profile"], id);
            assert!(app.online("a") && app.online("b"));
        }
        assert!(
            app.select_profile(&json!({"id":"b","revision":"rev-a"}))
                .is_err()
        );
        assert_eq!(app.connected.as_ref().unwrap().id, "a");
        app.disconnect_profile("b").unwrap();
        let error = app
            .select_profile(&json!({"id":"b","revision":"rev-b"}))
            .unwrap_err();
        assert!(error.to_string().contains("账号已断开"));
        assert!(app.online("a") && !app.online("b"));
        assert_eq!(app.connected.as_ref().unwrap().id, "a");
    }
    #[test]
    fn switching_during_a_transfer_preserves_owner_and_disconnect_refuses_until_done() {
        let root = tempfile::tempdir().unwrap();
        let mut app = fixture(root.path());
        app.select_online("a").unwrap();
        let gate = Arc::new(AtomicBool::new(false));
        let mut provider = OpaqueProvider::default();
        provider.download_gate = Some(gate.clone());
        provider
            .upload("/same.txt", b"original-account".to_vec())
            .unwrap();
        app.backend = Some(Backend::new(provider));
        let output = root.path().join("download.bin");
        let req = Request {
            id: 99,
            op: "download".into(),
            args: json!({"profile":"a","revision":"rev-a","path":"/same.txt","output":output,"size":16}),
        };
        let flags: Cancellations = Arc::default();
        let cancel = Arc::new(AtomicBool::new(false));
        flags.lock().unwrap().insert(99, cancel.clone());
        app.start_file_job(
            req,
            cancel,
            Arc::new(Mutex::new(std::io::stdout())),
            flags,
            false,
        )
        .unwrap();
        assert_eq!(app.select_online("b").unwrap().unwrap()["retained"], true);
        assert!(app.online("a") && app.online("b"));
        assert!(app.disconnect_profile("a").is_err());
        gate.store(true, Ordering::Relaxed);
        let start = Instant::now();
        while !app.jobs[&99].handle.is_finished() {
            assert!(start.elapsed() < Duration::from_secs(3));
            thread::sleep(Duration::from_millis(10));
        }
        app.reap_jobs();
        assert_eq!(fs::read(output).unwrap(), b"original-account");
        app.disconnect_profile("a").unwrap();
        assert!(app.online("b"));
    }
}
