use super::*;
use crate::write_recovery::{self, Recovery};
use serde_json::json;
use std::io::{Seek, SeekFrom};
const BLOCK: usize = 4 * 1024 * 1024;
impl BaiduDrive {
    pub fn with_writable(mut self, yes: bool) -> Self {
        self.writable = yes;
        self
    }
    fn mutation_lock(&self) -> anyhow::Result<(State, File)> {
        ensure!(self.writable, "百度写入已锁定");
        let root = State::new(self.state.dir.join("recovery"))?;
        let lock = write_recovery::operation_lock(&root, &mut || Ok(()))?;
        Ok((root, lock))
    }
    fn token(&self) -> anyhow::Result<String> {
        auth::access(&self.state, &self.api, &self.generation, None, &mut || {
            Ok(())
        })
    }
    fn post(&self, query: &[(&str, &str)], form: &[(&str, &str)]) -> anyhow::Result<Value> {
        self.api.post(&self.token()?, query, form)
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
        let remote = self.remote(path)?;
        ensure!(path != "/", "不能覆盖根目录");
        let size = std::fs::metadata(source)?.len();
        ensure!(size <= 64 * 1024 * 1024 * 1024, "文件超过当前上传预算");
        let expected = write_recovery::snapshot(self, path, id, version)?;
        let recovery = Recovery::prepare(
            self,
            &root.dir,
            path,
            Some(source),
            expected.as_ref(),
            &mut || progress(0, size),
        )?;
        let source = recovery.edited.as_ref().unwrap();
        let mut input = File::open(source)?;
        let mut hashes = Vec::new();
        let mut buffer = vec![0u8; BLOCK];
        loop {
            progress(0, size)?;
            let n = read_block(&mut input, &mut buffer)?;
            if n == 0 && !hashes.is_empty() {
                break;
            }
            hashes.push(format!("{:x}", md5::Md5::digest(&buffer[..n])));
            if n < BLOCK {
                break;
            }
        }
        let blocks = serde_json::to_string(&hashes)?;
        let size_text = size.to_string();
        let revision = if expected.is_some() { "1" } else { "0" };
        let rtype = if expected.is_some() { "3" } else { "0" };
        let pre = self.post(
            &[("method", "precreate")],
            &[
                ("path", &remote),
                ("size", &size_text),
                ("isdir", "0"),
                ("autoinit", "1"),
                ("rtype", rtype),
                ("is_revision", revision),
                ("block_list", &blocks),
            ],
        )?;
        let upload = pre["uploadid"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() < 4096)
            .context("百度未提供上传会话，编辑副本保留")?;
        let needed = pre["block_list"]
            .as_array()
            .context("百度未明确返回分片列表")?;
        let mut seen = HashSet::new();
        for block in needed {
            progress(0, size)?;
            let index = block
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n < hashes.len() && seen.insert(*n))
                .context("百度分片序号越界或重复")?;
            input.seek(SeekFrom::Start(index as u64 * BLOCK as u64))?;
            let n = read_block(&mut input, &mut buffer)?;
            let response =
                self.api
                    .slice(&self.token()?, &remote, upload, index, buffer[..n].to_vec())?;
            ensure!(
                response["md5"].as_str() == Some(hashes[index].as_str()),
                "百度分片校验失败，未提交文件"
            );
            progress(((index + 1) as u64 * BLOCK as u64).min(size), size)?;
        }
        // Check again after transferring temporary blocks, immediately before
        // publishing. This is a preflight, not an atomic cloud CAS.
        write_recovery::snapshot(self, path, id, version)?;
        progress(size, size)?;
        let created = self.post(
            &[("method", "create")],
            &[
                ("path", &remote),
                ("size", &size_text),
                ("isdir", "0"),
                ("uploadid", upload),
                ("rtype", rtype),
                ("is_revision", revision),
                ("block_list", &blocks),
            ],
        )?;
        ensure!(
            created["path"].as_str() == Some(remote.as_str()),
            "百度保存到其他名称，恢复副本保留，请刷新确认"
        );
        let m = self
            .get_metadata_by_path(path)?
            .context("百度未返回新文件，恢复副本保留")?;
        ensure!(m.size == size, "百度保存大小不一致，恢复副本保留");
        recovery.verify(self, &m, &mut || progress(size, size))?;
        recovery.complete(path)?;
        Ok(m)
    }
    pub(super) fn mkdir_manual(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        let (_root, _lock) = self.mutation_lock()?;
        let remote = self.remote(path)?;
        ensure!(
            path != "/" && self.get_metadata_by_path(path)?.is_none(),
            "目录已存在"
        );
        self.post(
            &[("method", "create")],
            &[
                ("path", &remote),
                ("size", "0"),
                ("isdir", "1"),
                ("rtype", "0"),
            ],
        )?;
        self.get_metadata_by_path(path)?.context("百度未确认新目录")
    }
    fn manage(&self, operation: &str, files: Value) -> anyhow::Result<()> {
        let list = serde_json::to_string(&files)?;
        let v = self.post(
            &[("method", "filemanager"), ("opera", operation)],
            &[("async", "0"), ("ondup", "fail"), ("filelist", &list)],
        )?;
        let info = v["info"].as_array().context("百度未明确返回文件操作结果")?;
        ensure!(
            info.len() == 1 && info[0]["errno"] == 0,
            "百度未完成指定文件操作，请刷新确认"
        );
        Ok(())
    }
    pub(super) fn rename_manual(&self, id: &str, to: &str) -> anyhow::Result<MetadataEntry> {
        let (_root, _lock) = self.mutation_lock()?;
        let s = self.decode(id)?;
        let remote = self.remote(to)?;
        ensure!(
            s.path != "/" && to != "/" && !to.starts_with(&format!("{}/", s.path)),
            "无效的移动目标"
        );
        let now = self
            .get_metadata_by_path(&s.path)?
            .context("源文件不存在")?;
        ensure!(
            now.remote_id == id && self.get_metadata_by_path(to)?.is_none(),
            "文件版本改变或目标已存在"
        );
        let (parent, name) = remote.rsplit_once('/').context("无效目标")?;
        self.manage("move",json!([{"path":self.remote(&s.path)?,"dest":if parent.is_empty(){"/"}else{parent},"newname":name,"ondup":"fail"}]))?;
        self.get_metadata_by_path(to)?.context("百度未确认移动结果")
    }
    pub(super) fn delete_manual(&self, id: &str) -> anyhow::Result<()> {
        let (root, _lock) = self.mutation_lock()?;
        let s = self.decode(id)?;
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
        self.manage("delete", json!([self.remote(&s.path)?]))?;
        ensure!(
            self.get_metadata_by_path(&s.path)?.is_none(),
            "百度未确认删除结果，请刷新"
        );
        if let Some(r) = recovery {
            r.complete(&s.path)?;
        }
        Ok(())
    }
}
fn read_block(file: &mut File, buffer: &mut [u8]) -> anyhow::Result<usize> {
    let mut n = 0;
    while n < buffer.len() {
        let count = file.read(&mut buffer[n..])?;
        if count == 0 {
            break;
        }
        n += count;
    }
    Ok(n)
}
