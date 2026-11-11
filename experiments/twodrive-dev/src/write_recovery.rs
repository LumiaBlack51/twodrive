//! Recovery for explicit manual writes on APIs without atomic compare-and-swap.
//! This does not advertise conditional_upload or make backups eligible.
use crate::{
    model::{CloudBackend, MetadataEntry},
    state::{State, random_secret},
};
use anyhow::{Context, ensure};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};
pub struct Recovery {
    pub state: State,
    pub edited: Option<std::path::PathBuf>,
}
pub fn operation_lock(
    root: &State,
    check: &mut dyn FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<File> {
    use fs2::FileExt;
    let path = root.dir.join("operation.lock");
    ensure!(
        !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_symlink()),
        "拒绝链接形式的写入锁"
    );
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let start = std::time::Instant::now();
    loop {
        check()?;
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(file),
            Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                ensure!(
                    start.elapsed() < std::time::Duration::from_secs(45),
                    "写入排队超时，编辑内容保留，请稍后重试"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => anyhow::bail!("无法取得本机写入锁，未开始修改"),
        }
    }
}
pub fn snapshot(
    backend: &dyn CloudBackend,
    path: &str,
    id: Option<&str>,
    version: Option<&str>,
) -> anyhow::Result<Option<MetadataEntry>> {
    crate::provider_guard::snapshot(backend, path, id, version)
}
impl Recovery {
    pub fn prepare(
        backend: &dyn CloudBackend,
        root: &Path,
        path: &str,
        source: Option<&Path>,
        expected: Option<&MetadataEntry>,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Self> {
        let state = State::new(root.join(random_secret()))?;
        state.save(
            "record.json",
            &json!({"path":path,"state":"preparing","version":expected.map(|m|&m.etag)}),
        )?;
        let edited = if let Some(source) = source {
            let mut input = File::open(source)?;
            let mut output = File::create(state.dir.join("edited.bin"))?;
            let mut buffer = [0u8; 65536];
            loop {
                check()?;
                let n = input.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                output.write_all(&buffer[..n])?;
            }
            output.sync_all()?;
            Some(state.dir.join("edited.bin"))
        } else {
            None
        };
        if let Some(m) = expected {
            let mut original = File::create(state.dir.join("before.bin"))?;
            let n = backend.download_to(&m.remote_id, &mut original, &mut |_| check())?;
            ensure!(n == m.size, "恢复副本不完整，未开始修改");
            original.sync_all()?;
            snapshot(backend, path, Some(&m.remote_id), Some(&m.etag))?;
        }
        state.save("record.json",&json!({"path":path,"state":"ready","version":expected.map(|m|&m.etag),"has_original":expected.is_some()}))?;
        Ok(Self { state, edited })
    }
    pub fn complete(&self, path: &str) -> anyhow::Result<()> {
        self.state.save("record.json",&json!({"path":path,"state":"completed","has_original":self.state.dir.join("before.bin").is_file()}))?;
        if let Some(edited) = &self.edited {
            std::fs::remove_file(edited)?;
        }
        Ok(())
    }
    pub fn verify(
        &self,
        backend: &dyn CloudBackend,
        entry: &MetadataEntry,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        struct HashWriter(Sha256);
        impl Write for HashWriter {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.update(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let edited = self.edited.as_ref().context("没有待核对的编辑副本")?;
        let mut local = File::open(edited)?;
        let mut a = HashWriter(Sha256::new());
        std::io::copy(&mut local, &mut a)?;
        let mut b = HashWriter(Sha256::new());
        let count = backend.download_to(&entry.remote_id, &mut b, &mut |_| check())?;
        ensure!(
            count == entry.size && a.0.finalize() == b.0.finalize(),
            "云端内容核对未通过；原文件与编辑副本已保留在恢复目录"
        );
        Ok(())
    }
}
