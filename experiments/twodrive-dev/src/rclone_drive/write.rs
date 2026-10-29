use super::*;
use crate::write_recovery::{self, Recovery};
impl RcloneDrive {
    pub fn with_writable(mut self, yes: bool) -> Self {
        self.writable = yes;
        self
    }
    fn mutation_lock(&self) -> anyhow::Result<(State, File)> {
        ensure!(self.writable, "iCloud 写入已锁定");
        let recovery = State::new(self.state.dir.join("recovery"))?;
        let lock = wait_for_operation(&recovery, Duration::from_secs(45), || {
            ensure!(!self.cancel.load(Ordering::Relaxed), "连接已取消");
            Ok(())
        })?;
        Ok((recovery, lock))
    }
    fn mutate(
        &self,
        cmd: Command,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let _lock = wait_for_operation(&self.state, Duration::from_secs(45), &mut *check)?;
        let auth: Auth = self.state.read(MANIFEST)?;
        ensure!(auth.ready && auth.binding == self.binding, "登录状态已改变");
        stream(
            cmd,
            &mut std::io::sink(),
            |_| check(),
            u64::MAX,
            &self.cancel,
        )?;
        Ok(())
    }
    pub(super) fn put_manual(
        &self,
        path: &str,
        source: &Path,
        id: Option<&str>,
        version: Option<&str>,
        progress: Progress,
    ) -> anyhow::Result<MetadataEntry> {
        let (root, _lock) = self.mutation_lock()?;
        valid_path(path)?;
        ensure!(path != "/", "不能写入根目录");
        let size = fs::metadata(source)?.len();
        let mut check = || progress(0, size);
        let expected = write_recovery::snapshot(self, path, id, version)?;
        let recovery = Recovery::prepare(
            self,
            &root.dir,
            path,
            Some(source),
            expected.as_ref(),
            &mut check,
        )?;
        write_recovery::snapshot(self, path, id, version)?;
        check()?;
        let remote = format!("twodrive:{}", path.trim_start_matches('/'));
        let mut cmd = command(&self.exe, &self.state, &self.pass, &self.route);
        cmd.arg("copyto")
            .arg(recovery.edited.as_ref().unwrap())
            .arg(&remote)
            .arg("--ignore-times");
        if expected.is_none() {
            cmd.arg("--ignore-existing");
        }
        self.mutate(cmd, &mut check)?;
        let entry = self
            .get_metadata_by_path(path)?
            .context("iCloud 尚未返回已保存文件，恢复副本保留")?;
        recovery.verify(self, &entry, &mut check)?;
        recovery.complete(path)?;
        progress(size, size)?;
        Ok(entry)
    }
    pub(super) fn mkdir_manual(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        let (_root, _lock) = self.mutation_lock()?;
        valid_path(path)?;
        ensure!(
            path != "/" && self.get_metadata_by_path(path)?.is_none(),
            "目录已存在"
        );
        let mut cmd = command(&self.exe, &self.state, &self.pass, &self.route);
        cmd.args([
            "mkdir",
            &format!("twodrive:{}", path.trim_start_matches('/')),
        ]);
        let created = crate::state::now() as i64;
        self.mutate(cmd, &mut || Ok(()))?;
        let entry = self
            .get_metadata_by_path(path)?
            .context("iCloud 未返回新目录")?;
        self.remember_created(entry, created)
    }
    pub(super) fn rename_manual(&self, id: &str, to: &str) -> anyhow::Result<MetadataEntry> {
        let (_root, _lock) = self.mutation_lock()?;
        let s = Snapshot::decode(id, &self.binding)?;
        valid_path(to)?;
        ensure!(
            s.path != "/" && to != "/" && !to.starts_with(&format!("{}/", s.path)),
            "无效的移动目标"
        );
        let current = self
            .get_metadata_by_path(&s.path)?
            .context("源文件不存在")?;
        ensure!(
            current.remote_id == id && self.get_metadata_by_path(to)?.is_none(),
            "文件版本改变或目标已存在"
        );
        let mut cmd = command(&self.exe, &self.state, &self.pass, &self.route);
        cmd.args([
            "moveto",
            &format!("twodrive:{}", s.path.trim_start_matches('/')),
            &format!("twodrive:{}", to.trim_start_matches('/')),
            "--ignore-existing",
        ]);
        self.mutate(cmd, &mut || Ok(()))?;
        self.get_metadata_by_path(to)?
            .context("iCloud 未确认移动结果，请刷新")
    }
    pub(super) fn delete_manual(&self, id: &str) -> anyhow::Result<()> {
        let (root, _lock) = self.mutation_lock()?;
        let s = Snapshot::decode(id, &self.binding)?;
        let current = self
            .get_metadata_by_path(&s.path)?
            .context("源文件不存在")?;
        ensure!(s.path != "/" && current.remote_id == id, "文件版本已改变");
        let recovery = if s.directory {
            ensure!(
                self.list_directory(&s.path)?.is_empty(),
                "请先移走或删除目录内的文件，再删除空目录"
            );
            None
        } else {
            Some(Recovery::prepare(
                self,
                &root.dir,
                &s.path,
                None,
                Some(&current),
                &mut || Ok(()),
            )?)
        };
        let mut cmd = command(&self.exe, &self.state, &self.pass, &self.route);
        cmd.args([
            if s.directory { "rmdir" } else { "deletefile" },
            &format!("twodrive:{}", s.path.trim_start_matches('/')),
        ]);
        self.mutate(cmd, &mut || Ok(()))?;
        ensure!(
            self.get_metadata_by_path(&s.path)?.is_none(),
            "iCloud 未确认删除结果，请刷新"
        );
        if let Some(recovery) = recovery {
            recovery.complete(&s.path)?;
        }
        Ok(())
    }
}
