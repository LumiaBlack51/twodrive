//! Durable, provider-neutral cache leases for the future native filesystem.
//!
//! This opt-in owner never registers a root or changes legacy FUSE ownership.
//! A SQLite write intent precedes writable access. OS file locks, the owner lock,
//! generation CAS and uncertain-upload recovery are separate requirements.
use super::Database;
use crate::{FileRecord, FileState, MetadataEntry};
use anyhow::{Context, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct CacheJournal(Arc<Owner>);
struct Owner {
    db: Database,
    root: PathBuf,
    epoch: i64,
    gate: Mutex<()>,
    _lock: File,
    _directories: Vec<File>,
}

#[derive(Debug, Clone)]
pub struct CacheSnapshot {
    /// Visible native bytes differ from the private cache. No upload/recovery
    /// may treat the private copy as authoritative while this durable veto exists.
    pub native_dirty: bool,
    pub generation: i64,
    pub acknowledged_generation: i64,
    pub readers: i64,
    pub writers: i64,
    pub upload_generation: Option<i64>,
    pub recovery_required: bool,
    pub uncertain_upload: bool,
    pub dirty_intent: bool,
    pub pending_operations: bool,
    pub state: FileState,
    pub pinned: bool,
    pub resident: bool,
    base_etag: String,
    acknowledged_digest: String,
    upload_digest: Option<String>,
}

impl CacheSnapshot {
    pub fn sync_confirmed(&self) -> bool {
        matches!(self.state, FileState::Cached | FileState::Pinned)
            && self.generation == self.acknowledged_generation
            && self.writers == 0
            && self.upload_generation.is_none()
            && !self.dirty_intent
            && !self.recovery_required
            && !self.uncertain_upload
            && !self.pending_operations
            && !self.native_dirty
    }
    /// Engine eligibility only. A native adapter must additionally hold/recheck
    /// its CFAPI oplock, residency, modified ranges, pin and in-sync evidence.
    pub fn engine_release_ready(&self) -> bool {
        self.state == FileState::Cached
            && self.resident
            && !self.pinned
            && self.generation == self.acknowledged_generation
            && self.readers == 0
            && self.writers == 0
            && self.upload_generation.is_none()
            && !self.dirty_intent
            && !self.recovery_required
            && !self.uncertain_upload
            && !self.pending_operations
            && !self.native_dirty
    }
}

impl CacheJournal {
    /// Open only a dedicated private cache. The lifetime lock prevents a second
    /// process from resetting live handle counters. Leases retain this owner.
    pub fn open(db: Database, cache_root: &Path, binding_digest: &str) -> anyhow::Result<Self> {
        valid_digest(binding_digest)?;
        ensure!(cache_root.is_absolute(), "private cache must be absolute");
        ensure!(db.path().is_absolute(), "private database must be absolute");
        let mut directories = guard_directories(cache_root)?;
        directories.extend(guard_directories(
            db.path().parent().context("missing state parent")?,
        )?);
        let root = cache_root.canonicalize()?;
        let lock_path = db.path().with_extension("local-cache.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(3).custom_flags(0x00200000);
        }
        let owner_lock = options.open(lock_path)?;
        reject_reparse(&owner_lock.metadata()?)?;
        owner_lock
            .try_lock()
            .context("private cache already has a live owner")?;
        directories.push(guard_database(db.path())?);
        db.init()?;
        let mut conn = journal_connection(&db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let schema: Option<String> = tx
            .query_row(
                "SELECT value FROM app_state WHERE key='local_cache_schema'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let existing:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='local_cache_journal')",[],|r|r.get(0))?;
        ensure!(
            schema.as_deref().is_none_or(|value| value == "2")
                && (!existing || schema.as_deref() == Some("2")),
            "unsupported private-cache journal schema"
        );
        for (key, expected) in [
            ("local_cache_binding", binding_digest.to_owned()),
            ("local_cache_root", root.to_string_lossy().into_owned()),
        ] {
            let previous: Option<String> = tx
                .query_row("SELECT value FROM app_state WHERE key=?1", [key], |r| {
                    r.get(0)
                })
                .optional()?;
            ensure!(
                previous.as_deref().is_none_or(|value| value == expected)
                    && (!existing || previous.is_some()),
                "private cache belongs to another provider identity or root"
            );
            tx.execute(
                "INSERT INTO app_state(key,value) VALUES(?1,?2) ON CONFLICT(key) DO NOTHING",
                params![key, expected],
            )?;
        }
        tx.execute_batch("CREATE TABLE IF NOT EXISTS local_cache_journal (
            local_id TEXT PRIMARY KEY, generation INTEGER NOT NULL,
            acknowledged_generation INTEGER NOT NULL, readers INTEGER NOT NULL DEFAULT 0,
            writers INTEGER NOT NULL DEFAULT 0, dirty_intent INTEGER NOT NULL DEFAULT 0,
            recovery_required INTEGER NOT NULL DEFAULT 0, uncertain_upload INTEGER NOT NULL DEFAULT 0,
            upload_generation INTEGER, upload_epoch INTEGER, upload_digest TEXT,
            base_etag TEXT NOT NULL, acknowledged_digest TEXT NOT NULL,
            cache_path TEXT NOT NULL, cloud_id TEXT NOT NULL, cloud_path TEXT NOT NULL);")?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS local_native_dirty (cloud_id TEXT PRIMARY KEY);",
        )?;
        tx.execute("INSERT INTO app_state(key,value) VALUES('local_cache_schema','2') ON CONFLICT(key) DO NOTHING",[])?;
        let old: Option<String> = tx
            .query_row(
                "SELECT value FROM app_state WHERE key='local_cache_epoch'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let epoch = old
            .map(|v| v.parse::<i64>())
            .transpose()?
            .unwrap_or(0)
            .checked_add(1)
            .context("cache epoch exhausted")?;
        tx.execute("INSERT INTO app_state(key,value) VALUES('local_cache_epoch',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [epoch.to_string()])?;
        // The exclusive owner lock proves old leases are no longer live. A
        // vanished process does not prove its write or remote upload completed.
        tx.execute("UPDATE files SET state='dirty' WHERE remote_id IN (SELECT local_id FROM local_cache_journal WHERE writers>0)", [])?;
        tx.execute("UPDATE local_cache_journal SET recovery_required=CASE WHEN writers>0 THEN 1 ELSE recovery_required END,
            dirty_intent=CASE WHEN writers>0 THEN 1 ELSE dirty_intent END,
            uncertain_upload=CASE WHEN upload_generation IS NOT NULL THEN 1 ELSE uncertain_upload END,
            readers=0,writers=0", [])?;
        tx.commit()?;
        Ok(Self(Arc::new(Owner {
            db,
            root,
            epoch,
            gate: Mutex::new(()),
            _lock: owner_lock,
            _directories: directories,
        })))
    }

    pub fn database(&self) -> &Database {
        &self.0.db
    }
    pub fn cache_root(&self) -> &Path {
        &self.0.root
    }

    /// Refuse an already owned cache before provider metadata can invalidate it.
    /// The owner gate also serializes the initial claim against publication.
    pub fn begin_download(&self, remote: &MetadataEntry) -> anyhow::Result<(String, i64)> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        ensure!(
            !self.native_dirty(&remote.remote_id)?,
            "native bytes require explicit recovery"
        );
        for record in [
            self.0.db.get_by_cloud_remote_id(&remote.remote_id)?,
            self.0.db.get_by_path(&remote.path)?,
        ]
        .into_iter()
        .flatten()
        {
            ensure!(
                !has_journal_record(&journal_connection(&self.0.db)?, &record.metadata.remote_id)?
                    && record.cache_path.is_none()
                    && record.state == FileState::OnlineOnly,
                "existing cache must be recovered, not replaced"
            );
        }
        self.0.db.upsert_metadata(remote)?;
        let record = self
            .0
            .db
            .get_by_cloud_remote_id(&remote.remote_id)?
            .context("local identity missing")?;
        ensure!(
            record.metadata.etag == remote.etag
                && record.metadata.path == remote.path
                && record.metadata.size == remote.size
                && record.state == FileState::OnlineOnly
                && record.cache_path.is_none(),
            "download metadata or ownership changed"
        );
        let id = record.metadata.remote_id;
        let generation = self.0.db.download_generation(&id)?;
        ensure!(
            self.0.db.begin_hydration(&id, generation)?,
            "download claim refused"
        );
        Ok((id, generation))
    }

    fn record(&self, id: &str) -> anyhow::Result<FileRecord> {
        self.0
            .db
            .get_by_remote_id(id)?
            .context("unknown cache identity")
    }

    fn file(&self, record: &FileRecord, writable: bool) -> anyhow::Result<File> {
        let path = record
            .cache_path
            .as_deref()
            .context("cache is not resident")?;
        ensure!(
            path.parent() == Some(self.0.root.as_path()),
            "cache must be a direct private child"
        );
        reject_reparse(&fs::symlink_metadata(path)?)?;
        let mut options = OpenOptions::new();
        options.read(true).write(writable);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x00200000).share_mode(3);
        }
        let file = options.open(path)?;
        ensure!(file.metadata()?.is_file(), "cache must be a regular file");
        reject_reparse(&file.metadata()?)?;
        Ok(file)
    }

    /// Atomically publish a fsynced, newly generated download and its journal.
    /// The unique private file exists before commit; a crash can leave an orphan,
    /// never a falsely acknowledged writable item. No existing path is replaced.
    pub fn publish_download(
        &self,
        id: &str,
        download_generation: i64,
        path: &Path,
        digest: &str,
        etag: &str,
    ) -> anyhow::Result<()> {
        valid_digest(digest)?;
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut record = self.record(id)?;
        ensure!(
            record.cache_path.is_none()
                && record.cloud_remote_id.is_some()
                && record.metadata.etag == etag,
            "download metadata changed"
        );
        record.cache_path = Some(path.to_owned());
        let file = self.file(&record, true)?;
        file.try_lock()?;
        file.sync_all()?;
        ensure!(
            file.metadata()?.len() == record.metadata.size,
            "download size changed"
        );
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(tx.execute("UPDATE files SET state=CASE WHEN pin_explicit=1 OR pin_origin_remote_id IS NOT NULL THEN 'pinned' ELSE 'cached' END,cache_path=?2
            WHERE remote_id=?1 AND download_generation=?3 AND etag=?4 AND cache_path IS NULL AND release_pending=0 AND state IN ('online_only','hydrating','pinned')
            AND NOT EXISTS(SELECT 1 FROM pending_deletes WHERE remote_id=?1 OR local_id=?1 OR path=?5)
            AND NOT EXISTS(SELECT 1 FROM pending_metadata_operations op JOIN files parent ON parent.remote_id=op.local_id WHERE ?5=parent.path OR substr(?5,1,length(parent.path)+1)=parent.path||'/')",
            params![id,path.to_string_lossy(),download_generation,etag,record.metadata.path])?==1,"stale download generation");
        tx.execute("INSERT INTO local_cache_journal(local_id,generation,acknowledged_generation,base_etag,acknowledged_digest,cache_path,cloud_id,cloud_path) VALUES(?1,0,0,?2,?3,?4,?5,?6)",params![id,etag,digest,path.to_string_lossy(),record.cloud_remote_id,record.metadata.path])?;
        tx.commit()?;
        Ok(())
    }

    pub fn snapshot(&self, id: &str) -> anyhow::Result<CacheSnapshot> {
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction()?;
        let result = snapshot(&tx, id)?;
        tx.commit()?;
        Ok(result)
    }

    /// Record a full-byte mismatch observed under with_clean_cache. A later
    /// writer wins its generation CAS; this can never acknowledge any bytes.
    pub fn record_external_edit(&self, id: &str, generation: i64) -> anyhow::Result<bool> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = snapshot(&tx, id)?;
        if state.generation != generation || !state.engine_release_ready() {
            return Ok(false);
        }
        let next = generation
            .checked_add(1)
            .context("cache generation exhausted")?;
        tx.execute(
            "UPDATE local_cache_journal SET generation=?2,dirty_intent=1 WHERE local_id=?1",
            params![id, next],
        )?;
        tx.execute("UPDATE files SET state='dirty' WHERE remote_id=?1", [id])?;
        tx.commit()?;
        Ok(true)
    }

    pub fn native_dirty(&self, cloud_id: &str) -> anyhow::Result<bool> {
        Ok(journal_connection(&self.0.db)?.query_row(
            "SELECT EXISTS(SELECT 1 FROM local_native_dirty WHERE cloud_id=?1)",
            [cloud_id],
            |r| r.get(0),
        )?)
    }

    /// Persist an OS-observed edit, including a truncated placeholder that never
    /// needed hydration. Only a verified native snapshot may clear this veto.
    pub fn record_native_edit(&self, cloud_id: &str) -> anyhow::Result<()> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.execute(
            "INSERT INTO local_native_dirty(cloud_id) VALUES(?1) ON CONFLICT DO NOTHING",
            [cloud_id],
        )? == 1
        {
            // An upload owns an immutable PRIVATE snapshot. Native edits are a
            // separate durable veto; invalidating that upload's generation here
            // would make its receipt and lost-reply reconciliation impossible.
            tx.execute("UPDATE local_cache_journal SET generation=generation+1,dirty_intent=1,recovery_required=1
                WHERE upload_generation IS NULL AND local_id IN (SELECT remote_id FROM files WHERE cloud_remote_id=?1)", [cloud_id])?;
            tx.execute(
                "UPDATE files SET state='dirty' WHERE cloud_remote_id=?1
                 AND NOT EXISTS(SELECT 1 FROM local_cache_journal j WHERE j.local_id=files.remote_id AND j.upload_generation IS NOT NULL)",
                [cloud_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Publish a complete fsynced native-file snapshot without overwriting the
    /// private cache in place. The adapter must retain the source's exclusive
    /// native no-write/no-delete handle through this call and verify its
    /// placeholder identity and complete resident bytes.
    /// This never marks the visible file in sync or authorizes dehydration.
    pub fn stage_native_edit(
        &self,
        id: &str,
        expected_generation: i64,
        path: &Path,
        size: u64,
    ) -> anyhow::Result<i64> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let record = self.record(id)?;
        ensure!(
            record.cache_path.as_deref() != Some(path),
            "native snapshot must be a new file"
        );
        let old_file = self.file(&record, false)?;
        old_file.try_lock()?;
        let mut candidate = record.clone();
        candidate.cache_path = Some(path.to_path_buf());
        let new_file = self.file(&candidate, true)?;
        new_file.try_lock()?;
        new_file.sync_all()?;
        let metadata = new_file.metadata()?;
        ensure!(metadata.len() == size, "incomplete native capture");
        let modified = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = snapshot(&tx, id)?;
        ensure!(
            state.generation == expected_generation
                && state.native_dirty
                && state.readers == 0
                && state.writers == 0
                && state.upload_generation.is_none()
                && !state.uncertain_upload
                && !state.pending_operations
                && matches!(
                    state.state,
                    FileState::Dirty | FileState::Cached | FileState::Pinned
                ),
            "native capture is stale or another operation owns the cache"
        );
        let generation = state
            .generation
            .checked_add(1)
            .context("cache generation exhausted")?;
        tx.execute("UPDATE local_cache_journal SET generation=?2,cache_path=?3,dirty_intent=1,recovery_required=0 WHERE local_id=?1",
            params![id,generation,path.to_string_lossy()])?;
        tx.execute("UPDATE files SET cache_path=?2,size=?3,modified_unix=?4,state='dirty' WHERE remote_id=?1",
            params![id,path.to_string_lossy(),i64::try_from(metadata.len())?,i64::try_from(modified)?])?;
        tx.execute(
            "DELETE FROM local_native_dirty WHERE cloud_id=?1",
            [record
                .cloud_remote_id
                .as_deref()
                .context("missing cloud identity")?],
        )?;
        tx.commit()?;
        Ok(generation)
    }

    pub fn open_read(&self, id: &str) -> anyhow::Result<CacheReadLease> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let file = self.file(&self.record(id)?, false)?;
        file.try_lock_shared()?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = snapshot(&tx, id)?;
        ensure!(
            state.writers == 0 && !state.native_dirty,
            "cache writer or external native bytes require recovery"
        );
        tx.execute(
            "UPDATE local_cache_journal SET readers=readers+1 WHERE local_id=?1",
            [id],
        )?;
        tx.commit()?;
        Ok(CacheReadLease {
            acknowledged_digest: state.acknowledged_digest,
            journal: self.clone(),
            id: id.into(),
            file,
        })
    }

    pub fn open_write(&self, id: &str) -> anyhow::Result<CacheWriteLease> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let file = self.file(&self.record(id)?, true)?;
        file.try_lock()?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = snapshot(&tx, id)?;
        ensure!(
            state.readers == 0
                && state.writers == 0
                && state.upload_generation.is_none()
                && !state.uncertain_upload
                && !state.recovery_required
                && !state.pending_operations
                && matches!(
                    state.state,
                    FileState::Cached | FileState::Pinned | FileState::Dirty
                ),
            "cache mutation is blocked"
        );
        let generation = state
            .generation
            .checked_add(1)
            .context("cache generation exhausted")?;
        tx.execute("UPDATE local_cache_journal SET generation=?2,writers=1,dirty_intent=1 WHERE local_id=?1", params![id,generation])?;
        tx.execute("UPDATE files SET state='writing' WHERE remote_id=?1", [id])?;
        tx.commit()?;
        Ok(CacheWriteLease {
            journal: self.clone(),
            id: id.into(),
            generation,
            file,
            closed: false,
        })
    }

    pub fn recover_write(&self, id: &str) -> anyhow::Result<()> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let file = self.file(&self.record(id)?, true)?;
        file.try_lock()?;
        file.sync_all()?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = snapshot(&tx, id)?;
        ensure!(
            state.recovery_required
                && !state.native_dirty
                && state.writers == 0
                && state.readers == 0
                && state.upload_generation.is_none(),
            "write recovery is not eligible"
        );
        tx.execute(
            "UPDATE local_cache_journal SET recovery_required=0,dirty_intent=1 WHERE local_id=?1",
            [id],
        )?;
        tx.execute(
            "UPDATE files SET state='dirty',size=?2 WHERE remote_id=?1",
            params![id, i64::try_from(file.metadata()?.len())?],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// `digest` is computed by the engine while no writer can exist. The lease
    /// pins the actual private file until conditional transfer reconciliation.
    pub fn begin_upload(
        &self,
        id: &str,
        digest: impl FnOnce(&mut File) -> anyhow::Result<String>,
    ) -> anyhow::Result<UploadLease> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let record = self.record(id)?;
        let mut file = self.file(&record, false)?;
        file.try_lock_shared()?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = snapshot(&tx, id)?;
        ensure!(
            state.state == FileState::Dirty
                && state.dirty_intent
                && state.readers == 0
                && state.writers == 0
                && state.upload_generation.is_none()
                && !state.pending_operations
                && !state.recovery_required
                && !state.uncertain_upload
                && state.base_etag == record.metadata.etag,
            "upload snapshot is blocked"
        );
        let digest = digest(&mut file)?;
        valid_digest(&digest)?;
        tx.execute("UPDATE local_cache_journal SET upload_generation=generation,upload_epoch=?2,upload_digest=?3 WHERE local_id=?1", params![id,self.0.epoch,digest])?;
        tx.execute(
            "UPDATE files SET state='uploading' WHERE remote_id=?1",
            [id],
        )?;
        tx.commit()?;
        Ok(UploadLease {
            journal: self.clone(),
            record,
            generation: state.generation,
            epoch: self.0.epoch,
            digest,
            file,
            closed: false,
        })
    }

    /// Execute under the owner gate, SQLite write lock and an exclusive cache
    /// file lock. No detached boolean is permission to dehydrate or unlink.
    pub fn with_clean_cache<T>(
        &self,
        id: &str,
        generation: i64,
        action: impl FnOnce(&mut File, &str) -> anyhow::Result<T>,
    ) -> anyhow::Result<Option<T>> {
        self.with_verified_cache(id, generation, true, action)
    }

    /// Acknowledging native sync is also valid for pinned content. Retain the
    /// same generation/owner/transaction/file locks; this grants no eviction.
    pub fn with_acknowledged_cache<T>(
        &self,
        id: &str,
        generation: i64,
        action: impl FnOnce(&mut File, &str) -> anyhow::Result<T>,
    ) -> anyhow::Result<Option<T>> {
        self.with_verified_cache(id, generation, false, action)
    }

    fn with_verified_cache<T>(
        &self,
        id: &str,
        generation: i64,
        release: bool,
        action: impl FnOnce(&mut File, &str) -> anyhow::Result<T>,
    ) -> anyhow::Result<Option<T>> {
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = snapshot(&tx, id)?;
        let eligible = if release {
            state.engine_release_ready()
        } else {
            state.sync_confirmed() && state.resident
        };
        if state.generation != generation || !eligible {
            return Ok(None);
        }
        let mut file = self.file(&self.record(id)?, false)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(err) => return Err(err.into()),
        }
        let result = action(&mut file, &state.acknowledged_digest)?;
        tx.commit()?;
        Ok(Some(result))
    }

    /// Resolve an ambiguous upload only from a downloaded, version-rechecked
    /// provider object. A metadata response alone is never acknowledgement.
    pub fn reconcile_upload(
        &self,
        id: &str,
        remote: &MetadataEntry,
        remote_digest: &str,
        local_digest: impl FnOnce(&mut File) -> anyhow::Result<String>,
    ) -> anyhow::Result<bool> {
        valid_digest(remote_digest)?;
        let _guard = self
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let record = self.record(id)?;
        let mut file = self.file(&record, false)?;
        file.try_lock_shared()?;
        let mut conn = journal_connection(&self.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = snapshot(&tx, id)?;
        ensure!(
            state.uncertain_upload
                && state.state == FileState::Uploading
                && !state.pending_operations
                && !state.recovery_required
                && state.upload_generation == Some(state.generation)
                && state.readers == 0
                && state.writers == 0,
            "upload is not awaiting reconciliation"
        );
        ensure!(
            !remote.is_dir
                && record.metadata.path == remote.path
                && record.cloud_remote_id.as_deref() == Some(remote.remote_id.as_str())
                && !remote.etag.is_empty(),
            "provider identity changed"
        );
        // Compute after reacquiring the final cache lease and keep that same
        // lease through the SQLite acknowledgement. Never accept a stale hash.
        let local_digest = local_digest(&mut file)?;
        valid_digest(&local_digest)?;
        if state.upload_digest.as_deref() == Some(remote_digest)
            && remote_digest == local_digest
            && remote.size == file.metadata()?.len()
        {
            acknowledge(&tx, id, state.generation, remote, remote_digest)?;
            tx.commit()?;
            return Ok(true);
        }
        // Definitively unchanged remote version permits a conditional retry.
        // Any other mismatch preserves local bytes and exposes a conflict.
        let next = if remote.etag == state.base_etag {
            "dirty"
        } else {
            "conflict"
        };
        tx.execute(
            "UPDATE files SET state=?2 WHERE remote_id=?1",
            params![id, next],
        )?;
        tx.execute("UPDATE local_cache_journal SET upload_generation=NULL,upload_epoch=NULL,upload_digest=NULL,uncertain_upload=0 WHERE local_id=?1",[id])?;
        tx.commit()?;
        Ok(false)
    }
}

pub struct CacheReadLease {
    acknowledged_digest: String,
    journal: CacheJournal,
    id: String,
    file: File,
}
impl CacheReadLease {
    pub fn acknowledged_digest(&self) -> &str {
        &self.acknowledged_digest
    }
}
impl Read for CacheReadLease {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.file.read(b)
    }
}
impl Seek for CacheReadLease {
    fn seek(&mut self, p: SeekFrom) -> io::Result<u64> {
        self.file.seek(p)
    }
}
impl Drop for CacheReadLease {
    fn drop(&mut self) {
        // A failed decrement leaves a conservative count until owner recovery.
        let _ = journal_connection(&self.journal.0.db).and_then(|c| {
            Ok(c.execute(
                "UPDATE local_cache_journal SET readers=readers-1 WHERE local_id=?1 AND readers>0",
                [&self.id],
            )?)
        });
    }
}

