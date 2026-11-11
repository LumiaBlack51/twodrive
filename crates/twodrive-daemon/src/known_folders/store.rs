use super::state::{
    FileSnapshot, KnownFolderState, KnownFolderUploadJob, PendingKnownFolderUpload,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{fs, io::ErrorKind, path::Path, time::Duration};

fn open(data_dir: &Path) -> anyhow::Result<Connection> {
    fs::create_dir_all(data_dir)?;
    let path = data_dir.join("known-folders-state.sqlite3");
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    drop(options.open(&path)?);
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(10))?;
    conn.execute_batch(
        "PRAGMA synchronous=FULL;
         CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS files (
             local TEXT PRIMARY KEY, size INTEGER NOT NULL, modified INTEGER NOT NULL, nanos INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS pending (
             local TEXT PRIMARY KEY, remote TEXT NOT NULL, size INTEGER NOT NULL,
             modified INTEGER NOT NULL, nanos INTEGER NOT NULL);",
    )?;
    Ok(conn)
}

impl KnownFolderState {
    pub(super) fn load(data_dir: &Path) -> anyhow::Result<Self> {
        if data_dir.join("known-folders-state.sqlite3").exists() {
            let conn = open(data_dir)?;
            let initialized: Option<bool> = conn
                .query_row(
                    "SELECT value FROM settings WHERE key = 'baseline'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(baseline_initialized) = initialized {
                let mut state = Self {
                    baseline_initialized,
                    ..Self::default()
                };
                {
                    let mut stmt =
                        conn.prepare("SELECT local, size, modified, nanos FROM files")?;
                    for row in stmt.query_map([], |r| {
                        Ok((
                            r.get(0)?,
                            FileSnapshot {
                                size: r.get(1)?,
                                modified_unix: r.get(2)?,
                                modified_nanos: r.get(3)?,
                            },
                        ))
                    })? {
                        let (key, snapshot) = row?;
                        state.files.insert(key, snapshot);
                    }
                    let mut stmt =
                        conn.prepare("SELECT local, remote, size, modified, nanos FROM pending")?;
                    for row in stmt.query_map([], |r| {
                        Ok((
                            r.get(0)?,
                            PendingKnownFolderUpload {
                                remote_path: r.get(1)?,
                                snapshot: FileSnapshot {
                                    size: r.get(2)?,
                                    modified_unix: r.get(3)?,
                                    modified_nanos: r.get(4)?,
                                },
                            },
                        ))
                    })? {
                        let (key, pending) = row?;
                        state.pending.insert(key, pending);
                    }
                }
                state.storage = Some(conn);
                return Ok(state);
            }
        }
        // Import the old snapshot transactionally. Keep its bytes for rollback;
        // an interrupted import has no baseline marker and can be retried safely.
        match fs::read_to_string(data_dir.join("known-folders-state.json")) {
            Ok(data) => {
                let mut state: Self = serde_json::from_str(&data)?;
                // Old snapshots only had whole seconds. Adopt current subsecond
                // metadata when the old size/second signature still matches,
                // avoiding a migration-triggered reupload of unchanged history.
                for (local, snapshot) in &mut state.files {
                    if let Ok(metadata) = fs::symlink_metadata(local) {
                        let current = FileSnapshot::from_metadata(&metadata);
                        if current.size == snapshot.size
                            && current.modified_unix == snapshot.modified_unix
                        {
                            snapshot.modified_nanos = current.modified_nanos;
                        }
                    }
                }
                state.save(data_dir)?;
                Ok(state)
            }
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err.into()),
        }
    }

    pub(super) fn save(&mut self, data_dir: &Path) -> anyhow::Result<()> {
        if self.storage.is_none() {
            self.storage = Some(open(data_dir)?);
        }
        let tx = self.storage.as_mut().unwrap().transaction()?;
        tx.execute_batch("DELETE FROM files; DELETE FROM pending;")?;
        {
            let mut stmt = tx.prepare("INSERT INTO files VALUES (?1, ?2, ?3, ?4)")?;
            for (local, file) in &self.files {
                stmt.execute(params![
                    local,
                    file.size,
                    file.modified_unix,
                    file.modified_nanos
                ])?;
            }
            let mut stmt = tx.prepare("INSERT INTO pending VALUES (?1, ?2, ?3, ?4, ?5)")?;
            for (local, pending) in &self.pending {
                stmt.execute(params![
                    local,
                    pending.remote_path,
                    pending.snapshot.size,
                    pending.snapshot.modified_unix,
                    pending.snapshot.modified_nanos
                ])?;
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO settings VALUES ('baseline', ?1)",
            [self.baseline_initialized],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn queue_uploads(
        &mut self,
        data_dir: &Path,
        jobs: &[KnownFolderUploadJob],
    ) -> anyhow::Result<()> {
        if self.storage.is_none() {
            self.save(data_dir)?;
        }
        let tx = self.storage.as_mut().unwrap().transaction()?;
        {
            let mut stmt =
                tx.prepare("INSERT OR REPLACE INTO pending VALUES (?1, ?2, ?3, ?4, ?5)")?;
            for job in jobs {
                stmt.execute(params![
                    job.local_path.to_string_lossy(),
                    job.remote_path,
                    job.snapshot.size,
                    job.snapshot.modified_unix,
                    job.snapshot.modified_nanos
                ])?;
            }
        }
        tx.commit()?;
        for job in jobs {
            self.pending.insert(
                job.local_path.to_string_lossy().into_owned(),
                PendingKnownFolderUpload {
                    remote_path: job.remote_path.clone(),
                    snapshot: job.snapshot.clone(),
                },
            );
        }
        Ok(())
    }

    pub(super) fn complete_upload(
        &mut self,
        local: String,
        snapshot: FileSnapshot,
    ) -> anyhow::Result<()> {
        let tx = self
            .storage
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("upload was not persisted"))?
            .transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO files VALUES (?1, ?2, ?3, ?4)",
            params![
                local,
                snapshot.size,
                snapshot.modified_unix,
                snapshot.modified_nanos
            ],
        )?;
        tx.execute("DELETE FROM pending WHERE local = ?1", [&local])?;
        tx.commit()?;
        self.files.insert(local.clone(), snapshot);
        self.pending.remove(&local);
        Ok(())
    }
}
