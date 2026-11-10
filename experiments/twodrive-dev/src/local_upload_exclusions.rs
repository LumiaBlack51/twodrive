//! Durable local namespace for excluded upload names. No cloud mutations.
use crate::{
    model::MetadataEntry,
    provider::valid_path,
    state::{State, random_secret},
};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs::File, path::Path, sync::Mutex};

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    id: String,
    size: u64,
    modified: i64,
}
pub(crate) struct LocalExcluded {
    state: State,
    records: Mutex<BTreeMap<String, Record>>,
}
impl LocalExcluded {
    pub(crate) fn has_children(&self, path: &str) -> anyhow::Result<bool> {
        let prefix = format!("{}/", path.trim_end_matches('/'));
        Ok(self
            .records
            .lock()
            .map_err(|_| anyhow::anyhow!("本地文件索引不可用"))?
            .keys()
            .any(|p| p.starts_with(&prefix)))
    }
    pub(crate) fn new(directory: &Path) -> anyhow::Result<Self> {
        let state = State::new(directory.to_owned())?;
        let records: BTreeMap<String, Record> = if directory.join("files.json").exists() {
            state.read("files.json")?
        } else {
            BTreeMap::new()
        };
        for (path, r) in &records {
            valid_path(path)?;
            ensure!(
                !r.id.is_empty()
                    && r.id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
                "本地排除文件身份无效"
            );
            let m = std::fs::symlink_metadata(directory.join(format!("{}.data", r.id)))?;
            ensure!(
                m.is_file() && !m.is_symlink() && m.len() == r.size,
                "本地排除文件内容不完整"
            );
        }
        Ok(Self {
            state,
            records: Mutex::new(records),
        })
    }
    fn meta(path: &str, r: &Record) -> MetadataEntry {
        MetadataEntry::new_file(
            format!("local-excluded:{}", r.id),
            path,
            r.size,
            r.modified,
            &r.id,
        )
    }
    pub(crate) fn put(&self, path: &str, source: &Path) -> anyhow::Result<()> {
        valid_path(path)?;
        let mut records = self
            .records
            .lock()
            .map_err(|_| anyhow::anyhow!("本地文件索引不可用"))?;
        ensure!(
            records.len() < 100_000 || records.contains_key(path),
            "本地排除文件过多"
        );
        let id = random_secret();
        let destination = self.state.dir.join(format!("{id}.data"));
        let mut out = File::options()
            .write(true)
            .create_new(true)
            .open(&destination)?;
        let size = std::io::copy(&mut File::open(source)?, &mut out)?;
        out.sync_all()?;
        drop(out);
        let record = Record {
            id,
            size,
            modified: twodrive_core::now_unix(),
        };
        let mut next = records.clone();
        let old = next.insert(path.into(), record);
        self.state.save("files.json", &next)?;
        *records = next;
        if let Some(old) = old {
            let _ = std::fs::remove_file(self.state.dir.join(format!("{}.data", old.id)));
        }
        Ok(())
    }
    pub(crate) fn file(&self, path: &str) -> anyhow::Result<Option<(MetadataEntry, File)>> {
        let records = self
            .records
            .lock()
            .map_err(|_| anyhow::anyhow!("本地文件索引不可用"))?;
        let Some(r) = records.get(path) else {
            return Ok(None);
        };
        let file = File::open(self.state.dir.join(format!("{}.data", r.id)))?;
        ensure!(file.metadata()?.len() == r.size, "本地排除文件内容不完整");
        Ok(Some((Self::meta(path, r), file)))
    }
    pub(crate) fn entries(&self, parent: &str) -> anyhow::Result<Vec<MetadataEntry>> {
        Ok(self
            .records
            .lock()
            .map_err(|_| anyhow::anyhow!("本地文件索引不可用"))?
            .iter()
            .filter(|(path, _)| {
                path.rsplit_once('/')
                    .is_some_and(|(p, _)| if p.is_empty() { "/" } else { p } == parent)
            })
            .map(|(path, r)| Self::meta(path, r))
            .collect())
    }
    pub(crate) fn delete(&self, path: &str) -> anyhow::Result<bool> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| anyhow::anyhow!("本地文件索引不可用"))?;
        let mut next = records.clone();
        let Some(old) = next.remove(path) else {
            return Ok(false);
        };
        self.state.save("files.json", &next)?;
        *records = next;
        let _ = std::fs::remove_file(self.state.dir.join(format!("{}.data", old.id)));
        Ok(true)
    }
    pub(crate) fn rename(&self, from: &str, to: &str) -> anyhow::Result<bool> {
        valid_path(to)?;
        let mut records = self
            .records
            .lock()
            .map_err(|_| anyhow::anyhow!("本地文件索引不可用"))?;
        let mut next = records.clone();
        let Some(record) = next.remove(from) else {
            return Ok(false);
        };
        ensure!(!next.contains_key(to), "本地目标已存在");
        next.insert(to.into(), record);
        self.state
            .save("files.json", &next)
            .context("保存本地重命名")?;
        *records = next;
        Ok(true)
    }
}
