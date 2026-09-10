use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("unknown file state: {0}")]
    UnknownState(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    OnlineOnly,
    Hydrating,
    Cached,
    Pinned,
    Writing,
    Dirty,
    Uploading,
    Conflict,
    Error,
}

impl FileState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OnlineOnly => "online_only",
            Self::Hydrating => "hydrating",
            Self::Cached => "cached",
            Self::Pinned => "pinned",
            Self::Writing => "writing",
            Self::Dirty => "dirty",
            Self::Uploading => "uploading",
            Self::Conflict => "conflict",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for FileState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FileState {
    type Err = CoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "online_only" => Ok(Self::OnlineOnly),
            "hydrating" => Ok(Self::Hydrating),
            "cached" => Ok(Self::Cached),
            "pinned" => Ok(Self::Pinned),
            "writing" => Ok(Self::Writing),
            "dirty" => Ok(Self::Dirty),
            "uploading" => Ok(Self::Uploading),
            "conflict" => Ok(Self::Conflict),
            "error" => Ok(Self::Error),
            other => Err(CoreError::UnknownState(other.to_string())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MetadataEntry {
    pub remote_id: String,
    pub path: String,
    pub parent_path: String,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified_unix: i64,
    pub etag: String,
}

impl MetadataEntry {
    pub fn new_file(
        remote_id: impl Into<String>,
        path: impl Into<String>,
        size: u64,
        modified_unix: i64,
        etag: impl Into<String>,
    ) -> Self {
        Self::new(remote_id, path, false, size, modified_unix, etag)
    }

    pub fn new_dir(
        remote_id: impl Into<String>,
        path: impl Into<String>,
        modified_unix: i64,
        etag: impl Into<String>,
    ) -> Self {
        Self::new(remote_id, path, true, 0, modified_unix, etag)
    }

    fn new(
        remote_id: impl Into<String>,
        path: impl Into<String>,
        is_dir: bool,
        size: u64,
        modified_unix: i64,
        etag: impl Into<String>,
    ) -> Self {
        let path = normalize_cloud_path(&path.into());
        let parent_path = parent_cloud_path(&path);
        let name = cloud_name(&path);

        Self {
            remote_id: remote_id.into(),
            path,
            parent_path,
            name,
            is_dir,
            size,
            modified_unix,
            etag: etag.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileRecord {
    pub metadata: MetadataEntry,
    /// Stable remote identity assigned by OneDrive. `metadata.remote_id` is the
    /// local identity used by the cache/database and never changes after create.
    pub cloud_remote_id: Option<String>,
    pub state: FileState,
    pub cache_path: Option<PathBuf>,
    pub cache_accessed_unix: Option<i64>,
    pub pin_explicit: bool,
    pub pin_origin_remote_id: Option<String>,
    pub pin_inheritance_blocked: bool,
}

#[derive(Debug, Clone)]
pub struct PendingDelete {
    pub remote_id: String,
    pub path: String,
    pub cache_path: Option<PathBuf>,
    pub queued_unix: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingMetadataKind {
    CreateFolder,
    Move,
}

impl PendingMetadataKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CreateFolder => "create_folder",
            Self::Move => "move",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PendingMetadataOperation {
    pub local_id: String,
    pub kind: PendingMetadataKind,
    pub path: String,
    pub queued_unix: i64,
}

impl FileRecord {
    pub fn effective_pinned(&self) -> bool {
        self.pin_explicit || self.pin_origin_remote_id.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub db_path: PathBuf,
    pub mount_dir: PathBuf,
    pub token_path: PathBuf,
}

impl AppPaths {
    pub fn discover() -> anyhow::Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;

        let config_dir = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("twodrive");
        let data_dir = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("twodrive");
        let cache_dir = data_dir.join("cache");
        let db_path = data_dir.join("twodrive.sqlite3");
        let mount_dir = env::var_os("TWODRIVE_MOUNT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("TwoDrive/OneDrive"));
        let config_path = config_dir.join("config.toml");
        let token_path = config_dir.join("tokens.json");

        Ok(Self {
            config_dir,
            config_path,
            data_dir,
            cache_dir,
            db_path,
            mount_dir,
            token_path,
        })
    }

    pub fn ensure(&self) -> anyhow::Result<()> {
        ensure_dir(&self.config_dir)?;
        ensure_dir(&self.data_dir)?;
        ensure_dir(&self.cache_dir)?;
        ensure_dir(&self.mount_dir)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub graph: GraphConfig,
    pub cache: CacheConfig,
    pub power: PowerConfig,
    pub known_folders: KnownFoldersConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphConfig {
    pub client_id: String,
    pub tenant: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheConfig {
    pub retain_for: String,
    pub max_size: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PowerConfig {
    pub ac_sync_interval: String,
    pub battery_sync_interval: String,
    pub ac_download_concurrency: u8,
    pub battery_download_concurrency: u8,
    pub ac_upload_concurrency: u8,
    pub battery_upload_concurrency: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KnownFoldersConfig {
    pub enabled: bool,
    pub mode: String,
    pub debounce: String,
    pub rescan_interval: String,
    pub startup_scan: bool,
    pub upload_deletes: bool,
    pub exclude_suffixes: Vec<String>,
    pub folders: Vec<KnownFolderConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KnownFolderConfig {
    pub local: String,
    pub remote: String,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            client_id: "PASTE_AZURE_APP_CLIENT_ID_HERE".to_string(),
            tenant: "common".to_string(),
            redirect_uri: "http://localhost:53682".to_string(),
            scopes: vec![
                "Files.ReadWrite".to_string(),
                "User.Read".to_string(),
                "offline_access".to_string(),
                "openid".to_string(),
                "profile".to_string(),
            ],
        }
    }
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            retain_for: "30d".to_string(),
            max_size: "20GiB".to_string(),
        }
    }
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self {
            ac_sync_interval: "15m".to_string(),
            battery_sync_interval: "60m".to_string(),
            ac_download_concurrency: 4,
            battery_download_concurrency: 1,
            ac_upload_concurrency: 4,
            battery_upload_concurrency: 2,
        }
    }
}

impl Default for KnownFoldersConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: "upload_only".to_string(),
            debounce: "5s".to_string(),
            rescan_interval: "15m".to_string(),
            startup_scan: true,
            upload_deletes: false,
            exclude_suffixes: vec![
                ".crdownload".to_string(),
                ".part".to_string(),
                ".tmp".to_string(),
                ".download".to_string(),
                ".temp".to_string(),
                ".partial".to_string(),
                ".filepart".to_string(),
                ".opdownload".to_string(),
                ".aria2".to_string(),
                ".!qB".to_string(),
                ".swp".to_string(),
                ".swo".to_string(),
                ".swx".to_string(),
                ".bak".to_string(),
            ],
            folders: vec![
                KnownFolderConfig {
                    local: "~/Pictures".to_string(),
                    remote: "/Pictures".to_string(),
                },
                KnownFolderConfig {
                    local: "~/Downloads".to_string(),
                    remote: "/Downloads".to_string(),
                },
            ],
        }
    }
}

impl Default for KnownFolderConfig {
    fn default() -> Self {
        Self {
            local: String::new(),
            remote: "/".to_string(),
        }
    }
}

impl Config {
    pub fn load_or_create(paths: &AppPaths) -> anyhow::Result<Self> {
        paths.ensure()?;
        if !paths.config_path.exists() {
            let config = Self::default();
            config.save(paths)?;
            return Ok(config);
        }

        let data = fs::read_to_string(&paths.config_path)?;
        Ok(toml::from_str(&data)?)
    }

    pub fn save(&self, paths: &AppPaths) -> anyhow::Result<()> {
        fs::create_dir_all(&paths.config_dir)?;
        fs::write(&paths.config_path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn validate_graph_login(&self) -> anyhow::Result<()> {
        if self.graph.client_id.trim().is_empty()
            || self.graph.client_id == "PASTE_AZURE_APP_CLIENT_ID_HERE"
        {
            anyhow::bail!(
                "set graph.client_id in {} before running login",
                AppPaths::discover()?.config_path.display()
            );
        }
        Ok(())
    }

    pub fn cache_retain_seconds(&self) -> anyhow::Result<i64> {
        parse_duration_seconds(&self.cache.retain_for)
    }

    pub fn known_folder_debounce_seconds(&self) -> anyhow::Result<u64> {
        parse_duration_seconds(&self.known_folders.debounce).map(|value| value.max(1) as u64)
    }

    pub fn known_folder_rescan_seconds(&self) -> anyhow::Result<u64> {
        parse_duration_seconds(&self.known_folders.rescan_interval)
            .map(|value| if value <= 0 { 0 } else { value.max(30) as u64 })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenData {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at_unix: i64,
}

#[derive(Debug, Clone)]
pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self) -> anyhow::Result<Option<TokenData>> {
        if !self.path.exists() {
            return Ok(None);
        }

        let data = fs::read_to_string(&self.path)?;
        Ok(Some(serde_json::from_str(&data)?))
    }

    pub fn save(&self, token: &TokenData) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, serde_json::to_string_pretty(token)?)?;
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
        eprintln!(
            "twodrive: token Secret Service integration is not enabled yet; using 0600 fallback file {}",
            self.path.display()
        );
        Ok(())
    }

    pub fn delete(&self) -> anyhow::Result<()> {
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Database {
    db_path: PathBuf,
}

impl Database {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.db_path
    }

    pub fn init(&self) -> anyhow::Result<()> {
        let _migration_guard = database_init_lock()
            .lock()
            .map_err(|_| anyhow::anyhow!("database initialization lock is poisoned"))?;
        if let Some(parent) = self.db_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let conn = self.connect()?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS files (
                remote_id TEXT PRIMARY KEY,
                cloud_remote_id TEXT,
                path TEXT NOT NULL UNIQUE,
                parent_path TEXT NOT NULL,
                name TEXT NOT NULL,
                is_dir INTEGER NOT NULL,
                size INTEGER NOT NULL,
                modified_unix INTEGER NOT NULL,
                etag TEXT NOT NULL,
                state TEXT NOT NULL,
                cache_path TEXT,
                cache_accessed_unix INTEGER,
                pin_explicit INTEGER NOT NULL DEFAULT 0,
                pin_origin_remote_id TEXT,
                pin_inheritance_blocked INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_files_parent_path ON files(parent_path);
            CREATE TABLE IF NOT EXISTS app_state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS pending_deletes (
                remote_id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                cache_path TEXT,
                queued_unix INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS pending_metadata_operations (
                local_id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                path TEXT NOT NULL,
                queued_unix INTEGER NOT NULL
            );
            "#,
        )?;
        ensure_column(
            &conn,
            "files",
            "cloud_remote_id",
            "ALTER TABLE files ADD COLUMN cloud_remote_id TEXT",
        )?;
        conn.execute(
            "UPDATE files SET cloud_remote_id = remote_id WHERE cloud_remote_id IS NULL AND remote_id NOT LIKE 'local-upload-%'",
            [],
        )?;
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_files_cloud_remote_id ON files(cloud_remote_id) WHERE cloud_remote_id IS NOT NULL",
            [],
        )?;
        ensure_column(
            &conn,
            "files",
            "cache_accessed_unix",
            "ALTER TABLE files ADD COLUMN cache_accessed_unix INTEGER",
        )?;
        ensure_column(
            &conn,
            "files",
            "pin_explicit",
            "ALTER TABLE files ADD COLUMN pin_explicit INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "files",
            "pin_origin_remote_id",
            "ALTER TABLE files ADD COLUMN pin_origin_remote_id TEXT",
        )?;
        ensure_column(
            &conn,
            "files",
            "pin_inheritance_blocked",
            "ALTER TABLE files ADD COLUMN pin_inheritance_blocked INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "files",
            "release_pending",
            "ALTER TABLE files ADD COLUMN release_pending INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "files",
            "download_generation",
            "ALTER TABLE files ADD COLUMN download_generation INTEGER NOT NULL DEFAULT 0",
        )?;
        let migrated = conn
            .query_row(
                "SELECT value FROM app_state WHERE key = 'pin_policy_v1_migrated'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .is_some();
        if !migrated {
            conn.execute_batch(
                r#"
                UPDATE files AS child
                SET pin_explicit = 1
                WHERE child.state = 'pinned'
                  AND NOT EXISTS (
                    SELECT 1
                    FROM files AS parent
                    WHERE parent.path = child.parent_path
                      AND parent.state = 'pinned'
                  );
                INSERT INTO app_state(key, value)
                VALUES ('pin_policy_v1_migrated', '1')
                ON CONFLICT(key) DO UPDATE SET value = excluded.value;
                "#,
            )?;
        }
        drop(conn);
        if !migrated {
            self.recompute_pin_inheritance()?;
        }
        Ok(())
    }

    pub fn upsert_metadata(&self, entry: &MetadataEntry) -> anyhow::Result<()> {
        self.upsert_metadata_inner(entry, true)
    }

    pub fn upsert_metadata_batch<'a>(
        &self,
        entries: impl IntoIterator<Item = &'a MetadataEntry>,
    ) -> anyhow::Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for entry in entries {
            let pending_delete = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM pending_deletes WHERE remote_id = ?1 OR path = ?2)",
                params![entry.remote_id, normalize_cloud_path(&entry.path)],
                |row| row.get::<_, bool>(0),
            )?;
            if pending_delete {
                continue;
            }

            let local_id = tx
                .query_row(
                    "SELECT remote_id FROM files WHERE cloud_remote_id = ?1",
                    params![entry.remote_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .unwrap_or_else(|| entry.remote_id.clone());
            let pending_metadata = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM pending_metadata_operations WHERE local_id = ?1)",
                params![local_id],
                |row| row.get::<_, bool>(0),
            )?;
            if pending_metadata {
                continue;
            }

            let protected_path = tx.query_row(
                r#"
                SELECT EXISTS(
                    SELECT 1 FROM files
                    WHERE path = ?1
                      AND remote_id <> ?2
                      AND state IN ('writing', 'dirty', 'uploading')
                )
                "#,
                params![entry.path, local_id],
                |row| row.get::<_, bool>(0),
            )?;
            let protected_id = tx.query_row(
                r#"
                SELECT EXISTS(
                    SELECT 1 FROM files
                    WHERE remote_id = ?1
                      AND state IN ('writing', 'dirty', 'uploading')
                )
                "#,
                params![local_id],
                |row| row.get::<_, bool>(0),
            )?;
            if protected_path || protected_id {
                continue;
            }

            let old_path = tx
                .query_row(
                    "SELECT path FROM files WHERE remote_id = ?1",
                    params![local_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            tx.execute(
                "DELETE FROM files WHERE path = ?1 AND remote_id <> ?2",
                params![entry.path, local_id],
            )?;
            tx.execute(
                r#"
                INSERT INTO files (
                    remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state,
                    cloud_remote_id,
                    pin_inheritance_blocked
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1)
                ON CONFLICT(remote_id) DO UPDATE SET
                    path = excluded.path,
                    parent_path = excluded.parent_path,
                    name = excluded.name,
                    is_dir = excluded.is_dir,
                    size = excluded.size,
                    modified_unix = CASE
                        WHEN files.is_dir = 0 AND files.etag <> ''
                             AND files.etag = excluded.etag AND files.size = excluded.size
                        THEN files.modified_unix
                        ELSE excluded.modified_unix
                    END,
                    etag = excluded.etag,
                    cloud_remote_id = excluded.cloud_remote_id
                "#,
                params![
                    local_id,
                    entry.path,
                    entry.parent_path,
                    entry.name,
                    entry.is_dir as i64,
                    entry.size as i64,
                    entry.modified_unix,
                    entry.etag,
                    FileState::OnlineOnly.as_str(),
                    entry.remote_id,
                ],
            )?;

            if let Some(old_path) = old_path.filter(|old_path| old_path != &entry.path) {
                let descendants = {
                    let mut stmt = tx.prepare(
                        r#"
                        SELECT remote_id, path FROM files
                        WHERE (
                            ?1 = '/' AND path <> '/' AND substr(path, 1, 1) = '/'
                        ) OR (
                            ?1 <> '/' AND substr(path, 1, length(?1) + 1) = ?1 || '/'
                        )
                        ORDER BY path
                        "#,
                    )?;
                    stmt.query_map(params![old_path], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?
                };
                for (remote_id, old_descendant_path) in descendants {
                    let suffix = old_descendant_path
                        .strip_prefix(&old_path)
                        .ok_or_else(|| anyhow::anyhow!("descendant path lost its parent prefix"))?;
                    let path = normalize_cloud_path(&format!("{}{suffix}", entry.path));
                    tx.execute(
                        "UPDATE files SET path = ?1, parent_path = ?2, name = ?3 WHERE remote_id = ?4",
                        params![path, parent_cloud_path(&path), cloud_name(&path), remote_id],
                    )?;
                }
            }
        }
        tx.commit()?;
        self.recompute_pin_inheritance()
    }

    fn upsert_metadata_inner(
        &self,
        entry: &MetadataEntry,
        recompute_pins: bool,
    ) -> anyhow::Result<()> {
        if self.is_pending_delete(&entry.remote_id, &entry.path)? {
            return Ok(());
        }
        let incoming_cloud_id =
            (!entry.remote_id.starts_with("local-upload-")).then_some(entry.remote_id.clone());
        let existing = match incoming_cloud_id.as_deref() {
            Some(cloud_remote_id) => self.get_by_cloud_remote_id(cloud_remote_id)?,
            None => self.get_by_remote_id(&entry.remote_id)?,
        };
        let local_id = existing
            .as_ref()
            .map(|record| record.metadata.remote_id.clone())
            .unwrap_or_else(|| entry.remote_id.clone());
        if self.pending_metadata_operation(&local_id)?.is_some() {
            return Ok(());
        }
        if self.get_by_path(&entry.path)?.is_some_and(|record| {
            record.metadata.remote_id != local_id
                && matches!(
                    record.state,
                    FileState::Writing | FileState::Dirty | FileState::Uploading
                )
        }) {
            return Ok(());
        }
        if self.get_by_remote_id(&local_id)?.is_some_and(|record| {
            matches!(
                record.state,
                FileState::Writing | FileState::Dirty | FileState::Uploading
            )
        }) {
            if recompute_pins {
                self.recompute_pin_inheritance()?;
            }
            return Ok(());
        }
        let old_path = self
            .get_by_remote_id(&local_id)?
            .map(|record| record.metadata.path);
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM files WHERE path = ?1 AND remote_id <> ?2",
            params![entry.path, local_id],
        )?;
        tx.execute(
            r#"
            INSERT INTO files (
                remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state,
                cloud_remote_id
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(remote_id) DO UPDATE SET
                path = excluded.path,
                parent_path = excluded.parent_path,
                name = excluded.name,
                is_dir = excluded.is_dir,
                size = excluded.size,
                modified_unix = CASE
                        WHEN files.is_dir = 0 AND files.etag <> ''
                             AND files.etag = excluded.etag AND files.size = excluded.size
                        THEN files.modified_unix
                        ELSE excluded.modified_unix
                    END,
                etag = excluded.etag,
                cloud_remote_id = excluded.cloud_remote_id
            "#,
            params![
                local_id,
                entry.path,
                entry.parent_path,
                entry.name,
                entry.is_dir as i64,
                entry.size as i64,
                entry.modified_unix,
                entry.etag,
                FileState::OnlineOnly.as_str(),
                incoming_cloud_id,
            ],
        )?;
        tx.commit()?;
        if let Some(old_path) = old_path.filter(|old_path| old_path != &entry.path) {
            self.move_descendants(&old_path, &entry.path)?;
        }
        if recompute_pins {
            self.recompute_pin_inheritance()?;
        }
        Ok(())
    }

    pub fn mark_state(&self, remote_id: &str, state: FileState) -> anyhow::Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE files SET state = ?1 WHERE remote_id = ?2",
            params![state.as_str(), remote_id],
        )?;
        Ok(())
    }

    /// Claim only the snapshot inspected by the worker. Network preflight can
    /// overlap a rename/replacement or a reopen on the FUSE thread.
    pub fn begin_upload(&self, record: &FileRecord) -> anyhow::Result<bool> {
        if !matches!(record.state, FileState::Dirty | FileState::Uploading) {
            return Ok(false);
        }
        let changed = self.connect()?.execute(
            "UPDATE files SET state = 'uploading'
             WHERE remote_id = ?1 AND path = ?2 AND cache_path IS ?3
               AND state = ?4 AND size = ?5 AND modified_unix = ?6
               AND etag = ?7 AND cloud_remote_id IS ?8",
            params![
                record.metadata.remote_id,
                record.metadata.path,
                record
                    .cache_path
                    .as_ref()
                    .map(|path| path.to_string_lossy()),
                record.state.as_str(),
                i64::try_from(record.metadata.size).unwrap_or(i64::MAX),
                record.metadata.modified_unix,
                record.metadata.etag,
                record.cloud_remote_id,
            ],
        )?;
        Ok(changed == 1)
    }

    pub fn mark_dirty_with_size(&self, remote_id: &str, size: u64) -> anyhow::Result<()> {
        let conn = self.connect()?;
        let changed = conn.execute(
            "UPDATE files SET state = ?1, size = ?2, modified_unix = ?3 WHERE remote_id = ?4",
            params![
                FileState::Dirty.as_str(),
                i64::try_from(size).unwrap_or(i64::MAX),
                now_unix(),
                remote_id
            ],
        )?;
        if changed == 0 {
            anyhow::bail!("cannot mark unknown item {remote_id} dirty");
        }
        Ok(())
    }

    pub fn download_generation(&self, remote_id: &str) -> anyhow::Result<i64> {
        Ok(self.connect()?.query_row(
            "SELECT download_generation FROM files WHERE remote_id = ?1",
            params![remote_id],
            |row| row.get(0),
        )?)
    }

    pub fn begin_hydration(&self, remote_id: &str, generation: i64) -> anyhow::Result<bool> {
        Ok(self.connect()?.execute(
            "UPDATE files SET state = CASE WHEN pin_explicit = 1 OR pin_origin_remote_id IS NOT NULL THEN 'pinned' ELSE 'hydrating' END, cache_path = NULL, cache_accessed_unix = NULL
             WHERE remote_id = ?1 AND download_generation = ?2 AND release_pending = 0
             AND state IN ('online_only', 'hydrating', 'pinned', 'cached', 'error') AND cloud_remote_id IS NOT NULL",
            params![remote_id, generation])? == 1)
    }

    pub fn finish_hydration(
        &self,
        remote_id: &str,
        generation: i64,
        temporary: &Path,
        cache: &Path,
    ) -> anyhow::Result<bool> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let eligible: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM files WHERE remote_id = ?1 AND download_generation = ?2
             AND release_pending = 0 AND state IN ('online_only', 'hydrating', 'pinned'))",
            params![remote_id, generation],
            |row| row.get(0),
        )?;
        if !eligible {
            return Ok(false);
        }
        // Serialize publication with release and local writes, including the final-byte race.
        fs::rename(temporary, cache)?;
        tx.execute("UPDATE files SET state = CASE WHEN pin_explicit = 1 OR pin_origin_remote_id IS NOT NULL THEN 'pinned' ELSE 'cached' END,
            cache_path = ?2, cache_accessed_unix = ?3 WHERE remote_id = ?1",
            params![remote_id, cache.to_string_lossy(), now_unix()])?;
        tx.commit()?;
        Ok(true)
    }

    pub fn fail_hydration(&self, remote_id: &str, generation: i64) -> anyhow::Result<()> {
        self.connect()?.execute(
            "UPDATE files SET state = 'online_only' WHERE remote_id = ?1
            AND download_generation = ?2 AND state = 'hydrating' AND cache_path IS NULL",
            params![remote_id, generation],
        )?;
        Ok(())
    }

    pub fn mark_cached(&self, remote_id: &str, cache_path: &Path) -> anyhow::Result<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            UPDATE files
            SET state = CASE
                    WHEN pin_explicit = 1 OR pin_origin_remote_id IS NOT NULL THEN 'pinned'
                    ELSE ?1
                END,
                cache_path = ?2,
                cache_accessed_unix = ?3
            WHERE remote_id = ?4
            "#,
            params![
                FileState::Cached.as_str(),
                cache_path.to_string_lossy(),
                now_unix(),
                remote_id
            ],
        )?;
        Ok(())
    }

    pub fn mark_online_only(&self, remote_id: &str) -> anyhow::Result<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            UPDATE files
            SET state = CASE
                    WHEN pin_explicit = 1 OR pin_origin_remote_id IS NOT NULL THEN 'hydrating'
                    ELSE ?1
                END,
                cache_path = NULL,
                cache_accessed_unix = NULL
            WHERE remote_id = ?2
            "#,
            params![FileState::OnlineOnly.as_str(), remote_id],
        )?;
        Ok(())
    }

    pub fn mark_cache_accessed(&self, remote_id: &str) -> anyhow::Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE files SET cache_accessed_unix = ?1 WHERE remote_id = ?2",
            params![now_unix(), remote_id],
        )?;
        Ok(())
    }

    pub fn mark_pinned(&self, remote_id: &str) -> anyhow::Result<()> {
        self.set_explicit_pin(remote_id, true)
    }

    pub fn mark_unpinned(&self, remote_id: &str) -> anyhow::Result<()> {
        self.set_explicit_pin(remote_id, false)
    }

    pub fn set_explicit_pin(&self, remote_id: &str, pinned: bool) -> anyhow::Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = self.query_record(
            &tx,
            "SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id FROM files WHERE remote_id = ?1",
            params![remote_id],
        )?;
        let Some(record) = record else {
            anyhow::bail!("cannot change pin policy for unknown item {remote_id}");
        };
        let changed = tx.execute(
            r#"
            UPDATE files
            SET pin_explicit = ?1,
                pin_inheritance_blocked = CASE WHEN ?1 = 1 THEN 0 ELSE pin_inheritance_blocked END
            WHERE remote_id = ?2
            "#,
            params![pinned as i64, remote_id],
        )?;
        if changed == 0 {
            anyhow::bail!("cannot change pin policy for unknown item {remote_id}");
        }
        if pinned && record.metadata.is_dir {
            tx.execute(
                r#"
                UPDATE files
                SET pin_inheritance_blocked = 0
                WHERE substr(path, 1, length(?1) + 1) = ?1 || '/'
                "#,
                params![record.metadata.path],
            )?;
        }
        tx.commit()?;
        self.recompute_pin_inheritance()
    }

    pub fn recompute_pin_inheritance(&self) -> anyhow::Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut stmt = tx.prepare(
            "SELECT remote_id, path, pin_explicit, pin_inheritance_blocked FROM files ORDER BY length(path), path",
        )?;
        let records = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? != 0,
                    row.get::<_, i64>(3)? != 0,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);

        let explicit_roots = records
            .iter()
            .filter(|(_, _, explicit, _)| *explicit)
            .map(|(remote_id, path, _, _)| (path.clone(), remote_id.clone()))
            .collect::<HashMap<_, _>>();

        for (remote_id, path, explicit, inheritance_blocked) in records {
            let origin = if explicit || inheritance_blocked {
                None
            } else {
                let mut ancestor = parent_cloud_path(&path);
                let mut origin = None;
                loop {
                    if let Some(remote_id) = explicit_roots.get(&ancestor) {
                        origin = Some(remote_id.clone());
                        break;
                    }
                    if ancestor == "/" {
                        break;
                    }
                    ancestor = parent_cloud_path(&ancestor);
                }
                origin
            };
            let effective = explicit || origin.is_some();
            tx.execute(
                r#"
                UPDATE files
                SET pin_origin_remote_id = ?1,
                    release_pending = CASE WHEN ?2 = 1 THEN 0 ELSE release_pending END,
                    state = CASE
                        WHEN state IN ('writing', 'dirty', 'uploading', 'conflict', 'error') THEN state
                        WHEN ?2 = 1 AND is_dir = 1 THEN 'pinned'
                        WHEN ?2 = 1 AND cache_path IS NOT NULL THEN 'pinned'
                        WHEN ?2 = 1 THEN 'hydrating'
                        WHEN cache_path IS NOT NULL THEN 'cached'
                        ELSE 'online_only'
                    END
                WHERE remote_id = ?3
                "#,
                params![origin, effective as i64, remote_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn move_subtree(&self, remote_id: &str, new_path: &str) -> anyhow::Result<()> {
        let record = self
            .get_by_remote_id(remote_id)?
            .ok_or_else(|| anyhow::anyhow!("cannot move unknown item {remote_id}"))?;
        let new_path = normalize_cloud_path(new_path);
        let entry = if record.metadata.is_dir {
            MetadataEntry::new_dir(
                remote_id,
                &new_path,
                record.metadata.modified_unix,
                &record.metadata.etag,
            )
        } else {
            MetadataEntry::new_file(
                remote_id,
                &new_path,
                record.metadata.size,
                record.metadata.modified_unix,
                &record.metadata.etag,
            )
        };
        self.upsert_metadata(&entry)?;
        self.allow_pin_inheritance(remote_id)
    }

    pub fn create_local_directory(&self, local_id: &str, path: &str) -> anyhow::Result<FileRecord> {
        let path = normalize_cloud_path(path);
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx
            .query_row(
                "SELECT 1 FROM files WHERE path = ?1 LIMIT 1",
                params![path],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            anyhow::bail!("path already exists: {path}");
        }
        tx.execute(
            r#"
            INSERT INTO files(
                remote_id, cloud_remote_id, path, parent_path, name, is_dir, size,
                modified_unix, etag, state, pin_inheritance_blocked
            ) VALUES (?1, NULL, ?2, ?3, ?4, 1, 0, ?5, '', 'dirty', 0)
            "#,
            params![
                local_id,
                path,
                parent_cloud_path(&path),
                cloud_name(&path),
                now_unix()
            ],
        )?;
        tx.execute(
            "INSERT INTO pending_metadata_operations(local_id, kind, path, queued_unix) VALUES (?1, 'create_folder', ?2, ?3)",
            params![local_id, path, now_unix()],
        )?;
        tx.commit()?;
        self.recompute_pin_inheritance()?;
        self.get_by_remote_id(local_id)?
            .ok_or_else(|| anyhow::anyhow!("created local directory disappeared"))
    }

    pub fn move_subtree_and_queue(&self, local_id: &str, new_path: &str) -> anyhow::Result<()> {
        let record = self
            .get_by_remote_id(local_id)?
            .ok_or_else(|| anyhow::anyhow!("cannot move unknown item {local_id}"))?;
        let old_path = record.metadata.path.clone();
        let new_path = normalize_cloud_path(new_path);
        let descendants = self.list_descendants(&old_path)?;
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE files SET path = ?1, parent_path = ?2, name = ?3, pin_inheritance_blocked = 0, state = CASE WHEN state = 'error' AND cloud_remote_id IS NULL AND cache_path IS NOT NULL THEN 'dirty' ELSE state END WHERE remote_id = ?4",
            params![new_path, parent_cloud_path(&new_path), cloud_name(&new_path), local_id],
        )?;
        for descendant in descendants {
            let suffix = descendant
                .metadata
                .path
                .strip_prefix(&old_path)
                .ok_or_else(|| anyhow::anyhow!("descendant path lost its parent prefix"))?;
            let descendant_path = normalize_cloud_path(&format!("{new_path}{suffix}"));
            tx.execute(
                "UPDATE files SET path = ?1, parent_path = ?2, name = ?3, pin_inheritance_blocked = 0, state = CASE WHEN state = 'error' AND cloud_remote_id IS NULL AND cache_path IS NOT NULL THEN 'dirty' ELSE state END WHERE remote_id = ?4",
                params![
                    descendant_path,
                    parent_cloud_path(&descendant_path),
                    cloud_name(&descendant_path),
                    descendant.metadata.remote_id
                ],
            )?;
        }
        if record.cloud_remote_id.is_some() {
            tx.execute(
                r#"
                INSERT INTO pending_metadata_operations(local_id, kind, path, queued_unix)
                VALUES (?1, 'move', ?2, ?3)
                ON CONFLICT(local_id) DO UPDATE SET
                    kind = CASE
                        WHEN pending_metadata_operations.kind = 'create_folder' THEN 'create_folder'
                        ELSE 'move'
                    END,
                    path = excluded.path,
                    queued_unix = excluded.queued_unix
                "#,
                params![local_id, new_path, now_unix()],
            )?;
        } else if record.metadata.is_dir {
            tx.execute(
                "UPDATE pending_metadata_operations SET path = ?1, queued_unix = ?2 WHERE local_id = ?3",
                params![new_path, now_unix(), local_id],
            )?;
        }
        tx.commit()?;
        self.recompute_pin_inheritance()
    }

    pub fn replace_file_locally(
        &self,
        source_local_id: &str,
        target_local_id: &str,
        new_path: &str,
    ) -> anyhow::Result<FileRecord> {
        let source = self
            .get_by_remote_id(source_local_id)?
            .ok_or_else(|| anyhow::anyhow!("replacement source disappeared"))?;
        let target = self
            .get_by_remote_id(target_local_id)?
            .ok_or_else(|| anyhow::anyhow!("replacement target disappeared"))?;
        if source.metadata.is_dir || target.metadata.is_dir {
            anyhow::bail!("cannot replace a directory as a file");
        }
        let new_path = normalize_cloud_path(new_path);
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM pending_metadata_operations WHERE local_id IN (?1, ?2)",
            params![source_local_id, target_local_id],
        )?;
        if let Some(source_cloud_id) = source
            .cloud_remote_id
            .as_deref()
            .filter(|source_id| Some(*source_id) != target.cloud_remote_id.as_deref())
        {
            tx.execute(
                r#"
                INSERT INTO pending_deletes(remote_id, path, cache_path, queued_unix)
                VALUES (?1, ?2, NULL, ?3)
                ON CONFLICT(remote_id) DO UPDATE SET path = excluded.path, queued_unix = excluded.queued_unix
                "#,
                params![source_cloud_id, source.metadata.path, now_unix()],
            )?;
        }
        tx.execute(
            "DELETE FROM files WHERE remote_id = ?1",
            params![source_local_id],
        )?;
        tx.execute(
            r#"
            UPDATE files
            SET path = ?1,
                parent_path = ?2,
                name = ?3,
                size = ?4,
                modified_unix = ?5,
                state = CASE WHEN ?6 = 'writing' THEN 'writing' ELSE 'dirty' END,
                cache_path = ?7,
                cache_accessed_unix = ?8
            WHERE remote_id = ?9
            "#,
            params![
                new_path,
                parent_cloud_path(&new_path),
                cloud_name(&new_path),
                i64::try_from(source.metadata.size).unwrap_or(i64::MAX),
                source.metadata.modified_unix,
                source.state.as_str(),
                source
                    .cache_path
                    .as_ref()
                    .map(|path| path.to_string_lossy()),
                now_unix(),
                target_local_id,
            ],
        )?;
        tx.commit()?;
        self.recompute_pin_inheritance()?;
        self.get_by_remote_id(target_local_id)?
            .ok_or_else(|| anyhow::anyhow!("local replacement disappeared"))
    }

    pub fn pending_metadata_operation(
        &self,
        local_id: &str,
    ) -> anyhow::Result<Option<PendingMetadataOperation>> {
        let conn = self.connect()?;
        Ok(conn
            .query_row(
                "SELECT local_id, kind, path, queued_unix FROM pending_metadata_operations WHERE local_id = ?1",
                params![local_id],
                |row| {
                    let kind: String = row.get(1)?;
                    let kind = match kind.as_str() {
                        "create_folder" => PendingMetadataKind::CreateFolder,
                        "move" => PendingMetadataKind::Move,
                        _ => return Err(rusqlite::Error::InvalidQuery),
                    };
                    Ok(PendingMetadataOperation {
                        local_id: row.get(0)?,
                        kind,
                        path: row.get(2)?,
                        queued_unix: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn pending_metadata_operations(&self) -> anyhow::Result<Vec<PendingMetadataOperation>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT local_id FROM pending_metadata_operations ORDER BY queued_unix, local_id",
        )?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        ids.into_iter()
            .filter_map(|local_id| self.pending_metadata_operation(&local_id).transpose())
            .collect()
    }

    pub fn complete_metadata_operation(
        &self,
        local_id: &str,
        expected_path: &str,
        entry: &MetadataEntry,
    ) -> anyhow::Result<bool> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_path = tx
            .query_row(
                "SELECT path FROM files WHERE remote_id = ?1",
                params![local_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let still_current = current_path.as_deref() == Some(&normalize_cloud_path(expected_path));
        tx.execute(
            "UPDATE files SET cloud_remote_id = ?1, etag = ?2, modified_unix = CASE WHEN is_dir = 1 THEN ?3 ELSE modified_unix END, state = CASE WHEN ?4 = 1 AND state = 'dirty' THEN 'cached' ELSE state END WHERE remote_id = ?5",
            params![
                entry.remote_id,
                entry.etag,
                entry.modified_unix,
                entry.is_dir as i64,
                local_id
            ],
        )?;
        if still_current {
            tx.execute(
                "DELETE FROM pending_metadata_operations WHERE local_id = ?1 AND path = ?2",
                params![local_id, normalize_cloud_path(expected_path)],
            )?;
        }
        tx.commit()?;
        Ok(still_current)
    }

    pub fn allow_pin_inheritance(&self, remote_id: &str) -> anyhow::Result<()> {
        let conn = self.connect()?;
        let record = self.query_record(
            &conn,
            "SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id FROM files WHERE remote_id = ?1",
            params![remote_id],
        )?;
        let Some(record) = record else {
            anyhow::bail!("cannot change pin inheritance for unknown item {remote_id}");
        };
        conn.execute(
            r#"
            UPDATE files
            SET pin_inheritance_blocked = 0
            WHERE remote_id = ?1
               OR substr(path, 1, length(?2) + 1) = ?2 || '/'
            "#,
            params![remote_id, record.metadata.path],
        )?;
        drop(conn);
        self.recompute_pin_inheritance()
    }

    fn move_descendants(&self, old_path: &str, new_path: &str) -> anyhow::Result<()> {
        let descendants = self.list_descendants(old_path)?;
        if descendants.is_empty() {
            return Ok(());
        }
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for record in descendants {
            let suffix = record
                .metadata
                .path
                .strip_prefix(old_path)
                .ok_or_else(|| anyhow::anyhow!("descendant path lost its parent prefix"))?;
            let path = normalize_cloud_path(&format!("{new_path}{suffix}"));
            let parent_path = parent_cloud_path(&path);
            let name = cloud_name(&path);
            tx.execute(
                "UPDATE files SET path = ?1, parent_path = ?2, name = ?3 WHERE remote_id = ?4",
                params![path, parent_path, name, record.metadata.remote_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    // Re-read under a write transaction: the caller's snapshot may predate a save.
    pub fn release_record(&self, record: &FileRecord) -> anyhow::Result<bool> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = self.query_record(&tx,
            "SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id FROM files WHERE remote_id = ?1",
            params![record.metadata.remote_id])?;
        let Some(record) = current else {
            return Ok(false);
        };
        if record.metadata.is_dir
            || record.effective_pinned()
            || !matches!(record.state, FileState::Cached)
            || record.cloud_remote_id.is_none()
        {
            return Ok(false);
        }
        let cache_file = match record.cache_path.as_ref().map(fs::File::open).transpose() {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => return Err(err.into()),
        };
        if let Some(file) = &cache_file {
            match file.try_lock() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
                Err(err) => return Err(err.into()),
            }
        }
        if let Some(cache_path) = &record.cache_path {
            match fs::remove_file(cache_path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        }
        tx.execute(
            "UPDATE files SET state = 'online_only', cache_path = NULL, cache_accessed_unix = NULL, release_pending = 0 WHERE remote_id = ?1",
            params![record.metadata.remote_id],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn finish_pending_releases(&self) -> anyhow::Result<usize> {
        let conn = self.connect()?;
        let ids = conn
            .prepare("SELECT remote_id FROM files WHERE release_pending = 1")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(conn);
        let mut count = 0;
        for id in ids {
            if let Some(record) = self.get_by_remote_id(&id)? {
                count += usize::from(self.release_record(&record)?);
            }
        }
        Ok(count)
    }

    pub fn pending_release_count(&self, path: &str) -> anyhow::Result<usize> {
        let path = normalize_cloud_path(path);
        Ok(self.connect()?.query_row(
            "SELECT count(*) FROM files WHERE release_pending = 1 AND (path = ?1 OR ?1 = '/' OR substr(path, 1, length(?1) + 1) = ?1 || '/')",
            params![path], |row| row.get(0))?)
    }

    pub fn release_path(&self, path: &str) -> anyhow::Result<usize> {
        let Some(record) = self.get_by_path(path)? else {
            anyhow::bail!(
                "path is not in twodrive metadata: {}",
                normalize_cloud_path(path)
            );
        };

        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let cancelled: usize = tx.query_row(
            "SELECT count(*) FROM files WHERE is_dir = 0 AND pin_explicit = 0 AND pin_origin_remote_id IS NULL
             AND state = 'hydrating' AND cache_path IS NULL
             AND (path = ?1 OR ?1 = '/' OR substr(path, 1, length(?1) + 1) = ?1 || '/')",
            params![record.metadata.path], |row| row.get(0))?;
        tx.execute(
            "UPDATE files SET download_generation = download_generation + 1,
             release_pending = CASE WHEN state = 'online_only' OR (state = 'hydrating' AND cache_path IS NULL) THEN 0 ELSE 1 END,
             state = CASE WHEN state = 'hydrating' AND cache_path IS NULL THEN 'online_only' ELSE state END
             WHERE is_dir = 0 AND pin_explicit = 0 AND pin_origin_remote_id IS NULL
             AND (path = ?1 OR ?1 = '/' OR substr(path, 1, length(?1) + 1) = ?1 || '/')",
            params![record.metadata.path],
        )?;
        tx.commit()?;
        drop(conn);
        let records = if record.metadata.is_dir {
            self.list_descendants(&record.metadata.path)?
        } else {
            vec![record]
        };
        let mut released = cancelled;
        for record in records {
            released += usize::from(self.release_record(&record)?);
        }
        Ok(released)
    }

    pub fn prune_cache(&self, older_than_seconds: i64) -> anyhow::Result<usize> {
        let cutoff = now_unix().saturating_sub(older_than_seconds);
        let records = self
            .all_records()?
            .into_iter()
            .filter(|record| {
                record.state == FileState::Cached
                    && !record.effective_pinned()
                    && record
                        .cache_accessed_unix
                        .map(|accessed| accessed <= cutoff)
                        .unwrap_or(true)
            })
            .collect::<Vec<_>>();

        let mut pruned = 0;
        for record in records {
            if self.release_record(&record)? {
                pruned += 1;
            }
        }
        Ok(pruned)
    }

    pub fn get_by_path(&self, path: &str) -> anyhow::Result<Option<FileRecord>> {
        let conn = self.connect()?;
        let normalized = normalize_cloud_path(path);
        self.query_record(
            &conn,
            "SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id FROM files WHERE path = ?1",
            params![normalized],
        )
    }

    pub fn get_by_remote_id(&self, remote_id: &str) -> anyhow::Result<Option<FileRecord>> {
        let conn = self.connect()?;
        self.query_record(
            &conn,
            "SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id FROM files WHERE remote_id = ?1",
            params![remote_id],
        )
    }

    pub fn get_by_cloud_remote_id(
        &self,
        cloud_remote_id: &str,
    ) -> anyhow::Result<Option<FileRecord>> {
        let conn = self.connect()?;
        self.query_record(
            &conn,
            "SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id FROM files WHERE cloud_remote_id = ?1",
            params![cloud_remote_id],
        )
    }

    pub fn bind_cloud_identity(
        &self,
        local_id: &str,
        cloud_remote_id: &str,
        etag: &str,
    ) -> anyhow::Result<()> {
        let conn = self.connect()?;
        let changed = conn.execute(
            "UPDATE files SET cloud_remote_id = ?1, etag = ?2 WHERE remote_id = ?3",
            params![cloud_remote_id, etag, local_id],
        )?;
        if changed == 0 {
            anyhow::bail!("cannot bind cloud identity for unknown item {local_id}");
        }
        Ok(())
    }

    pub fn commit_uploaded(
        &self,
        local_id: &str,
        requested_path: &str,
        uploaded: &MetadataEntry,
        cache_path: &Path,
    ) -> anyhow::Result<FileRecord> {
        // Upload acknowledgement changes the cloud version, not the local
        // content's mtime. Readers such as Poppler reject saves if that mtime
        // changes after open. Matching delta echoes must preserve it as well.
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = self
            .query_record(
                &tx,
                "SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id FROM files WHERE remote_id = ?1",
                params![local_id],
            )?
            .ok_or_else(|| anyhow::anyhow!("uploaded local item disappeared: {local_id}"))?;
        let is_current_generation = current.metadata.path == normalize_cloud_path(requested_path)
            && current.state == FileState::Uploading
            && current.cache_path.as_deref() == Some(cache_path);
        let cloud_identity_matches = current
            .cloud_remote_id
            .as_deref()
            .is_none_or(|remote_id| remote_id == uploaded.remote_id);
        let needs_cloud_move = current.metadata.path != normalize_cloud_path(requested_path)
            && current.cloud_remote_id.is_none();
        tx.execute(
            r#"
            UPDATE files
            SET cloud_remote_id = CASE
                    WHEN ?3 = 1 OR cloud_remote_id IS NULL THEN ?1
                    ELSE cloud_remote_id
                END,
                etag = CASE WHEN ?9 = 1 THEN ?2 ELSE etag END,
                modified_unix = CASE WHEN ?3 = 1 THEN ?4 ELSE modified_unix END,
                size = CASE WHEN ?3 = 1 THEN ?5 ELSE size END,
                state = CASE
                    WHEN ?3 = 0 THEN state
                    WHEN pin_explicit = 1 OR pin_origin_remote_id IS NOT NULL THEN 'pinned'
                    ELSE 'cached'
                END,
                cache_path = CASE WHEN ?3 = 1 THEN ?6 ELSE cache_path END,
                cache_accessed_unix = CASE WHEN ?3 = 1 THEN ?7 ELSE cache_accessed_unix END
            WHERE remote_id = ?8
            "#,
            params![
                uploaded.remote_id,
                uploaded.etag,
                is_current_generation as i64,
                current.metadata.modified_unix,
                i64::try_from(uploaded.size).unwrap_or(i64::MAX),
                cache_path.to_string_lossy(),
                now_unix(),
                local_id,
                cloud_identity_matches as i64,
            ],
        )?;
        if needs_cloud_move {
            tx.execute(
                r#"
                INSERT INTO pending_metadata_operations(local_id, kind, path, queued_unix)
                VALUES (?1, 'move', ?2, ?3)
                ON CONFLICT(local_id) DO UPDATE SET
                    kind = 'move',
                    path = excluded.path,
                    queued_unix = excluded.queued_unix
                "#,
                params![local_id, current.metadata.path, now_unix()],
            )?;
        }
        tx.commit()?;
        self.get_by_remote_id(local_id)?
            .ok_or_else(|| anyhow::anyhow!("committed upload disappeared: {local_id}"))
    }

    pub fn list_children(&self, parent_path: &str) -> anyhow::Result<Vec<FileRecord>> {
        let conn = self.connect()?;
        let normalized = normalize_cloud_path(parent_path);
        let mut stmt = conn.prepare(
            r#"
            SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id
            FROM files
            WHERE parent_path = ?1
            ORDER BY is_dir DESC, lower(name), name
            "#,
        )?;
        let records = stmt
            .query_map(params![normalized], row_to_record)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(records)
    }

    pub fn list_descendants(&self, path: &str) -> anyhow::Result<Vec<FileRecord>> {
        let normalized = normalize_cloud_path(path);
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id
            FROM files
            WHERE (
                ?1 = '/' AND path <> '/' AND substr(path, 1, 1) = '/'
            ) OR (
                ?1 <> '/' AND substr(path, 1, length(?1) + 1) = ?1 || '/'
            )
            ORDER BY path
            "#,
        )?;
        let records = stmt
            .query_map(params![normalized], row_to_record)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(records)
    }

    pub fn all_records(&self) -> anyhow::Result<Vec<FileRecord>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT remote_id, path, parent_path, name, is_dir, size, modified_unix, etag, state, cache_path, cache_accessed_unix, pin_explicit, pin_origin_remote_id, pin_inheritance_blocked, cloud_remote_id
            FROM files
            ORDER BY path
            "#,
        )?;
        let records = stmt
            .query_map([], row_to_record)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(records)
    }

    pub fn remove_by_remote_id(&self, remote_id: &str) -> anyhow::Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM pending_metadata_operations WHERE local_id = ?1",
            params![remote_id],
        )?;
        tx.execute("DELETE FROM files WHERE remote_id = ?1", params![remote_id])?;
        tx.commit()?;
        self.recompute_pin_inheritance()
    }

    pub fn queue_pending_delete(&self, record: &FileRecord) -> anyhow::Result<()> {
        let cloud_remote_id = record
            .cloud_remote_id
            .as_deref()
            .unwrap_or(&record.metadata.remote_id);
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM pending_metadata_operations WHERE local_id = ?1",
            params![record.metadata.remote_id],
        )?;
        tx.execute(
            r#"
            INSERT INTO pending_deletes(remote_id, path, cache_path, queued_unix)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(remote_id) DO UPDATE SET
                path = excluded.path,
                cache_path = excluded.cache_path,
                queued_unix = excluded.queued_unix
            "#,
            params![
                cloud_remote_id,
                record.metadata.path,
                record
                    .cache_path
                    .as_ref()
                    .map(|path| path.to_string_lossy()),
                now_unix(),
            ],
        )?;
        if record.metadata.is_dir {
            tx.execute(
                r#"
                DELETE FROM files
                WHERE remote_id = ?1
                   OR path = ?2
                   OR substr(path, 1, length(?2) + 1) = ?2 || '/'
                "#,
                params![record.metadata.remote_id, record.metadata.path],
            )?;
        } else {
            tx.execute(
                "DELETE FROM files WHERE remote_id = ?1",
                params![record.metadata.remote_id],
            )?;
        }
        tx.commit()?;
        self.recompute_pin_inheritance()
    }

    pub fn queue_remote_delete(&self, cloud_remote_id: &str, path: &str) -> anyhow::Result<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            INSERT INTO pending_deletes(remote_id, path, cache_path, queued_unix)
            VALUES (?1, ?2, NULL, ?3)
            ON CONFLICT(remote_id) DO UPDATE SET path = excluded.path, queued_unix = excluded.queued_unix
            "#,
            params![cloud_remote_id, normalize_cloud_path(path), now_unix()],
        )?;
        Ok(())
    }

    pub fn pending_deletes(&self) -> anyhow::Result<Vec<PendingDelete>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT remote_id, path, cache_path, queued_unix FROM pending_deletes ORDER BY queued_unix, path",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(PendingDelete {
                    remote_id: row.get(0)?,
                    path: row.get(1)?,
                    cache_path: row.get::<_, Option<String>>(2)?.map(PathBuf::from),
                    queued_unix: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn remove_pending_delete(&self, remote_id: &str) -> anyhow::Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "DELETE FROM pending_deletes WHERE remote_id = ?1",
            params![remote_id],
        )?;
        Ok(())
    }

    pub fn is_pending_delete(&self, remote_id: &str, path: &str) -> anyhow::Result<bool> {
        let conn = self.connect()?;
        let path = normalize_cloud_path(path);
        Ok(conn
            .query_row(
                "SELECT 1 FROM pending_deletes WHERE remote_id = ?1 OR path = ?2 LIMIT 1",
                params![remote_id, path],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn get_state_value(&self, key: &str) -> anyhow::Result<Option<String>> {
        let conn = self.connect()?;
        Ok(conn
            .query_row(
                "SELECT value FROM app_state WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn set_state_value(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            INSERT INTO app_state(key, value)
            VALUES (?1, ?2)
            ON CONFLICT(key) DO UPDATE SET value = excluded.value
            "#,
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delta_link(&self) -> anyhow::Result<Option<String>> {
        self.get_state_value("onedrive_delta_link")
    }

    pub fn set_delta_link(&self, value: &str) -> anyhow::Result<()> {
        self.set_state_value("onedrive_delta_link", value)
    }

    fn connect(&self) -> anyhow::Result<Connection> {
        let conn = Connection::open(&self.db_path)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        Ok(conn)
    }

    fn query_record<P: rusqlite::Params>(
        &self,
        conn: &Connection,
        sql: &str,
        params: P,
    ) -> anyhow::Result<Option<FileRecord>> {
        Ok(conn.query_row(sql, params, row_to_record).optional()?)
    }
}

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileRecord> {
    let state_value: String = row.get(8)?;
    let cache_path: Option<String> = row.get(9)?;
    Ok(FileRecord {
        metadata: MetadataEntry {
            remote_id: row.get(0)?,
            path: row.get(1)?,
            parent_path: row.get(2)?,
            name: row.get(3)?,
            is_dir: row.get::<_, i64>(4)? != 0,
            size: row.get::<_, i64>(5)? as u64,
            modified_unix: row.get(6)?,
            etag: row.get(7)?,
        },
        cloud_remote_id: row.get(14)?,
        state: FileState::from_str(&state_value).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(err))
        })?,
        cache_path: cache_path.map(PathBuf::from),
        cache_accessed_unix: row.get(10)?,
        pin_explicit: row.get::<_, i64>(11)? != 0,
        pin_origin_remote_id: row.get(12)?,
        pin_inheritance_blocked: row.get::<_, i64>(13)? != 0,
    })
}

pub fn normalize_cloud_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return "/".to_string();
    }

    let mut normalized = String::from("/");
    normalized.push_str(trimmed.trim_matches('/'));
    normalized
}

pub fn join_cloud_path(parent: &str, name: &str) -> String {
    let parent = normalize_cloud_path(parent);
    if parent == "/" {
        normalize_cloud_path(name)
    } else {
        normalize_cloud_path(&format!("{parent}/{name}"))
    }
}

fn parent_cloud_path(path: &str) -> String {
    let path = normalize_cloud_path(path);
    if path == "/" {
        return "/".to_string();
    }

    match path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(index) => path[..index].to_string(),
    }
}

