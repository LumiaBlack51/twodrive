use crate::CloudBackend;
use std::{collections::HashMap, sync::Mutex};
use twodrive_core::{MetadataEntry, normalize_cloud_path, now_unix};
#[derive(Debug)]
pub struct MockBackend {
    entries: Mutex<Vec<MetadataEntry>>,
    files: Mutex<HashMap<String, Vec<u8>>>,
}

impl MockBackend {
    pub fn new() -> Self {
        let now = 1_719_000_000;
        let samples = vec![
            (
                "file-readme",
                "/README-cloud.txt",
                b"Welcome to the mock OneDrive tree.\nThis file is downloaded only when opened through FUSE.\n".to_vec(),
            ),
            (
                "file-lecture-01",
                "/Courses/Econometrics/Lecture_01.txt",
                b"Econometrics Lecture 01\n\n1. What is an estimator?\n2. Bias and variance\n3. Ordinary least squares\n".to_vec(),
            ),
            (
                "file-syllabus",
                "/Courses/Econometrics/Syllabus.md",
                b"# Econometrics Syllabus\n\n- Week 1: Linear models\n- Week 2: Inference\n- Week 3: Panel data\n".to_vec(),
            ),
            (
                "file-notes",
                "/Documents/twodrive-notes.txt",
                b"twodrive read-only MVP notes:\n- metadata first\n- hydrate on open\n- cache second reads\n".to_vec(),
            ),
        ];

        let mut files = HashMap::new();
        let mut entries = vec![
            MetadataEntry::new_dir("dir-courses", "/Courses", now, "etag-dir-courses"),
            MetadataEntry::new_dir(
                "dir-econometrics",
                "/Courses/Econometrics",
                now,
                "etag-dir-econometrics",
            ),
            MetadataEntry::new_dir("dir-documents", "/Documents", now, "etag-dir-documents"),
        ];

        for (remote_id, path, content) in samples {
            entries.push(MetadataEntry::new_file(
                remote_id,
                path,
                content.len() as u64,
                now,
                format!("etag-{remote_id}"),
            ));
            files.insert(remote_id.to_string(), content);
        }

        Self {
            entries: Mutex::new(entries),
            files: Mutex::new(files),
        }
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CloudBackend for MockBackend {
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        Ok(self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("mock entries lock is poisoned"))?
            .clone())
    }

    fn download(&self, remote_id: &str) -> anyhow::Result<Vec<u8>> {
        self.files
            .lock()
            .map_err(|_| anyhow::anyhow!("mock files lock is poisoned"))?
            .get(remote_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("mock backend has no file with remote id {remote_id}"))
    }

    fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        self.upload_with_etag(path, content, None)
    }

    fn upload_with_etag(
        &self,
        path: &str,
        content: Vec<u8>,
        if_match: Option<&str>,
    ) -> anyhow::Result<MetadataEntry> {
        let path = normalize_cloud_path(path);
        let remote_id = format!("mock-upload-{}", sanitize_remote_component(&path));
        if let Some(if_match) = if_match {
            let entries = self
                .entries
                .lock()
                .map_err(|_| anyhow::anyhow!("mock entries lock is poisoned"))?;
            if let Some(existing) = entries.iter().find(|entry| entry.path == path)
                && existing.etag != if_match
            {
                anyhow::bail!("Graph request failed with HTTP 412 Precondition Failed");
            }
        }
        let entry = MetadataEntry::new_file(
            remote_id.clone(),
            path.clone(),
            content.len() as u64,
            now_unix(),
            format!("etag-{remote_id}"),
        );

        self.files
            .lock()
            .map_err(|_| anyhow::anyhow!("mock files lock is poisoned"))?
            .insert(remote_id, content);
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("mock entries lock is poisoned"))?;
        entries.retain(|existing| existing.path != path);
        entries.push(entry.clone());
        Ok(entry)
    }

    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        let path = normalize_cloud_path(path);
        let remote_id = format!("mock-folder-{}", sanitize_remote_component(&path));
        let entry = MetadataEntry::new_dir(
            remote_id,
            path.clone(),
            now_unix(),
            format!("etag-{}", sanitize_remote_component(&path)),
        );
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("mock entries lock is poisoned"))?;
        entries.retain(|existing| existing.path != path);
        entries.push(entry.clone());
        Ok(entry)
    }

    fn rename(&self, remote_id: &str, new_path: &str) -> anyhow::Result<MetadataEntry> {
        let new_path = normalize_cloud_path(new_path);
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("mock entries lock is poisoned"))?;
        let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry.remote_id == remote_id)
        else {
            anyhow::bail!("mock backend has no item with remote id {remote_id}");
        };
        let updated = if entry.is_dir {
            MetadataEntry::new_dir(
                entry.remote_id.clone(),
                new_path,
                now_unix(),
                format!("etag-{}", entry.remote_id),
            )
        } else {
            MetadataEntry::new_file(
                entry.remote_id.clone(),
                new_path,
                entry.size,
                now_unix(),
                format!("etag-{}", entry.remote_id),
            )
        };
        *entry = updated.clone();
        Ok(updated)
    }

    fn delete(&self, remote_id: &str) -> anyhow::Result<()> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("mock entries lock is poisoned"))?;
        entries.retain(|entry| entry.remote_id != remote_id);
        self.files
            .lock()
            .map_err(|_| anyhow::anyhow!("mock files lock is poisoned"))?
            .remove(remote_id);
        Ok(())
    }
}

fn sanitize_remote_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}