pub struct CacheWriteLease {
    journal: CacheJournal,
    id: String,
    generation: i64,
    file: File,
    closed: bool,
}
impl Write for CacheWriteLease {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.file.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
impl Seek for CacheWriteLease {
    fn seek(&mut self, p: SeekFrom) -> io::Result<u64> {
        self.file.seek(p)
    }
}
impl CacheWriteLease {
    pub fn set_len(&self, size: u64) -> io::Result<()> {
        self.file.set_len(size)
    }
    pub fn sync_all(&self) -> io::Result<()> {
        self.file.sync_all()
    }
    pub fn finish(mut self) -> anyhow::Result<()> {
        self.finish_inner()?;
        self.closed = true;
        Ok(())
    }
    fn finish_inner(&self) -> anyhow::Result<()> {
        self.file.sync_all()?;
        let mut conn = journal_connection(&self.journal.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(tx.execute("UPDATE local_cache_journal SET writers=0 WHERE local_id=?1 AND generation=?2 AND writers=1",params![self.id,self.generation])?==1,"stale write lease");
        let metadata = self.file.metadata()?;
        let modified = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        tx.execute(
            "UPDATE files SET state='dirty',size=?2,modified_unix=?3 WHERE remote_id=?1",
            params![
                self.id,
                i64::try_from(metadata.len())?,
                i64::try_from(modified)?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
}
impl Drop for CacheWriteLease {
    fn drop(&mut self) {
        if !self.closed && self.finish_inner().is_err() {
            let _ = journal_connection(&self.journal.0.db).and_then(|c| {
                Ok(c.execute(
                    "UPDATE local_cache_journal SET recovery_required=1 WHERE local_id=?1",
                    [&self.id],
                )?)
            });
        }
    }
}

pub struct UploadLease {
    journal: CacheJournal,
    record: FileRecord,
    generation: i64,
    epoch: i64,
    digest: String,
    file: File,
    closed: bool,
}
impl UploadLease {
    pub fn record(&self) -> &FileRecord {
        &self.record
    }
    pub fn file(&mut self) -> &mut File {
        &mut self.file
    }
    pub fn acknowledge(
        mut self,
        remote: &MetadataEntry,
        verified_digest: &str,
    ) -> anyhow::Result<bool> {
        let _guard = self
            .journal
            .0
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("cache gate poisoned"))?;
        let mut conn = journal_connection(&self.journal.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = snapshot(&tx, &self.record.metadata.remote_id)?;
        let current = self.journal.record(&self.record.metadata.remote_id)?;
        let epoch: Option<i64> = tx.query_row(
            "SELECT upload_epoch FROM local_cache_journal WHERE local_id=?1",
            [&self.record.metadata.remote_id],
            |r| r.get(0),
        )?;
        let matches = !remote.is_dir
            && epoch == Some(self.epoch)
            && self.epoch == self.journal.0.epoch
            && state.generation == self.generation
            && state.upload_generation == Some(self.generation)
            && state.state == FileState::Uploading
            && !state.uncertain_upload
            && !state.pending_operations
            && !state.recovery_required
            && state.writers == 0
            && current.metadata.path == self.record.metadata.path
            && current.cache_path == self.record.cache_path
            && current.metadata.etag == self.record.metadata.etag
            && current.cloud_remote_id == self.record.cloud_remote_id
            && current.cloud_remote_id.as_deref() == Some(remote.remote_id.as_str())
            && remote.path == current.metadata.path
            && !remote.etag.is_empty()
            && remote.size == self.file.metadata()?.len()
            && verified_digest == self.digest;
        if !matches {
            return Ok(false);
        }
        acknowledge(
            &tx,
            &current.metadata.remote_id,
            self.generation,
            remote,
            verified_digest,
        )?;
        tx.commit()?;
        self.closed = true;
        Ok(true)
    }
    pub fn reject_conflict(mut self) -> anyhow::Result<()> {
        let mut conn = journal_connection(&self.journal.0.db)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = snapshot(&tx, &self.record.metadata.remote_id)?;
        ensure!(
            state.generation == self.generation
                && state.upload_generation == Some(self.generation)
                && state.state == FileState::Uploading,
            "stale conflict completion"
        );
        ensure!(tx.execute("UPDATE local_cache_journal SET upload_generation=NULL,upload_epoch=NULL,upload_digest=NULL WHERE local_id=?1 AND upload_generation=?2 AND generation=?2 AND upload_epoch=?3",params![self.record.metadata.remote_id,self.generation,self.epoch])?==1,"stale conflict epoch");
        tx.execute(
            "UPDATE files SET state='conflict' WHERE remote_id=?1",
            [&self.record.metadata.remote_id],
        )?;
        tx.commit()?;
        self.closed = true;
        Ok(())
    }
}
impl Drop for UploadLease {
    fn drop(&mut self) {
        if !self.closed {
            let _=journal_connection(&self.journal.0.db).and_then(|c|Ok(c.execute("UPDATE local_cache_journal SET uncertain_upload=1 WHERE local_id=?1 AND upload_generation=?2 AND upload_epoch=?3",params![self.record.metadata.remote_id,self.generation,self.epoch])?));
        }
    }
}

fn acknowledge(
    conn: &rusqlite::Connection,
    id: &str,
    generation: i64,
    remote: &MetadataEntry,
    digest: &str,
) -> anyhow::Result<()> {
    conn.execute("UPDATE local_cache_journal SET acknowledged_generation=?2,acknowledged_digest=?3,base_etag=?4,dirty_intent=0,uncertain_upload=0,recovery_required=0,upload_generation=NULL,upload_epoch=NULL,upload_digest=NULL WHERE local_id=?1",params![id,generation,digest,remote.etag])?;
    // Match the Linux upload commit: a server acknowledgement changes the
    // cloud version, not the content timestamp observed by an open editor.
    conn.execute("UPDATE files SET etag=?2,size=?3,state=CASE WHEN pin_explicit=1 OR pin_origin_remote_id IS NOT NULL THEN 'pinned' ELSE 'cached' END WHERE remote_id=?1",params![id,remote.etag,i64::try_from(remote.size)?])?;
    Ok(())
}

fn snapshot(conn: &rusqlite::Connection, id: &str) -> anyhow::Result<CacheSnapshot> {
    let pending_sql = "SELECT EXISTS(SELECT 1 FROM pending_deletes d JOIN files f ON f.remote_id=?1
        WHERE d.remote_id=?1 OR d.local_id=?1 OR d.path=f.path OR d.upload_path=f.path OR d.uploaded_path=f.path)
        OR EXISTS(SELECT 1 FROM pending_metadata_operations op
            JOIN files parent ON parent.remote_id=op.local_id JOIN files f ON f.remote_id=?1
            WHERE f.path=parent.path OR substr(f.path,1,length(parent.path)+1)=parent.path||'/')
        OR EXISTS(SELECT 1 FROM files f JOIN local_cache_journal j ON j.local_id=f.remote_id
            WHERE f.remote_id=?1 AND (f.etag<>j.base_etag OR f.cloud_remote_id IS NOT j.cloud_id
                OR f.path<>j.cloud_path OR f.cache_path IS NOT j.cache_path))";
    let pending_operations: bool = conn.query_row(pending_sql, [id], |row| row.get(0))?;
    let row_sql = "SELECT j.generation,j.acknowledged_generation,j.readers,j.writers,
        j.upload_generation,j.recovery_required,j.uncertain_upload,j.dirty_intent,f.state,
        f.pin_explicit=1 OR f.pin_origin_remote_id IS NOT NULL,f.cache_path IS NOT NULL,
        j.base_etag,j.acknowledged_digest,j.upload_digest,
        EXISTS(SELECT 1 FROM local_native_dirty n WHERE n.cloud_id=j.cloud_id)
        FROM local_cache_journal j JOIN files f ON f.remote_id=j.local_id WHERE j.local_id=?1";
    let result = conn.query_row(row_sql, [id], |row| {
        let value: String = row.get(8)?;
        let state = FileState::from_str(&value).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                8,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
        Ok(CacheSnapshot {
            native_dirty: row.get(14)?,
            generation: row.get(0)?,
            acknowledged_generation: row.get(1)?,
            readers: row.get(2)?,
            writers: row.get(3)?,
            upload_generation: row.get(4)?,
            recovery_required: row.get(5)?,
            uncertain_upload: row.get(6)?,
            dirty_intent: row.get(7)?,
            state,
            pinned: row.get(9)?,
            resident: row.get(10)?,
            base_etag: row.get(11)?,
            acknowledged_digest: row.get(12)?,
            upload_digest: row.get(13)?,
            pending_operations,
        })
    })?;
    ensure!(
        result.generation >= 0
            && result.acknowledged_generation >= 0
            && result.acknowledged_generation <= result.generation
            && result.readers >= 0
            && result.writers >= 0,
        "invalid durable cache counters"
    );
    Ok(result)
}

fn valid_digest(value: &str) -> anyhow::Result<()> {
    ensure!(
        value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit()),
        "invalid SHA256 evidence"
    );
    Ok(())
}

fn journal_connection(db: &Database) -> anyhow::Result<rusqlite::Connection> {
    let conn = db.connect()?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    Ok(conn)
}

pub(super) fn has_journal_record(conn: &rusqlite::Connection, id: &str) -> anyhow::Result<bool> {
    let exists:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='local_cache_journal')",[],|row|row.get(0))?;
    Ok(exists
        && conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM local_cache_journal WHERE local_id=?1)",
            [id],
            |row| row.get(0),
        )?)
}
fn reject_reparse(metadata: &fs::Metadata) -> anyhow::Result<()> {
    ensure!(!metadata.file_type().is_symlink(), "symlink cache refused");
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & (0x400 | 0x1000 | 0x40000 | 0x400000) == 0,
            "placeholder or reparse cache refused"
        );
    }
    Ok(())
}
fn guard_directories(path: &Path) -> anyhow::Result<Vec<File>> {
    ensure!(path.is_absolute(), "private state must be absolute");
    let mut guards = Vec::new();
    for ancestor in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        reject_reparse(&fs::symlink_metadata(ancestor)?)?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options
                .access_mode(0x81)
                .share_mode(3)
                .custom_flags(0x02000000 | 0x00200000);
        }
        let directory = options.open(ancestor)?;
        let metadata = directory.metadata()?;
        ensure!(
            metadata.is_dir(),
            "private state ancestor is not a directory"
        );
        reject_reparse(&metadata)?;
        guards.push(directory);
    }
    Ok(guards)
}

fn guard_database(path: &Path) -> anyhow::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(3).custom_flags(0x00200000);
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) => reject_reparse(&metadata)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            options.create_new(true);
        }
        Err(error) => return Err(error.into()),
    }
    let file = options.open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "private database must be regular"
    );
    reject_reparse(&file.metadata()?)?;
    Ok(file)
}
