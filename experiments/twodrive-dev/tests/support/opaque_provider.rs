//! Disposable non-DAV provider. IDs intentionally cannot be mistaken for paths.
use anyhow::{Context, ensure};
use std::{
    collections::BTreeMap,
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
};
use twodrive_dev::{
    model::{CloudBackend, DeltaResult, MetadataEntry},
    provider::{Capabilities, Progress, Provider},
};

type Files = BTreeMap<String, (MetadataEntry, Vec<u8>)>;

#[derive(Clone)]
pub struct OpaqueProvider {
    files: Arc<Mutex<Files>>,
    pub caps: Capabilities,
    pub download_gate: Option<Arc<std::sync::atomic::AtomicBool>>,
}
impl Default for OpaqueProvider {
    fn default() -> Self {
        Self {
            files: Arc::default(),
            download_gate: None,
            caps: Capabilities {
                upload: true,
                mkdir: true,
                rename: true,
                delete: true,
                conditional_upload: true,
            },
        }
    }
}
impl OpaqueProvider {
    pub fn with_caps(mut self, caps: Capabilities) -> Self {
        self.caps = caps;
        self
    }
    fn put(
        &self,
        path: &str,
        bytes: Vec<u8>,
        version: Option<&str>,
    ) -> anyhow::Result<MetadataEntry> {
        let mut files = self.files.lock().unwrap();
        let old = files
            .values()
            .find(|(m, _)| m.path == path)
            .map(|(m, _)| m.clone());
        ensure!(
            match (&old, version) {
                (None, None) => true,
                (Some(m), Some(v)) => m.etag == v,
                _ => false,
            },
            "version conflict"
        );
        let id = old
            .as_ref()
            .map(|m| m.remote_id.clone())
            .unwrap_or_else(|| format!("object-{}", files.len() + 1));
        let generation = old
            .as_ref()
            .and_then(|m| m.etag.parse::<u64>().ok())
            .unwrap_or(0)
            + 1;
        let m = MetadataEntry::new_file(&id, path, bytes.len() as u64, 42, generation.to_string());
        files.insert(id, (m.clone(), bytes));
        Ok(m)
    }
    fn by_id(&self, id: &str) -> anyhow::Result<(MetadataEntry, Vec<u8>)> {
        ensure!(
            !id.starts_with('/'),
            "a path was incorrectly used as a remote ID"
        );
        self.files
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .context("missing object ID")
    }
}
impl CloudBackend for OpaqueProvider {
    fn is_conflict_error(&self, e: &anyhow::Error) -> bool {
        e.to_string().contains("version conflict")
    }
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        Ok(self
            .files
            .lock()
            .unwrap()
            .values()
            .map(|(m, _)| m.clone())
            .collect())
    }
    fn list_delta(&self, _: Option<&str>) -> anyhow::Result<DeltaResult> {
        Ok(DeltaResult {
            entries: self.list_all()?,
            deleted_remote_ids: vec![],
            delta_link: None,
        })
    }
    fn get_metadata(&self, id: &str) -> anyhow::Result<Option<MetadataEntry>> {
        Ok(self.files.lock().unwrap().get(id).map(|(m, _)| m.clone()))
    }
    fn get_metadata_by_path(&self, path: &str) -> anyhow::Result<Option<MetadataEntry>> {
        if path == "/" {
            return Ok(Some(MetadataEntry::new_dir("opaque-root", "/", 42, "1")));
        }
        Ok(self.list_all()?.into_iter().find(|m| m.path == path))
    }
    fn download(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        Ok(self.by_id(id)?.1)
    }
    fn download_to(
        &self,
        id: &str,
        w: &mut dyn Write,
        progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        while self
            .download_gate
            .as_ref()
            .is_some_and(|gate| !gate.load(std::sync::atomic::Ordering::Relaxed))
        {
            progress(0)?;
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let data = self.download(id)?;
        w.write_all(&data)?;
        progress(data.len() as u64)?;
        Ok(data.len() as u64)
    }
    fn upload(&self, path: &str, data: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        self.put(path, data, None)
    }
    fn upload_with_etag(
        &self,
        path: &str,
        data: Vec<u8>,
        version: Option<&str>,
    ) -> anyhow::Result<MetadataEntry> {
        self.put(path, data, version)
    }
    fn upload_file_with_version(
        &self,
        path: &str,
        source: &Path,
        id: Option<&str>,
        version: Option<&str>,
        progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        if let Some(id) = id {
            ensure!(self.by_id(id)?.0.path == path, "wrong upload ID");
        }
        let bytes = std::fs::read(source)?;
        progress(0, bytes.len() as u64)?;
        let entry = self.put(path, bytes, version)?;
        progress(entry.size, entry.size)?;
        Ok(entry)
    }
    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        let mut files = self.files.lock().unwrap();
        let id = format!("object-{}", files.len() + 1);
        let m = MetadataEntry::new_dir(&id, path, 42, "1");
        files.insert(id, (m.clone(), vec![]));
        Ok(m)
    }
    fn rename(&self, id: &str, path: &str) -> anyhow::Result<MetadataEntry> {
        let (old, bytes) = self.by_id(id)?;
        let m = if old.is_dir {
            MetadataEntry::new_dir(id, path, 42, "2")
        } else {
            MetadataEntry::new_file(id, path, old.size, 42, "2")
        };
        self.files
            .lock()
            .unwrap()
            .insert(id.into(), (m.clone(), bytes));
        Ok(m)
    }
    fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.by_id(id)?;
        self.files.lock().unwrap().remove(id);
        Ok(())
    }
}
impl Provider for OpaqueProvider {
    fn capabilities(&self) -> Capabilities {
        self.caps
    }
    fn list_directory(&self, path: &str) -> anyhow::Result<Vec<MetadataEntry>> {
        Ok(self
            .list_all()?
            .into_iter()
            .filter(|m| m.parent_path == path)
            .collect())
    }
    fn upload_controlled(
        &self,
        path: &str,
        source: &Path,
        id: Option<&str>,
        version: Option<&str>,
        progress: Progress,
    ) -> anyhow::Result<MetadataEntry> {
        self.upload_file_with_version(path, source, id, version, &mut |done, total| {
            progress(done, total)
        })
    }
}
