//! Immutable publication versions for an account-bound native connection.
//! Fresh cloud discovery cannot silently supply bytes for an older placeholder.
use crate::local_files::{LocalFileBridge, PublishedRead};
use anyhow::{Context, ensure};
use std::sync::OnceLock;
use std::{
    collections::HashMap,
    sync::{Mutex, atomic::AtomicBool},
};
use twodrive_core::FileRecord;
use twodrive_local_files::native::{HydrationRead, HydrationSource};
use twodrive_local_files::native::{
    NativeReadContext,
    persistent::{PersistentSession, ROOT_IDENTITY_PREFIX, RootReceipt},
};

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

/// Runtime source: native callback identity must match the durable publication
/// and physical binding before opening any provider-backed private-cache reader.
pub struct BoundPublishedSource {
    bridge: LocalFileBridge,
    root: OnceLock<RootReceipt>,
    download_gate: Mutex<()>,
}
impl BoundPublishedSource {
    pub fn unbound(bridge: LocalFileBridge) -> Self {
        Self {
            bridge,
            root: OnceLock::new(),
            download_gate: Mutex::new(()),
        }
    }
    pub fn from_receipt(bridge: LocalFileBridge, receipt: RootReceipt) -> anyhow::Result<Self> {
        let source = Self::unbound(bridge);
        source.bind_receipt(receipt)?;
        Ok(source)
    }
    pub fn bind_root(&self, owner: &PersistentSession) -> anyhow::Result<()> {
        self.bind_receipt(owner.receipt().clone())
    }
    fn bind_receipt(&self, receipt: RootReceipt) -> anyhow::Result<()> {
        ensure!(
            receipt.schema_version == 1
                && receipt.file_id != 0
                && receipt.identity.starts_with(ROOT_IDENTITY_PREFIX)
                && receipt.identity.len() > ROOT_IDENTITY_PREFIX.len()
                && receipt.identity.len() <= 4096,
            "invalid native source root receipt"
        );
        std::str::from_utf8(&receipt.identity)?;
        if let Some(current) = self.root.get() {
            ensure!(current == &receipt, "native source cannot switch roots");
            return Ok(());
        }
        self.root
            .set(receipt)
            .map_err(|_| anyhow::anyhow!("native source root binding raced"))?;
        Ok(())
    }
    fn publication(
        &self,
        tag: &[u8],
    ) -> anyhow::Result<(twodrive_core::NativeProjection, FileRecord)> {
        let root = self.root.get().context("native source root is not bound")?;
        let root_binding = std::str::from_utf8(&root.identity)?;
        let projection = self
            .bridge
            .journal()
            .native_projection(root_binding, tag)?
            .context("unknown native publication tag")?;
        ensure!(
            projection.state == "published" && !projection.directory,
            "native publication is not ready for content"
        );
        let record = self
            .bridge
            .journal()
            .database()
            .get_by_remote_id(&projection.local_id)?
            .context("native publication no longer exists")?;
        ensure!(
            record.cloud_remote_id.as_deref() == Some(projection.cloud_id.as_str())
                && record.metadata.path == projection.path
                && record.metadata.etag == projection.etag
                && record.metadata.size == projection.size
                && record.metadata.modified_unix == projection.modified_unix
                && !record.metadata.is_dir
                && !self.bridge.journal().native_dirty(&projection.local_id)?,
            "native publication version needs reconciliation"
        );
        Ok((projection, record))
    }
}
impl HydrationSource for BoundPublishedSource {
    fn size(&self, identity: &[u8]) -> Result<usize, String> {
        self.publication(identity)
            .and_then(|(p, _)| Ok(usize::try_from(p.size)?))
            .map_err(|e| e.to_string())
    }
    fn begin_native_read(
        &self,
        identity: &[u8],
        context: NativeReadContext<'_>,
        cancelled: &AtomicBool,
    ) -> Result<Option<Box<dyn HydrationRead>>, String> {
        (|| -> anyhow::Result<_> {
            let root = self.root.get().context("native source root is not bound")?;
            let (projection, record) = self.publication(identity)?;
            ensure!(
                context.root_identity == root.identity.as_slice()
                    && context.volume_serial == root.volume_serial
                    && context.root_file_id == root.file_id
                    && projection.volume_serial == Some(context.volume_serial)
                    && projection.native_file_id.as_deref()
                        == Some(format!("{:016x}", context.file_id).as_str())
                    && context.file_size == projection.size,
                "native fetch physical binding mismatch"
            );
            let _guard = self
                .download_gate
                .lock()
                .map_err(|_| anyhow::anyhow!("native download gate poisoned"))?;
            let read = self.bridge.open_published_read(&record, cancelled)?;
            Ok(Some(Box::new(read) as Box<dyn HydrationRead>))
        })()
        .map_err(|e| e.to_string())
    }
    fn read_range(&self, _: &[u8], _: usize, _: usize, _: &AtomicBool) -> Result<Vec<u8>, String> {
        Err("native physical request context required".into())
    }
}

