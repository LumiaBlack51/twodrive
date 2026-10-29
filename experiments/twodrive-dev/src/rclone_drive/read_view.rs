//! Linux read-only metadata queries must not queue behind a whole-file read.
//! Each child may refresh its own encrypted config. Publish those updates only
//! after account identity validation and a nonblocking compare-and-replace.
use super::*;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

pub(super) fn encrypted_bytes(state: &State) -> anyhow::Result<Vec<u8>> {
    encrypted_bytes_wait(state, thread::sleep)
}
fn encrypted_bytes_wait(state: &State, mut wait: impl FnMut(Duration)) -> anyhow::Result<Vec<u8>> {
    let mut attempts = 0;
    let mut file = loop {
        match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(state.dir.join(CONFIG))
        {
            Ok(file) => break file,
            // The pinned component moves the previous config aside before
            // renaming the complete replacement. Retry only that short name
            // gap, never fall back to an older backup or loosen validation.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && attempts < 20 => {
                attempts += 1;
                wait(Duration::from_millis(10));
            }
            Err(e) => return Err(e.into()),
        }
    };
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.permissions().mode() & 0o077 == 0
            && meta.len() <= JSON_LIMIT as u64,
        "读取配置必须为本用户的私密加密文件"
    );
    let mut bytes = Vec::new();
    (&mut file)
        .take(JSON_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= JSON_LIMIT && bytes.starts_with(HEADER),
        "拒绝未加密或过大的读取配置"
    );
    Ok(bytes)
}
impl RcloneDrive {
    pub(super) fn check_read_identity(&self) -> anyhow::Result<()> {
        ensure!(!self.cancel.load(Ordering::Relaxed), "连接已取消");
        let auth: Auth = self.state.read(MANIFEST)?;
        ensure!(
            auth.ready && auth.kind == "icloud" && auth.binding == self.binding,
            "登录状态已改变，拒绝旧连接"
        );
        Ok(())
    }
    pub(super) fn publish_read_refresh(
        &self,
        view: &State,
        before: &[u8],
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        check()?;
        self.check_read_identity()?;
        let after = encrypted_bytes(view)?;
        if after == before {
            return Ok(());
        }
        // A component refresh must still belong to the exact connected account.
        let identity = check_config(&self.exe, view, &self.pass, &self.route, &self.cancel)?;
        ensure!(
            self.binding.starts_with(&format!("icloud:{identity}:")),
            "读取配置的账号身份已改变"
        );
        let after = encrypted_bytes(view)?;
        let lock = private_lock(&self.state, "operation.lock")?;
        match lock.try_lock_exclusive() {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }
        check()?;
        self.check_read_identity()?;
        if encrypted_bytes(&self.state)? == before {
            self.state.write_bytes(CONFIG, &after)?;
        }
        Ok(())
    }
    pub(super) fn query_read_view(
        &self,
        args: &[&str],
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Value> {
        ensure!(args.first() == Some(&"lsjson"), "读取视图仅允许元数据查询");
        check()?;
        self.check_read_identity()?;
        let before = encrypted_bytes(&self.state)?;
        // The lifetime account lock blocks login/logout. Canonical config saves
        // use complete replacement blobs; no network or account operation lock is needed
        // to copy a complete encrypted blob into this 0700 temporary directory.
        let temporary = tempfile::Builder::new()
            .prefix("metadata-view-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(&self.state.dir)?;
        let view = State::new(temporary.path().to_owned())?;
        view.write_bytes(CONFIG, &before)?;
        self.check_read_identity()?;
        let mut cmd = command(&self.exe, &view, &self.pass, &self.route);
        cmd.args(args);
        let mut bytes = Vec::new();
        stream(
            cmd,
            &mut bytes,
            |_| {
                self.check_read_identity()?;
                check()
            },
            JSON_LIMIT as u64,
            &self.cancel,
        )?;
        let value = serde_json::from_slice(&bytes).context("网盘组件返回的元数据格式无效")?;
        self.publish_read_refresh(&view, &before, check)?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn prepared() -> (tempfile::TempDir, RcloneDrive, Vec<u8>) {
        let (root, mut backend) = super::super::tests::fixture();
        backend.binding = format!("icloud:{:x}:epoch", Sha256::digest(b"synthetic-account"));
        backend
            .state
            .save(
                MANIFEST,
                &Auth {
                    kind: "icloud".into(),
                    binding: backend.binding.clone(),
                    ready: true,
                },
            )
            .unwrap();
        let before = [HEADER, b"\nSYNTHETIC BEFORE"].concat();
        backend.state.write_bytes(CONFIG, &before).unwrap();
        let exe = root.path().join("synthetic-component");
        fs::write(&exe, b"#!/usr/bin/python3\nimport sys\nassert sys.argv[-2:] == ['config','dump']\nprint('{\"twodrive\":{\"type\":\"iclouddrive\",\"apple_id\":\"synthetic-account\"}}')\n").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
        backend.exe = exe;
        (root, backend, before)
    }
    #[test]
    fn canonical_config_rename_gap_retries_without_loading_a_backup() {
        let (_root, backend, before) = prepared();
        fs::rename(
            backend.state.dir.join(CONFIG),
            backend.state.dir.join("preserved-old-blob"),
        )
        .unwrap();
        let mut waits = 0;
        let bytes = encrypted_bytes_wait(&backend.state, |_| {
            waits += 1;
            backend.state.write_bytes(CONFIG, &before).unwrap();
        })
        .unwrap();
        assert_eq!(waits, 1);
        assert_eq!(bytes, before);
        fs::remove_file(backend.state.dir.join(CONFIG)).unwrap();
        let mut waits = 0;
        assert!(encrypted_bytes_wait(&backend.state, |_| waits += 1).is_err());
        assert_eq!(waits, 20, "missing configs must fail after a bounded retry");
    }
    #[test]
    fn refresh_publication_keeps_identity_and_never_overwrites_a_newer_config() {
        let (root, backend, before) = prepared();
        let view = State::new(root.path().join("view")).unwrap();
        let refreshed = [HEADER, b"\nSYNTHETIC REFRESH"].concat();
        view.write_bytes(CONFIG, &refreshed).unwrap();
        backend
            .publish_read_refresh(&view, &before, &mut || Ok(()))
            .unwrap();
        assert_eq!(encrypted_bytes(&backend.state).unwrap(), refreshed);
        let newer = [HEADER, b"\nSYNTHETIC NEWER"].concat();
        backend.state.write_bytes(CONFIG, &newer).unwrap();
        backend
            .publish_read_refresh(&view, &before, &mut || Ok(()))
            .unwrap();
        assert_eq!(encrypted_bytes(&backend.state).unwrap(), newer);
        let mut changed: Auth = backend.state.read(MANIFEST).unwrap();
        changed.binding = "another-login-epoch".into();
        backend.state.save(MANIFEST, &changed).unwrap();
        assert!(
            backend
                .publish_read_refresh(&view, &newer, &mut || Ok(()))
                .is_err()
        );
        assert_eq!(encrypted_bytes(&backend.state).unwrap(), newer);
    }
    #[test]
    fn busy_refresh_publication_is_nonblocking_and_does_not_release_the_owner() {
        let (root, backend, before) = prepared();
        let view = State::new(root.path().join("view")).unwrap();
        view.write_bytes(CONFIG, &[HEADER, b"\nSYNTHETIC REFRESH"].concat())
            .unwrap();
        let owner = private_lock(&backend.state, "operation.lock").unwrap();
        owner.try_lock_exclusive().unwrap();
        let start = Instant::now();
        backend
            .publish_read_refresh(&view, &before, &mut || Ok(()))
            .unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(encrypted_bytes(&backend.state).unwrap(), before);
        let probe = private_lock(&backend.state, "operation.lock").unwrap();
        assert!(probe.try_lock_exclusive().is_err());
    }
    #[test]
    fn read_view_rejects_plaintext_links_public_files_and_reaps_failed_child() {
        let (_root, mut backend, before) = prepared();
        let config = backend.state.dir.join(CONFIG);
        backend
            .state
            .write_bytes(CONFIG, b"SYNTHETIC PLAINTEXT")
            .unwrap();
        assert!(
            backend
                .query_read_view(&["lsjson", "twodrive:"], &mut || Ok(()))
                .is_err()
        );
        backend.state.write_bytes(CONFIG, &before).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(encrypted_bytes(&backend.state).is_err());
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        fs::rename(&config, backend.state.dir.join("original")).unwrap();
        std::os::unix::fs::symlink(backend.state.dir.join("original"), &config).unwrap();
        assert!(encrypted_bytes(&backend.state).is_err());
        fs::remove_file(&config).unwrap();
        fs::rename(backend.state.dir.join("original"), &config).unwrap();
        let exe = backend.state.dir.join("failed-component");
        fs::write(&exe, b"#!/usr/bin/python3\nimport os,sys,pathlib\np=pathlib.Path(sys.argv[sys.argv.index('--config')+1])\nassert p.parent.stat().st_mode & 0o77 == 0\nassert p.stat().st_mode & 0o77 == 0\nassert p.parent.name.startswith('metadata-view-')\nprint('invalid-json')\n").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
        backend.exe = exe;
        let error = backend
            .query_read_view(&["lsjson", "twodrive:"], &mut || Ok(()))
            .unwrap_err();
        assert!(
            error.to_string().contains("元数据格式无效"),
            "failed child must actually execute: {error}"
        );
        assert_eq!(encrypted_bytes(&backend.state).unwrap(), before);
        assert!(!fs::read_dir(&backend.state.dir).unwrap().any(|p| {
            p.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("metadata-view-")
        }));
    }
}