fn cloud_name(path: &str) -> String {
    let path = normalize_cloud_path(path);
    if path == "/" {
        return String::new();
    }

    path.rsplit('/').next().unwrap_or_default().to_string()
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    alter_sql: &str,
) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !columns.iter().any(|name| name == column)
        && let Err(err) = conn.execute_batch(alter_sql)
    {
        let mut retry = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let now_exists = retry
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|name| name == column);
        if !now_exists {
            return Err(err.into());
        }
    }
    Ok(())
}

fn database_init_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn ensure_dir(path: &Path) -> anyhow::Result<()> {
    match fs::create_dir_all(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err.into()),
    }
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

pub fn parse_duration_seconds(value: &str) -> anyhow::Result<i64> {
    let value = value.trim();
    if value.is_empty() {
        anyhow::bail!("duration is empty");
    }
    let (number, unit) = value.split_at(
        value
            .find(|ch: char| !ch.is_ascii_digit())
            .unwrap_or(value.len()),
    );
    let amount = number.parse::<i64>()?;
    let multiplier = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        other => anyhow::bail!("unsupported duration unit {other:?}; use s, m, h, or d"),
    };
    Ok(amount.saturating_mul(multiplier))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TestDatabase {
        db: Database,
        root: PathBuf,
    }

    impl TestDatabase {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "twodrive-core-{name}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&root).unwrap();
            let db = Database::new(root.join("test.sqlite3"));
            db.init().unwrap();
            Self { db, root }
        }
    }

    impl Drop for TestDatabase {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn add_dir(db: &Database, id: &str, path: &str) {
        db.upsert_metadata(&MetadataEntry::new_dir(id, path, 1, "etag"))
            .unwrap();
    }

    fn add_file(db: &Database, id: &str, path: &str) {
        db.upsert_metadata(&MetadataEntry::new_file(id, path, 4, 1, "etag"))
            .unwrap();
    }

    #[test]
    fn release_invalidates_downloads_and_wins_the_completion_race() {
        let test = TestDatabase::new("download-cancellation");
        add_dir(&test.db, "dir", "/folder");
        add_file(&test.db, "file", "/folder/file");
        add_file(&test.db, "sibling", "/folder-other");
        let generation = test.db.download_generation("file").unwrap();
        assert!(test.db.begin_hydration("file", generation).unwrap());
        let tmp = test.root.join("download.tmp");
        let cache = test.root.join("download");
        fs::write(&tmp, b"done").unwrap();
        assert_eq!(test.db.release_path("/folder").unwrap(), 1);
        assert_eq!(test.db.download_generation("sibling").unwrap(), 0);
        assert!(
            !test
                .db
                .finish_hydration("file", generation, &tmp, &cache)
                .unwrap()
        );
        assert!(!cache.exists());
        let reopened = Database::new(test.db.db_path.clone());
        reopened.init().unwrap();
        assert!(!reopened.begin_hydration("file", generation).unwrap());
        assert_eq!(
            reopened.get_by_remote_id("file").unwrap().unwrap().state,
            FileState::OnlineOnly
        );
        let next = reopened.download_generation("file").unwrap();
        assert!(reopened.begin_hydration("file", next).unwrap());
        assert!(
            reopened
                .finish_hydration("file", next, &tmp, &cache)
                .unwrap()
        );
        assert_eq!(fs::read(cache).unwrap(), b"done");
    }

    #[test]
    fn deferred_release_survives_restart_and_waits_for_latest_upload() {
        let test = TestDatabase::new("deferred-release");
        let id = "local-upload-deferred";
        let cache = test.root.join("content");
        fs::write(&cache, b"latest local contents").unwrap();
        test.db
            .upsert_metadata(&MetadataEntry::new_file(id, "/paper.pdf", 21, 1, ""))
            .unwrap();
        test.db.mark_cached(id, &cache).unwrap();
        for state in [
            FileState::Writing,
            FileState::Dirty,
            FileState::Uploading,
            FileState::Error,
            FileState::Conflict,
        ] {
            test.db.mark_state(id, state).unwrap();
            assert_eq!(test.db.release_path("/paper.pdf").unwrap(), 0);
            assert!(cache.exists());
        }
        let restarted = Database::new(test.db.db_path.clone());
        restarted.init().unwrap();
        assert_eq!(restarted.pending_release_count("/").unwrap(), 1);
        restarted.mark_state(id, FileState::Uploading).unwrap();
        restarted
            .commit_uploaded(
                id,
                "/paper.pdf",
                &MetadataEntry::new_file("cloud-paper", "/paper.pdf", 21, 2, "etag"),
                &cache,
            )
            .unwrap();
        assert_eq!(restarted.finish_pending_releases().unwrap(), 1);
        assert!(!cache.exists());
        let record = restarted.get_by_remote_id(id).unwrap().unwrap();
        assert_eq!(record.state, FileState::OnlineOnly);
        assert_eq!(record.cloud_remote_id.as_deref(), Some("cloud-paper"));
        assert_eq!(restarted.pending_release_count("/").unwrap(), 0);
    }

    #[test]
    fn renaming_failed_local_upload_requeues_without_losing_release_request() {
        let test = TestDatabase::new("rename-failed-release");
        let id = "local-upload-invalid";
        let cache = test.root.join("content");
        fs::write(&cache, b"safe").unwrap();
        test.db
            .upsert_metadata(&MetadataEntry::new_file(id, "/bad:name", 4, 1, ""))
            .unwrap();
        test.db.mark_cached(id, &cache).unwrap();
        test.db.mark_state(id, FileState::Error).unwrap();
        test.db.release_path("/bad:name").unwrap();
        test.db.move_subtree_and_queue(id, "/good-name").unwrap();
        assert_eq!(
            test.db.get_by_path("/good-name").unwrap().unwrap().state,
            FileState::Dirty
        );
        assert_eq!(test.db.pending_release_count("/good-name").unwrap(), 1);
        assert!(cache.exists());
    }

    #[test]
    fn release_rechecks_state_and_preserves_open_or_pinned_cache() {
        let test = TestDatabase::new("release-locks");
        let cache = test.root.join("content");
        fs::write(&cache, b"safe").unwrap();
        test.db
            .upsert_metadata(&MetadataEntry::new_file("cloud", "/file", 4, 1, "etag"))
            .unwrap();
        test.db.mark_cached("cloud", &cache).unwrap();
        let stale = test.db.get_by_path("/file").unwrap().unwrap();
        test.db.mark_state("cloud", FileState::Dirty).unwrap();
        assert!(!test.db.release_record(&stale).unwrap());
        test.db.mark_cached("cloud", &cache).unwrap();
        let reader = fs::File::open(&cache).unwrap();
        reader.lock_shared().unwrap();
        assert_eq!(test.db.release_path("/file").unwrap(), 0);
        assert_eq!(test.db.pending_release_count("/file").unwrap(), 1);
        test.db.set_explicit_pin("cloud", true).unwrap();
        assert_eq!(test.db.pending_release_count("/file").unwrap(), 0);
        drop(reader);
        assert_eq!(test.db.finish_pending_releases().unwrap(), 0);
        assert!(cache.exists());
        test.db.set_explicit_pin("cloud", false).unwrap();
        assert_eq!(test.db.release_path("/file").unwrap(), 1);
    }

    #[test]
    fn normalizes_cloud_paths() {
        assert_eq!(normalize_cloud_path(""), "/");
        assert_eq!(normalize_cloud_path("/"), "/");
        assert_eq!(normalize_cloud_path("Documents/a.txt"), "/Documents/a.txt");
        assert_eq!(join_cloud_path("/Documents", "a.txt"), "/Documents/a.txt");
    }

    #[test]
    fn locally_added_items_inherit_an_explicitly_pinned_ancestor() {
        let test = TestDatabase::new("new-inherits");
        add_dir(&test.db, "courses", "/Courses");
        test.db.set_explicit_pin("courses", true).unwrap();

        add_dir(&test.db, "week-1", "/Courses/Week 1");
        add_file(&test.db, "notes", "/Courses/Week 1/notes.txt");

        let week = test.db.get_by_remote_id("week-1").unwrap().unwrap();
        let notes = test.db.get_by_remote_id("notes").unwrap().unwrap();
        assert!(week.effective_pinned());
        assert!(notes.effective_pinned());
        assert_eq!(week.pin_origin_remote_id.as_deref(), Some("courses"));
        assert_eq!(notes.pin_origin_remote_id.as_deref(), Some("courses"));
    }

    #[test]
    fn newly_discovered_cloud_items_stay_online_only() {
        let test = TestDatabase::new("metadata-batch");
        add_dir(&test.db, "courses", "/Courses");
        test.db.set_explicit_pin("courses", true).unwrap();
        add_file(&test.db, "local-draft", "/Courses/draft.txt");
        test.db.mark_state("local-draft", FileState::Dirty).unwrap();

        let entries = vec![
            MetadataEntry::new_dir("week-1", "/Courses/Week 1", 1, "dir-etag"),
            MetadataEntry::new_file("notes", "/Courses/Week 1/notes.txt", 4, 1, "file-etag"),
            MetadataEntry::new_file("remote-draft", "/Courses/draft.txt", 99, 2, "remote-etag"),
        ];
        test.db.upsert_metadata_batch(&entries).unwrap();

        let notes = test.db.get_by_remote_id("notes").unwrap().unwrap();
        assert!(!notes.effective_pinned());
        assert!(notes.pin_inheritance_blocked);
        assert_eq!(notes.pin_origin_remote_id, None);
        assert_eq!(notes.state, FileState::OnlineOnly);
        let draft = test.db.get_by_path("/Courses/draft.txt").unwrap().unwrap();
        assert_eq!(draft.metadata.remote_id, "local-draft");
        assert_eq!(draft.state, FileState::Dirty);

        test.db.set_explicit_pin("courses", true).unwrap();
        let notes = test.db.get_by_remote_id("notes").unwrap().unwrap();
        assert!(notes.effective_pinned());
        assert!(!notes.pin_inheritance_blocked);
        assert_eq!(notes.pin_origin_remote_id.as_deref(), Some("courses"));
    }

    #[test]
    fn moving_a_subtree_recomputes_inherited_pin_policy() {
        let test = TestDatabase::new("move-inheritance");
        add_dir(&test.db, "pinned", "/Pinned");
        add_dir(&test.db, "other", "/Other");
        add_dir(&test.db, "project", "/Other/Project");
        add_file(&test.db, "readme", "/Other/Project/README.md");
        test.db.set_explicit_pin("pinned", true).unwrap();

        test.db.move_subtree("project", "/Pinned/Project").unwrap();
        assert!(
            test.db
                .get_by_remote_id("readme")
                .unwrap()
                .unwrap()
                .effective_pinned()
        );

        test.db.move_subtree("project", "/Other/Project").unwrap();
        assert!(
            !test
                .db
                .get_by_remote_id("readme")
                .unwrap()
                .unwrap()
                .effective_pinned()
        );
    }

    #[test]
    fn unpinning_parent_preserves_nested_explicit_pin() {
        let test = TestDatabase::new("nested-explicit");
        add_dir(&test.db, "parent", "/Parent");
        add_dir(&test.db, "nested", "/Parent/Nested");
        add_file(&test.db, "a", "/Parent/a.txt");
        add_file(&test.db, "b", "/Parent/Nested/b.txt");
        test.db.set_explicit_pin("parent", true).unwrap();
        test.db.set_explicit_pin("nested", true).unwrap();

        test.db.set_explicit_pin("parent", false).unwrap();

        assert!(
            !test
                .db
                .get_by_remote_id("a")
                .unwrap()
                .unwrap()
                .effective_pinned()
        );
        let nested = test.db.get_by_remote_id("nested").unwrap().unwrap();
        let child = test.db.get_by_remote_id("b").unwrap().unwrap();
        assert!(nested.pin_explicit);
        assert!(child.effective_pinned());
        assert_eq!(child.pin_origin_remote_id.as_deref(), Some("nested"));
    }

    #[test]
    fn remote_metadata_does_not_replace_a_dirty_local_generation() {
        let test = TestDatabase::new("dirty-generation");
        add_file(&test.db, "local-generation", "/draft.txt");
        let cache_path = test.root.join("draft.cache");
        fs::write(&cache_path, b"new local data").unwrap();
        test.db
            .mark_cached("local-generation", &cache_path)
            .unwrap();
        test.db
            .mark_state("local-generation", FileState::Dirty)
            .unwrap();

        add_file(&test.db, "remote-generation", "/draft.txt");

        let current = test.db.get_by_path("/draft.txt").unwrap().unwrap();
        assert_eq!(current.metadata.remote_id, "local-generation");
        assert_eq!(current.state, FileState::Dirty);
        assert_eq!(
            fs::read(current.cache_path.unwrap()).unwrap(),
            b"new local data"
        );
    }

    #[test]
    fn remote_metadata_for_same_id_does_not_mutate_a_dirty_generation() {
        let test = TestDatabase::new("dirty-same-id");
        add_file(&test.db, "same-id", "/draft.txt");
        let cache_path = test.root.join("draft.cache");
        fs::write(&cache_path, b"local data").unwrap();
        test.db.mark_cached("same-id", &cache_path).unwrap();
        test.db.mark_state("same-id", FileState::Dirty).unwrap();

        test.db
            .upsert_metadata(&MetadataEntry::new_file(
                "same-id",
                "/draft.txt",
                999,
                1234,
                "remote-etag",
            ))
            .unwrap();

        let current = test.db.get_by_remote_id("same-id").unwrap().unwrap();
        assert_eq!(current.state, FileState::Dirty);
        assert_eq!(current.metadata.size, 4);
        assert_eq!(current.metadata.etag, "etag");
        assert_eq!(current.cache_path.as_deref(), Some(cache_path.as_path()));
    }

    #[test]
    fn pending_delete_hides_item_and_blocks_delta_reappearance_until_recovered() {
        let test = TestDatabase::new("pending-delete");
        add_file(&test.db, "delete-me", "/delete-me.txt");
        let record = test.db.get_by_remote_id("delete-me").unwrap().unwrap();

        test.db.queue_pending_delete(&record).unwrap();
        assert!(test.db.get_by_path("/delete-me.txt").unwrap().is_none());
        assert_eq!(test.db.pending_deletes().unwrap().len(), 1);

        test.db
            .upsert_metadata(&MetadataEntry::new_file(
                "delete-me",
                "/delete-me.txt",
                10,
                2,
                "remote-still-there",
            ))
            .unwrap();
        assert!(test.db.get_by_path("/delete-me.txt").unwrap().is_none());
        assert_eq!(test.db.pending_deletes().unwrap().len(), 1);
    }

    #[test]
    fn pin_ancestry_uses_exact_case_and_path_boundaries() {
        let test = TestDatabase::new("path-boundaries");
        add_dir(&test.db, "foo", "/foo");
        add_dir(&test.db, "foobar", "/foobar");
        add_dir(&test.db, "upper", "/Foo");
        add_file(&test.db, "inside", "/foo/inside.txt");
        add_file(&test.db, "sibling", "/foobar/sibling.txt");
        add_file(&test.db, "case", "/Foo/case.txt");
        test.db.set_explicit_pin("foo", true).unwrap();

        assert!(
            test.db
                .get_by_remote_id("inside")
                .unwrap()
                .unwrap()
                .effective_pinned()
        );
        assert!(
            !test
                .db
                .get_by_remote_id("sibling")
                .unwrap()
                .unwrap()
                .effective_pinned()
        );
        assert!(
            !test
                .db
                .get_by_remote_id("case")
                .unwrap()
                .unwrap()
                .effective_pinned()
        );
    }

    #[test]
    fn legacy_pinned_states_migrate_to_a_single_explicit_root() {
        let root = std::env::temp_dir().join(format!(
            "twodrive-core-migration-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let db_path = root.join("legacy.sqlite3");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE files (
                remote_id TEXT PRIMARY KEY,
                path TEXT NOT NULL UNIQUE,
                parent_path TEXT NOT NULL,
                name TEXT NOT NULL,
                is_dir INTEGER NOT NULL,
                size INTEGER NOT NULL,
                modified_unix INTEGER NOT NULL,
                etag TEXT NOT NULL,
                state TEXT NOT NULL,
                cache_path TEXT,
                cache_accessed_unix INTEGER
            );
            INSERT INTO files VALUES
                ('root', '/Pinned', '/', 'Pinned', 1, 0, 1, 'e1', 'pinned', NULL, NULL),
                ('child', '/Pinned/a.txt', '/Pinned', 'a.txt', 0, 1, 1, 'e2', 'pinned', '/tmp/a', 1);
            "#,
        )
        .unwrap();
        drop(conn);

        let db = Database::new(&db_path);
        db.init().unwrap();
        let pinned_root = db.get_by_remote_id("root").unwrap().unwrap();
        let child = db.get_by_remote_id("child").unwrap().unwrap();
        assert!(pinned_root.pin_explicit);
        assert!(!child.pin_explicit);
        assert_eq!(child.pin_origin_remote_id.as_deref(), Some("root"));
        db.init().unwrap();

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_database_initialization_is_idempotent() {
        let root = std::env::temp_dir().join(format!(
            "twodrive-core-concurrent-init-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let db_path = root.join("concurrent.sqlite3");
        let workers = (0..8)
            .map(|_| {
                let db_path = db_path.clone();
                std::thread::spawn(move || Database::new(db_path).init())
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }
        let db = Database::new(db_path);
        db.init().unwrap();
        assert_eq!(
            db.get_state_value("pin_policy_v1_migrated").unwrap(),
            Some("1".to_string())
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_metadata_writers_wait_instead_of_failing_snapshot_upgrade() {
        let root = std::env::temp_dir().join(format!(
            "twodrive-core-concurrent-writes-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let db = Database::new(root.join("concurrent.sqlite3"));
        db.init().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let workers = (0..8)
            .map(|worker| {
                let db = db.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || -> anyhow::Result<()> {
                    barrier.wait();
                    for index in 0..20 {
                        let id = format!("worker-{worker}-file-{index}");
                        db.upsert_metadata(&MetadataEntry::new_file(
                            &id,
                            format!("/{id}.txt"),
                            index,
                            index as i64,
                            format!("etag-{id}"),
                        ))?;
                    }
                    Ok(())
                })
            })
            .collect::<Vec<_>>();

        for worker in workers {
            worker.join().unwrap().unwrap();
        }
        assert_eq!(db.all_records().unwrap().len(), 160);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn upload_binding_preserves_the_local_identity() {
        let test = TestDatabase::new("stable-local-identity");
        let local_id = "local-upload-stable";
        let cache_path = test.root.join("stable.cache");
        fs::write(&cache_path, b"local data").unwrap();
        test.db
            .upsert_metadata(&MetadataEntry::new_file(local_id, "/stable.pdf", 10, 1, ""))
            .unwrap();
        test.db.mark_cached(local_id, &cache_path).unwrap();
        test.db.mark_state(local_id, FileState::Uploading).unwrap();

        let uploaded = MetadataEntry::new_file("cloud-stable", "/stable.pdf", 10, 2, "etag-cloud");
        let committed = test
            .db
            .commit_uploaded(local_id, "/stable.pdf", &uploaded, &cache_path)
            .unwrap();

        assert_eq!(committed.metadata.remote_id, local_id);
        assert_eq!(committed.cloud_remote_id.as_deref(), Some("cloud-stable"));
        assert_eq!(committed.state, FileState::Cached);
        assert_eq!(committed.metadata.modified_unix, 1);
        test.db.upsert_metadata(&uploaded).unwrap();
        assert_eq!(
            test.db
                .get_by_remote_id(local_id)
                .unwrap()
                .unwrap()
                .metadata
                .modified_unix,
            1
        );
        test.db.upsert_metadata_batch([&uploaded]).unwrap();
        assert_eq!(
            test.db
                .get_by_remote_id(local_id)
                .unwrap()
                .unwrap()
                .metadata
                .modified_unix,
            1
        );
        let mut remote_edit = uploaded.clone();
        remote_edit.etag = "etag-real-edit".to_string();
        remote_edit.modified_unix = 3;
        test.db.upsert_metadata_batch([&remote_edit]).unwrap();
        assert_eq!(
            test.db
                .get_by_remote_id(local_id)
                .unwrap()
                .unwrap()
                .metadata
                .modified_unix,
            3
        );
        assert!(test.db.get_by_remote_id("cloud-stable").unwrap().is_none());
    }

    #[test]
    fn upload_completion_after_a_local_move_queues_the_remote_move() {
        let test = TestDatabase::new("upload-finished-after-move");
        let local_id = "local-upload-moving";
        let cache_path = test.root.join("moving.cache");
        fs::write(&cache_path, b"local data").unwrap();
        test.db
            .upsert_metadata(&MetadataEntry::new_file(local_id, "/before.pdf", 10, 1, ""))
            .unwrap();
        test.db.mark_cached(local_id, &cache_path).unwrap();
        test.db.mark_state(local_id, FileState::Uploading).unwrap();
        test.db
            .move_subtree_and_queue(local_id, "/after.pdf")
            .unwrap();

        let uploaded = MetadataEntry::new_file("cloud-moving", "/before.pdf", 10, 2, "etag-cloud");
        let committed = test
            .db
            .commit_uploaded(local_id, "/before.pdf", &uploaded, &cache_path)
            .unwrap();

        assert_eq!(committed.metadata.path, "/after.pdf");
        assert_eq!(committed.cloud_remote_id.as_deref(), Some("cloud-moving"));
        let pending = test
            .db
            .pending_metadata_operation(local_id)
            .unwrap()
            .unwrap();
        assert_eq!(pending.kind, PendingMetadataKind::Move);
        assert_eq!(pending.path, "/after.pdf");
    }

    #[test]
    fn stale_temp_upload_is_discarded_after_replacement() {
        let test = TestDatabase::new("stale-temp-upload-etag");
        let source_id = "local-upload-temp";
        let cache_path = test.root.join("temp.cache");
        fs::write(&cache_path, b"replacement").unwrap();
        test.db
            .upsert_metadata(&MetadataEntry::new_file(
                source_id,
                "/.target.pdf.tmp",
                11,
                1,
                "",
            ))
            .unwrap();
        test.db.mark_cached(source_id, &cache_path).unwrap();
        test.db.mark_state(source_id, FileState::Uploading).unwrap();
        test.db
            .upsert_metadata(&MetadataEntry::new_file(
                "cloud-target",
                "/target.pdf",
                4,
                1,
                "etag-target",
            ))
            .unwrap();
        test.db
            .replace_file_locally(source_id, "cloud-target", "/target.pdf")
            .unwrap();

        let stale_upload =
            MetadataEntry::new_file("cloud-temp", "/.target.pdf.tmp", 11, 2, "etag-temp");
        let error = test
            .db
            .commit_uploaded(source_id, "/.target.pdf.tmp", &stale_upload, &cache_path)
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("uploaded local item disappeared")
        );
        let target = test.db.get_by_remote_id("cloud-target").unwrap().unwrap();
        assert_eq!(target.cloud_remote_id.as_deref(), Some("cloud-target"));
        assert_eq!(target.metadata.etag, "etag-target");
        assert_eq!(target.metadata.path, "/target.pdf");
        assert_eq!(target.cache_path.as_deref(), Some(cache_path.as_path()));
    }

    #[test]
    fn replacement_preserves_target_identity_during_inflight_upload() {
        let test = TestDatabase::new("replace-inflight-target");
        let target_id = "local-upload-target";
        let source_id = "local-upload-temp";
        let old_cache = test.root.join("old.cache");
        let new_cache = test.root.join("new.cache");
        fs::write(&old_cache, b"initial generation").unwrap();
        fs::write(&new_cache, b"new generation").unwrap();
        test.db
            .upsert_metadata(&MetadataEntry::new_file(
                target_id,
                "/target.pdf",
                18,
                1,
                "",
            ))
            .unwrap();
        test.db.mark_cached(target_id, &old_cache).unwrap();
        test.db.mark_state(target_id, FileState::Uploading).unwrap();
        test.db
            .upsert_metadata(&MetadataEntry::new_file(
                source_id,
                "/.target.pdf.tmp",
                14,
                2,
                "",
            ))
            .unwrap();
        test.db.mark_cached(source_id, &new_cache).unwrap();
        test.db.mark_state(source_id, FileState::Writing).unwrap();

        let replaced = test
            .db
            .replace_file_locally(source_id, target_id, "/target.pdf")
            .unwrap();
        assert_eq!(replaced.metadata.remote_id, target_id);
        assert_eq!(replaced.cache_path.as_deref(), Some(new_cache.as_path()));
        assert_eq!(replaced.state, FileState::Writing);
        assert!(test.db.get_by_remote_id(source_id).unwrap().is_none());

        let initial_upload =
            MetadataEntry::new_file("cloud-target", "/target.pdf", 18, 3, "etag-initial");
        let after_initial = test
            .db
            .commit_uploaded(target_id, "/target.pdf", &initial_upload, &old_cache)
            .unwrap();
        assert_eq!(after_initial.metadata.remote_id, target_id);
        assert_eq!(
            after_initial.cloud_remote_id.as_deref(),
            Some("cloud-target")
        );
        assert_eq!(
            after_initial.cache_path.as_deref(),
            Some(new_cache.as_path())
        );
        assert_eq!(after_initial.metadata.size, 14);
        assert_eq!(after_initial.state, FileState::Writing);

        test.db.mark_dirty_with_size(target_id, 14).unwrap();
        test.db.mark_state(target_id, FileState::Uploading).unwrap();
        let latest_upload =
            MetadataEntry::new_file("cloud-target", "/target.pdf", 14, 4, "etag-latest");
        let committed = test
            .db
            .commit_uploaded(target_id, "/target.pdf", &latest_upload, &new_cache)
            .unwrap();
        assert_eq!(committed.cache_path.as_deref(), Some(new_cache.as_path()));
        assert_eq!(committed.metadata.etag, "etag-latest");
        assert_eq!(committed.state, FileState::Cached);
    }

    #[test]
    fn upload_claim_and_acknowledgement_reject_replaced_cache() {
        let test = TestDatabase::new("upload-claim-replaced-cache");
        let old_cache = test.root.join("empty.cache");
        let new_cache = test.root.join("complete.cache");
        for (id, path, cache, bytes) in [
            (
                "local-upload-target",
                "/target.pdf",
                &old_cache,
                b"".as_slice(),
            ),
            (
                "local-upload-source",
                "/target.part",
                &new_cache,
                b"PDF contents".as_slice(),
            ),
        ] {
            fs::write(cache, bytes).unwrap();
            test.db
                .upsert_metadata(&MetadataEntry::new_file(
                    id,
                    path,
                    bytes.len() as u64,
                    1,
                    "",
                ))
                .unwrap();
            test.db.mark_cached(id, cache).unwrap();
            test.db
                .mark_dirty_with_size(id, bytes.len() as u64)
                .unwrap();
        }
        let stale = test.db.get_by_path("/target.pdf").unwrap().unwrap();
        test.db
            .mark_state("local-upload-target", FileState::Writing)
            .unwrap();
        assert!(!test.db.begin_upload(&stale).unwrap());
        test.db
            .mark_state("local-upload-target", FileState::Dirty)
            .unwrap();
        assert!(test.db.begin_upload(&stale).unwrap());
        let replaced = test
            .db
            .replace_file_locally("local-upload-source", "local-upload-target", "/target.pdf")
            .unwrap();
        assert!(!test.db.begin_upload(&stale).unwrap());
        assert!(test.db.begin_upload(&replaced).unwrap());
        let old_ack = MetadataEntry::new_file("cloud-target", "/target.pdf", 0, 2, "old-etag");
        let current = test
            .db
            .commit_uploaded("local-upload-target", "/target.pdf", &old_ack, &old_cache)
            .unwrap();
        assert_eq!(current.cache_path.as_ref(), Some(&new_cache));
        assert_eq!(current.metadata.size, 12);
        assert_eq!(current.state, FileState::Uploading);
        assert!(!test.db.begin_upload(&stale).unwrap());
    }

    #[test]
    fn metadata_operations_are_durable_and_moves_coalesce() {
        let test = TestDatabase::new("durable-metadata-operations");
        let directory = test
            .db
            .create_local_directory("local-upload-folder", "/Draft")
            .unwrap();
        assert!(directory.cloud_remote_id.is_none());
        test.db
            .move_subtree_and_queue("local-upload-folder", "/Final")
            .unwrap();

        let reopened = Database::new(test.db.path().to_path_buf());
        reopened.init().unwrap();
        let operations = reopened.pending_metadata_operations().unwrap();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].kind, PendingMetadataKind::CreateFolder);
        assert_eq!(operations[0].path, "/Final");
        assert_eq!(
            reopened
                .get_by_remote_id("local-upload-folder")
                .unwrap()
                .unwrap()
                .metadata
                .path,
            "/Final"
        );
    }

    #[test]
    fn delta_metadata_does_not_undo_a_pending_local_move() {
        let test = TestDatabase::new("pending-move-protection");
        let remote = MetadataEntry::new_file("cloud-move", "/Old.pdf", 12, 1, "etag-1");
        test.db.upsert_metadata(&remote).unwrap();
        test.db
            .move_subtree_and_queue("cloud-move", "/New.pdf")
            .unwrap();

        test.db.upsert_metadata(&remote).unwrap();

        assert!(test.db.get_by_path("/Old.pdf").unwrap().is_none());
        let moved = test.db.get_by_path("/New.pdf").unwrap().unwrap();
        assert_eq!(moved.cloud_remote_id.as_deref(), Some("cloud-move"));
        assert!(
            test.db
                .pending_metadata_operation(&moved.metadata.remote_id)
                .unwrap()
                .is_some()
        );
    }
}