#[cfg(all(test, feature = "native-local-fixture"))]
mod tests {
    use super::*;
    use crate::{model::CloudBackend, native_local_fixture::GeneratedProvider, provider::Backend};
    use std::sync::Arc;
    use twodrive_local_files::fixtures::{Dataset, DownloadCounters};
    #[test]
    #[ignore = "explicit NEW generated CFAPI root; physical callback binding, dehydration and resume"]
    #[cfg(feature = "native-test-hooks")]
    fn bound_source_hydrates_only_the_published_physical_file_after_restart() {
        use std::{
            fs,
            path::PathBuf,
            time::{Duration, Instant},
        };
        use twodrive_local_files::native::with_exclusive_placeholder;
        let root = PathBuf::from(
            std::env::var_os("TWODRIVE_APPROVED_BOUND_SOURCE_ROOT")
                .expect("explicit new generated root"),
        );
        assert!(
            root.is_absolute()
                && !root.exists()
                && root
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("twodrive-cfapi-fixture-office-")
        );
        let checkpoint = root.with_extension("receipt.json");
        let state = tempfile::tempdir().unwrap();
        let private = state.path().canonicalize().unwrap();
        let cache = private.join("cache");
        fs::create_dir(&cache).unwrap();
        let db = twodrive_core::Database::new(private.join("native.sqlite3"));
        let dataset = Dataset::StandardV1;
        let backend = Backend::new(GeneratedProvider {
            dataset,
            counters: Arc::new(DownloadCounters::default()),
        });
        let metadata = backend
            .list_all()
            .unwrap()
            .into_iter()
            .find(|m| !m.is_dir)
            .unwrap();
        let bridge = LocalFileBridge::open_bound(
            db.clone(),
            &cache,
            backend,
            "generated-native-real-context",
        )
        .unwrap();
        db.upsert_metadata(&metadata).unwrap();
        let source = Arc::new(BoundPublishedSource::unbound(bridge.clone()));
        let mut owner = PersistentSession::create_new(
            &root,
            b"twodrive-dev-native-v1:generated-office-only",
            &checkpoint,
            source.clone(),
        )
        .unwrap();
        source.bind_root(&owner).unwrap();
        let receipt = owner.receipt().clone();
        let binding = std::str::from_utf8(&receipt.identity).unwrap();
        let tag = b"generated-root-private-publication";
        bridge
            .journal()
            .prepare_native_projection(&metadata.remote_id, binding, tag)
            .unwrap();
        let relative = PathBuf::from(metadata.path.trim_start_matches('/'));
        let file = owner
            .publish_placeholder(&relative, tag, metadata.size, false, metadata.modified_unix)
            .unwrap();
        let mut renamed_while_bound = false;
        owner
            .with_publication_binding(&relative, tag, metadata.size, false, |volume, file_id| {
                let moved = file.with_extension("unexpected-pub-rename");
                if fs::rename(&file, &moved).is_ok() {
                    renamed_while_bound = true;
                    fs::rename(&moved, &file).map_err(|e| e.to_string())?;
                }
                bridge
                    .journal()
                    .complete_native_projection(&metadata.remote_id, binding, tag, volume, file_id)
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        assert_eq!(fs::read(&file).unwrap(), dataset.bytes(0));
        let duplicate = owner
            .publish_placeholder(
                std::path::Path::new("unpublished-duplicate.bin"),
                tag,
                metadata.size,
                false,
                metadata.modified_unix,
            )
            .unwrap();
        assert!(
            fs::read(&duplicate).is_err(),
            "unpublished physical duplicate received data"
        );
        fs::remove_file(duplicate).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while bridge.status(&metadata.remote_id).unwrap().readers != 0 && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        let generation = bridge.status(&metadata.remote_id).unwrap().generation;
        bridge
            .journal()
            .with_clean_cache(&metadata.remote_id, generation, |cache, expected| {
                ensure!(
                    crate::local_files::digest_file(cache)? == expected,
                    "generated private cache changed"
                );
                with_exclusive_placeholder(&file, tag, metadata.size as usize, |locked| {
                    locked.dehydrate()
                })
                .map_err(anyhow::Error::msg)
            })
            .unwrap()
            .expect("generated clean lease required");
        owner.disconnect().unwrap();
        drop(owner);
        drop(source);
        drop(bridge);
        let backend = Backend::new(GeneratedProvider {
            dataset,
            counters: Arc::new(DownloadCounters::default()),
        });
        let bridge =
            LocalFileBridge::open_bound(db, &cache, backend, "generated-native-real-context")
                .unwrap();
        let source =
            Arc::new(BoundPublishedSource::from_receipt(bridge.clone(), receipt.clone()).unwrap());
        let mut owner = PersistentSession::resume(&root, &receipt, source.clone()).unwrap();
        assert_eq!(fs::read(&file).unwrap(), dataset.bytes(0));
        fs::remove_file(file).unwrap();
        owner.retire_empty_office_fixture().unwrap();
        drop(owner);
        drop(source);
        drop(bridge);
        fs::remove_dir(root).unwrap();
        fs::remove_file(checkpoint).unwrap();
        state.close().unwrap();
        assert!(
            !renamed_while_bound,
            "publication file lease permitted replacement"
        );
    }
    #[test]
    fn callback_context_requires_published_tag_root_volume_file_id_and_size() {
        let temp = tempfile::tempdir().unwrap();
        let private = temp.path().canonicalize().unwrap();
        let cache = private.join("cache");
        std::fs::create_dir(&cache).unwrap();
        let db = twodrive_core::Database::new(private.join("native.sqlite3"));
        let dataset = Dataset::StandardV1;
        let backend = Backend::new(GeneratedProvider {
            dataset,
            counters: Arc::new(DownloadCounters::default()),
        });
        let metadata = backend
            .list_all()
            .unwrap()
            .into_iter()
            .find(|m| !m.is_dir)
            .unwrap();
        let bridge = LocalFileBridge::open_bound(
            db.clone(),
            &cache,
            backend,
            "generated-native-context-account",
        )
        .unwrap();
        db.upsert_metadata(&metadata).unwrap();
        let receipt = RootReceipt {
            schema_version: 1,
            volume_serial: 7,
            file_id: 99,
            identity: b"twodrive-dev-native-v1:generated-context-only".to_vec(),
        };
        let root_binding = std::str::from_utf8(&receipt.identity).unwrap();
        let tag = b"generated-unique-private-publication";
        bridge
            .journal()
            .prepare_native_projection(&metadata.remote_id, root_binding, tag)
            .unwrap();
        let source = BoundPublishedSource::from_receipt(bridge.clone(), receipt.clone()).unwrap();
        let context = NativeReadContext {
            root_identity: &receipt.identity,
            volume_serial: 7,
            root_file_id: 99,
            file_id: 123,
            file_size: metadata.size,
        };
        let cancel = AtomicBool::new(false);
        assert!(source.begin_native_read(tag, context, &cancel).is_err());
        bridge
            .journal()
            .complete_native_projection(&metadata.remote_id, root_binding, tag, 7, 123)
            .unwrap();
        assert!(source.size(metadata.remote_id.as_bytes()).is_err());
        for wrong in [
            NativeReadContext {
                root_identity: b"foreign-root-identity",
                ..context
            },
            NativeReadContext {
                root_file_id: 100,
                ..context
            },
            NativeReadContext {
                volume_serial: 8,
                ..context
            },
            NativeReadContext {
                file_id: 124,
                ..context
            },
            NativeReadContext {
                file_size: context.file_size + 1,
                ..context
            },
        ] {
            assert!(source.begin_native_read(tag, wrong, &cancel).is_err());
            assert!(
                db.get_by_remote_id(&metadata.remote_id)
                    .unwrap()
                    .unwrap()
                    .cache_path
                    .is_none()
            );
        }
        assert_eq!(source.size(tag).unwrap(), metadata.size as usize);
        let mut read = source
            .begin_native_read(tag, context, &cancel)
            .unwrap()
            .unwrap();
        assert_eq!(
            read.read_range(0, 16, &cancel).unwrap(),
            dataset.bytes(0)[..16]
        );
        drop(read);
        assert!(source.read_range(tag, 0, 16, &cancel).is_err());
        let wrong_root = RootReceipt {
            file_id: 100,
            ..receipt
        };
        assert!(source.bind_receipt(wrong_root).is_err());
        drop(source);
        drop(bridge);
    }
}
