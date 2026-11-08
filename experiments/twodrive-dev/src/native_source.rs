//! Immutable publication versions for an account-bound native connection.
//! Fresh cloud discovery cannot silently supply bytes for an older placeholder.
use crate::local_files::{LocalFileBridge, PublishedRead};
use anyhow::{Context, ensure};
use std::{
    collections::HashMap,
    sync::{Mutex, atomic::AtomicBool},
};
use twodrive_core::FileRecord;
use twodrive_local_files::native::{HydrationRead, HydrationSource};

pub struct PublishedSource {
    bridge: LocalFileBridge,
    versions: HashMap<Vec<u8>, FileRecord>,
    download_gate: Mutex<()>,
}
impl PublishedSource {
    pub fn new(bridge: LocalFileBridge, versions: Vec<FileRecord>) -> anyhow::Result<Self> {
        let mut map = HashMap::new();
        for record in versions {
            ensure!(
                !record.metadata.is_dir
                    && !record.metadata.remote_id.is_empty()
                    && !record.metadata.etag.is_empty()
                    && record.cloud_remote_id.is_some(),
                "versioned file publications required"
            );
            ensure!(
                map.insert(record.metadata.remote_id.as_bytes().to_vec(), record)
                    .is_none(),
                "duplicate publication identity"
            );
        }
        Ok(Self {
            bridge,
            versions: map,
            download_gate: Mutex::new(()),
        })
    }
    fn version(&self, id: &[u8]) -> anyhow::Result<&FileRecord> {
        self.versions
            .get(id)
            .context("unknown native publication identity")
    }
    fn open(&self, identity: &[u8], cancelled: &AtomicBool) -> anyhow::Result<PublishedRead> {
        let expected = self.version(identity)?;
        let _guard = self
            .download_gate
            .lock()
            .map_err(|_| anyhow::anyhow!("native download gate poisoned"))?;
        self.bridge.open_published_read(expected, cancelled)
    }
}
impl HydrationRead for PublishedRead {
    fn read_range(
        &mut self,
        offset: usize,
        length: usize,
        cancelled: &AtomicBool,
    ) -> Result<Vec<u8>, String> {
        PublishedRead::read_range(self, offset, length, cancelled).map_err(|e| e.to_string())
    }
}
impl HydrationSource for PublishedSource {
    fn size(&self, identity: &[u8]) -> Result<usize, String> {
        usize::try_from(
            self.version(identity)
                .map_err(|e| e.to_string())?
                .metadata
                .size,
        )
        .map_err(|_| "native file too large".into())
    }
    fn begin_read(
        &self,
        identity: &[u8],
        cancelled: &AtomicBool,
    ) -> Result<Option<Box<dyn HydrationRead>>, String> {
        self.open(identity, cancelled)
            .map(|read| Some(Box::new(read) as Box<dyn HydrationRead>))
            .map_err(|e| e.to_string())
    }
    fn read_range(
        &self,
        identity: &[u8],
        offset: usize,
        length: usize,
        cancelled: &AtomicBool,
    ) -> Result<Vec<u8>, String> {
        self.open(identity, cancelled)
            .and_then(|mut read| read.read_range(offset, length, cancelled))
            .map_err(|e| e.to_string())
    }
}
