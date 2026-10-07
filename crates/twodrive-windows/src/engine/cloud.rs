use super::*;
use sha2::{Digest, Sha256};
use twodrive_core::{CloudIdentity, DownloadTask, IndexedItem};

fn guard_session(state: &State, epoch: u64, cancel: &AtomicBool) -> anyhow::Result<()> {
    anyhow::ensure!(
        state.epoch == epoch
            && state.signed_in
            && !state.signing_out
            && !cancel.load(Ordering::SeqCst),
        "stale_account"
    );
    Ok(())
}

impl Engine {
    fn graph_locked(
        &self,
        state: &mut State,
    ) -> anyhow::Result<Arc<twodrive_backend::GraphBackend>> {
        anyhow::ensure!(state.signed_in && !state.signing_out, "not_signed_in");
        if state.graph.is_none() {
            state.graph = Some(Arc::new(
                twodrive_backend::GraphBackend::from_paths(&self.0.paths)
                    .map_err(|_| anyhow::anyhow!("reauthentication_required"))?,
            ));
        }
        Ok(state.graph.as_ref().unwrap().clone())
    }
    fn cache_paths(&self, identity: &CloudIdentity, item: &IndexedItem) -> (PathBuf, PathBuf) {
        let digest = Sha256::digest(
            serde_json::to_vec(&(identity, &item.id, &item.etag, item.size)).unwrap(),
        );
        let key = format!("{digest:x}");
        // Keep a conservative extension for desktop file associations; never use a remote path.
        let extension = Path::new(&item.name)
            .extension()
            .and_then(|s| s.to_str())
            .filter(|s| s.len() <= 12 && s.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or("bin");
        (
            self.0.cache.join(format!("{key}.partial")),
            self.0.cache.join(format!("{key}.{extension}")),
        )
    }
    fn valid_cache(&self, task: &DownloadTask, item: &IndexedItem) -> bool {
        task.state == "cached"
            && task.item.etag == item.etag
            && task.item.size == item.size
            && self
                .cache_paths(&task.identity, &task.item)
                .1
                .metadata()
                .is_ok_and(|m| m.is_file() && m.len() == item.size)
    }
    pub(super) fn cloud_view(&self, state: &State) -> anyhow::Result<Option<CloudView>> {
        if !state.signed_in {
            return Ok(None);
        }
        let mut view = CloudView {
            file_count: 0,
            cached_bytes: 0,
            status: state.cloud_status.clone(),
            error: state.cloud_error.clone(),
            offset: state.cloud_offset,
            count: 0,
            items: vec![],
            tasks: vec![],
        };
        if let Some(identity) = &state.cloud_identity {
            let items = self.0.db.cloud_items(identity)?;
            let tasks = self.0.db.cloud_tasks(identity)?;
            view.count = items.len();
            view.file_count = items.iter().filter(|i| i.kind == "file").count();
            view.cached_bytes = items
                .iter()
                .filter(|i| {
                    tasks
                        .iter()
                        .any(|t| t.item.id == i.id && self.valid_cache(t, i))
                })
                .map(|i| i.size)
                .sum();
            view.items = items
                .into_iter()
                .skip(state.cloud_offset)
                .take(100)
                .map(|item| {
                    let task = tasks
                        .iter()
                        .find(|t| t.item.id == item.id && t.item.etag == item.etag);
                    let status = match task {
                        Some(t) if self.valid_cache(t, &item) => "cached",
                        Some(t) if t.state == "downloading" => "downloading",
                        Some(t) if t.state == "error" || t.state == "cached" => "error",
                        _ => "online_only",
                    };
                    CloudFile {
                        item,
                        state: status.into(),
                    }
                })
                .collect();
            view.tasks = tasks
                .into_iter()
                .filter(|t| t.state == "downloading" || t.state == "error")
                .take(100)
                .collect();
        }
        Ok(Some(view))
    }
    pub(super) fn refresh_index(&self, state: &mut State) -> anyhow::Result<()> {
        anyhow::ensure!(state.refresh_cancel.is_none(), "refresh_in_progress");
        let graph = self.graph_locked(state)?;
        let cancel = Arc::new(AtomicBool::new(false));
        state.refresh_cancel = Some(cancel.clone());
        state.cloud_status = "refreshing".into();
        state.cloud_error = None;
        let epoch = state.epoch;
        let engine = self.clone();
        thread::spawn(move || {
            let mut check = || {
                anyhow::ensure!(!cancel.load(Ordering::SeqCst), "refresh_cancelled");
                Ok(())
            };
            let result = (|| -> anyhow::Result<()> {
                let identity = graph.readonly_identity(&mut check)?;
                let (link, replace, _) = graph.stage_delta(&engine.0.db, &identity, &mut check)?;
                let mut state = engine.0.state.lock().unwrap();
                check()?;
                guard_session(&state, epoch, &cancel)?;
                engine.0.db.cloud_commit(&identity, &link, replace)?;
                engine
                    .0
                    .db
                    .set_state_value("cloud_identity", &serde_json::to_string(&identity)?)?;
                state.cloud_identity = Some(identity);
                Ok(())
            })();
            let mut state = engine.0.state.lock().unwrap();
            state.refresh_cancel = None;
            if state.epoch == epoch {
                state.cloud_status = if result.is_ok() { "ready" } else { "error" }.into();
                state.cloud_error = result.err().map(|e| e.to_string());
                state.revision += 1;
            }
        });
        Ok(())
    }
    pub(super) fn start_cloud_download(&self, state: &mut State, id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(!state.paused, "paused");
        anyhow::ensure!(state.download_cancel.is_none(), "download_busy");
        let graph = self.graph_locked(state)?;
        let identity = state
            .cloud_identity
            .clone()
            .ok_or_else(|| anyhow::anyhow!("refresh_index_first"))?;
        let item = self
            .0
            .db
            .cloud_items(&identity)?
            .into_iter()
            .find(|i| i.id == id)
            .ok_or_else(|| anyhow::anyhow!("unknown_item"))?;
        anyhow::ensure!(
            item.kind == "file" && !item.etag.is_empty(),
            "unsupported_item"
        );
        let (partial, complete) = self.cache_paths(&identity, &item);
        let mut task = DownloadTask {
            identity: identity.clone(),
            item: item.clone(),
            state: "downloading".into(),
            done: partial.metadata().map(|m| m.len()).unwrap_or(0),
            error: None,
        };
        if let Some(old) = self
            .0
            .db
            .cloud_tasks(&identity)?
            .iter()
            .find(|t| t.item.id == id)
        {
            anyhow::ensure!(!self.valid_cache(old, &item), "already_cached");
        }
        self.0.db.cloud_task_save(&task)?;
        let cancel = Arc::new(AtomicBool::new(false));
        state.download_cancel = Some((id.into(), cancel.clone()));
        let epoch = state.epoch;
        let engine = self.clone();
        thread::spawn(move || {
            let mut check = || {
                anyhow::ensure!(!cancel.load(Ordering::SeqCst), "download_cancelled");
                Ok(())
            };
            let mut last_save = std::time::Instant::now();
            let result = (|| -> anyhow::Result<()> {
                anyhow::ensure!(
                    graph.readonly_identity(&mut check)? == identity,
                    "stale_account"
                );
                graph.download_partial(&identity, &item, &partial, &mut check, &mut |done| {
                    task.done = done;
                    if last_save.elapsed() >= Duration::from_millis(200) || done == item.size {
                        engine.0.db.cloud_task_save(&task)?;
                        last_save = std::time::Instant::now();
                    }
                    Ok(())
                })?;
                let state = engine.0.state.lock().unwrap();
                check()?;
                guard_session(&state, epoch, &cancel)?;
                anyhow::ensure!(
                    state.cloud_identity.as_ref() == Some(&identity),
                    "stale_account"
                );
                let current = engine
                    .0
                    .db
                    .cloud_items(&identity)?
                    .into_iter()
                    .find(|i| i.id == item.id);
                anyhow::ensure!(
                    current.is_some_and(|i| i.etag == item.etag && i.size == item.size),
                    "cloud_version_changed"
                );
                // The completed path is never authoritative until the following DB commit.
                if complete.exists() {
                    fs::remove_file(&complete)?;
                }
                fs::rename(&partial, &complete)?;
                task.state = "cached".into();
                task.done = item.size;
                task.error = None;
                engine.0.db.cloud_task_save(&task)?;
                Ok(())
            })();
            if let Err(error) = result {
                task.state = "error".into();
                task.error = Some(error.to_string());
                let _ = engine.0.db.cloud_task_save(&task);
            }
            let mut state = engine.0.state.lock().unwrap();
            state.download_cancel = None;
            state.revision += 1;
        });
        Ok(())
    }
    pub(super) fn open_cached(&self, state: &State, id: &str, reveal: bool) -> anyhow::Result<()> {
        anyhow::ensure!(state.signed_in && !state.signing_out, "not_signed_in");
        let identity = state
            .cloud_identity
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("refresh_index_first"))?;
        let item = self
            .0
            .db
            .cloud_items(identity)?
            .into_iter()
            .find(|i| i.id == id)
            .ok_or_else(|| anyhow::anyhow!("unknown_item"))?;
        let tasks = self.0.db.cloud_tasks(identity)?;
        let task = tasks
            .iter()
            .find(|t| t.item.id == id && self.valid_cache(t, &item))
            .ok_or_else(|| anyhow::anyhow!("cache_unavailable"))?;
        let path = self.cache_paths(identity, &task.item).1;
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            if reveal {
                std::process::Command::new("explorer.exe")
                    .arg(format!("/select,{}", path.display()))
                    .creation_flags(0x08000000)
                    .spawn()?;
            } else {
                use std::os::windows::ffi::OsStrExt;
                let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
                let result = unsafe {
                    windows_sys::Win32::UI::Shell::ShellExecuteW(
                        std::ptr::null_mut(),
                        std::ptr::null(),
                        wide.as_ptr(),
                        std::ptr::null(),
                        std::ptr::null(),
                        1,
                    )
                };
                anyhow::ensure!(result as usize > 32, "open_failed");
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (path, reveal);
            anyhow::bail!("open_requires_windows");
        }
        #[cfg(windows)]
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logout_account_change_and_cancel_reject_late_publication() {
        let mut state = State {
            signed_in: true,
            epoch: 7,
            ..State::default()
        };
        let cancel = AtomicBool::new(false);
        assert!(guard_session(&state, 7, &cancel).is_ok());
        state.signed_in = false;
        assert!(guard_session(&state, 7, &cancel).is_err());
        state.signed_in = true;
        state.epoch = 8;
        assert!(guard_session(&state, 7, &cancel).is_err());
        cancel.store(true, Ordering::SeqCst);
        assert!(guard_session(&state, 8, &cancel).is_err());
    }
    #[test]
    fn restart_partial_stale_and_missing_cache_never_report_cached() {
        let root = tempfile::tempdir().unwrap();
        let engine = Engine::open(root.path(), false).unwrap();
        let identity = CloudIdentity {
            account: "synthetic-account".into(),
            drive: "synthetic-drive".into(),
        };
        let item = IndexedItem {
            id: "file".into(),
            parent: None,
            name: "file.txt".into(),
            kind: "file".into(),
            size: 4,
            etag: "v1".into(),
            modified: None,
            deleted: false,
        };
        engine
            .0
            .db
            .cloud_stage(&identity, std::slice::from_ref(&item))
            .unwrap();
        engine
            .0
            .db
            .cloud_commit(&identity, "synthetic-cursor", true)
            .unwrap();
        engine
            .0
            .db
            .set_state_value("cloud_identity", &serde_json::to_string(&identity).unwrap())
            .unwrap();
        TokenStore::new(&engine.0.paths.token_path)
            .save(&twodrive_core::TokenData {
                access_token: "synthetic".into(),
                refresh_token: None,
                expires_at_unix: twodrive_core::now_unix() + 3600,
            })
            .unwrap();
        let mut task = DownloadTask {
            identity: identity.clone(),
            item: item.clone(),
            state: "downloading".into(),
            done: 2,
            error: None,
        };
        engine.0.db.cloud_task_save(&task).unwrap();
        let (partial, complete) = engine.cache_paths(&identity, &item);
        fs::write(&partial, b"ab").unwrap();
        drop(engine);
        let engine = Engine::open(root.path(), false).unwrap();
        let view = engine.snapshot().unwrap().cloud.unwrap();
        assert_eq!(view.items[0].state, "error");
        assert_eq!(view.tasks[0].done, 2);
        task.state = "cached".into();
        engine.0.db.cloud_task_save(&task).unwrap();
        assert_eq!(
            engine.snapshot().unwrap().cloud.unwrap().items[0].state,
            "error"
        );
        fs::write(&complete, b"abcd").unwrap();
        assert_eq!(
            engine.snapshot().unwrap().cloud.unwrap().items[0].state,
            "cached"
        );
        let mut changed = item;
        changed.etag = "v2".into();
        engine.0.db.cloud_stage(&identity, &[changed]).unwrap();
        engine.0.db.cloud_commit(&identity, "new", false).unwrap();
        assert_eq!(
            engine.snapshot().unwrap().cloud.unwrap().items[0].state,
            "online_only"
        );
        let mut state = engine.0.state.lock().unwrap();
        state.signed_in = false;
        assert!(engine.cloud_view(&state).unwrap().is_none());
        state.signed_in = true;
        state.cloud_identity = Some(CloudIdentity {
            account: "other".into(),
            drive: "synthetic-drive".into(),
        });
        assert!(engine.cloud_view(&state).unwrap().unwrap().items.is_empty());
    }
}
