use std::process::Command;
use twodrive_core::{AppPaths, Config, Database, FileState};

use crate::arguments::cloud_path_from_arg;

pub(crate) fn print_status(paths: &AppPaths) -> anyhow::Result<()> {
    let config = Config::load_or_create(paths)?;
    let db = Database::new(paths.db_path.clone());
    db.init()?;

    println!("config: {}", paths.config_dir.display());
    println!("config file: {}", paths.config_path.display());
    println!("data:   {}", paths.data_dir.display());
    println!("cache:  {}", paths.cache_dir.display());
    println!("db:     {}", paths.db_path.display());
    println!("mount:  {}", paths.mount_dir.display());
    println!("cache retain: {}", config.cache.retain_for);
    println!("cache max: {}", config.cache.max_size);
    println!("sync interval ac: {}", config.power.ac_sync_interval);
    println!(
        "sync interval battery: {}",
        config.power.battery_sync_interval
    );
    println!(
        "download concurrency ac/battery: {}/{}",
        config.power.ac_download_concurrency, config.power.battery_download_concurrency
    );
    println!(
        "upload concurrency ac/battery: {}/{}",
        config.power.ac_upload_concurrency, config.power.battery_upload_concurrency
    );
    println!("known folders: {}", config.known_folders.enabled);
    println!("known folders mode: {}", config.known_folders.mode);
    println!("known folders debounce: {}", config.known_folders.debounce);
    println!(
        "known folders rescan: {}",
        config.known_folders.rescan_interval
    );
    println!(
        "known folders startup scan: {}",
        config.known_folders.startup_scan
    );
    println!(
        "known folders upload deletes: {}",
        config.known_folders.upload_deletes
    );
    for folder in &config.known_folders.folders {
        println!("known folder: {} -> {}", folder.local, folder.remote);
    }
    println!(
        "delta_link: {}",
        if db.delta_link()?.is_some() {
            "(stored)"
        } else {
            "(none)"
        }
    );
    println!("pending deletes: {}", db.pending_deletes()?.len());
    println!(
        "pending metadata operations: {}",
        db.pending_metadata_operations()?.len()
    );
    println!(
        "token: {}",
        if paths.token_path.exists() {
            paths.token_path.display().to_string()
        } else {
            "(not logged in)".to_string()
        }
    );
    Ok(())
}

pub(crate) fn status_path(paths: &AppPaths, path: &str) -> anyhow::Result<()> {
    let db = Database::new(paths.db_path.clone());
    db.init()?;
    let cloud_path = cloud_path_from_arg(paths, path);
    let Some(record) = db.get_by_path(&cloud_path)? else {
        println!("path={cloud_path}");
        println!("state=unknown");
        println!("emblem=emblem-twodrive-error");
        return Ok(());
    };

    println!("path={}", record.metadata.path);
    println!("name={}", record.metadata.name);
    println!("state={}", record.state);
    println!("pin_explicit={}", record.pin_explicit);
    println!("pin_inheritance_blocked={}", record.pin_inheritance_blocked);
    println!("effective_pinned={}", record.effective_pinned());
    println!(
        "pin_origin={}",
        record.pin_origin_remote_id.as_deref().unwrap_or_default()
    );
    println!("is_dir={}", record.metadata.is_dir);
    println!("size={}", record.metadata.size);
    println!("local_id={}", record.metadata.remote_id);
    println!(
        "cloud_remote_id={}",
        record.cloud_remote_id.as_deref().unwrap_or_default()
    );
    let metadata_pending = db
        .pending_metadata_operation(&record.metadata.remote_id)?
        .is_some();
    println!("metadata_operation_pending={metadata_pending}");
    println!(
        "cache_path={}",
        record
            .cache_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default()
    );
    println!(
        "emblem={}",
        if metadata_pending {
            "emblem-twodrive-syncing"
        } else {
            emblem_for_state(record.state)
        }
    );
    Ok(())
}

pub(crate) fn open_folder(paths: &AppPaths) -> anyhow::Result<()> {
    Command::new("xdg-open").arg(&paths.mount_dir).spawn()?;
    Ok(())
}

pub(crate) fn open_settings() -> anyhow::Result<()> {
    Command::new("twodrive-settings").spawn()?;
    Ok(())
}

pub(crate) fn emblem_for_state(state: FileState) -> &'static str {
    match state {
        FileState::OnlineOnly => "emblem-twodrive-cloud",
        FileState::Hydrating | FileState::Writing | FileState::Uploading => {
            "emblem-twodrive-syncing"
        }
        FileState::Cached => "emblem-twodrive-synced",
        FileState::Pinned => "emblem-twodrive-pinned",
        FileState::Dirty => "emblem-documents",
        FileState::Conflict | FileState::Error => "emblem-twodrive-error",
    }
}

pub(crate) fn print_help() {
    println!(
        "twodrive\n\nCommands:\n  login              Sign in to OneDrive with OAuth2 authorization code + PKCE\n  logout             Remove the local token fallback file\n  status             Show paths, token state, and delta_link state\n  sync               Run OneDrive delta sync and hydrate inherited pinned files\n  mount              Mount the OneDrive FUSE filesystem\n  pin <path>         Persist an always-keep policy and download existing files\n  unpin <path>       Remove this item's explicit policy without deleting cache\n  release <path>     Cancel pin policy and safely release selected local cache\n  cache prune        Remove expired cached files, preserving pinned files\n  status-path <path> Show local SQLite state for Nautilus/tray integration\n  open-folder        Open the TwoDrive folder with xdg-open\n  settings           Open the GTK settings helper\n  init-mock          Initialize SQLite with mock metadata\n  mount-mock         Mount the mock OneDrive filesystem\n  version            Show the installed version"
    );
}
