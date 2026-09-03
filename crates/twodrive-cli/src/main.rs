use std::env;
use std::path::Path;
use std::process::Command;
use twodrive_backend::{GraphBackend, MockBackend};
use twodrive_core::{AppPaths, Config, Database, FileState, TokenStore, normalize_cloud_path};
use twodrive_fs::{
    hydrate_pending_pins, mount_graph, mount_mock, pin_path, recover_dirty_uploads,
    recover_pending_deletes, sync_delta_metadata, sync_metadata, unpin_path,
};

fn main() -> anyhow::Result<()> {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    let command = args.first().cloned().unwrap_or_else(|| "help".to_string());
    if !args.is_empty() {
        args.remove(0);
    }
    let paths = AppPaths::discover()?;

    match command.as_str() {
        "login" => GraphBackend::login(&paths),
        "logout" => logout(&paths),
        "status" => print_status(&paths),
        "sync" => sync(&paths),
        "mount" => mount(paths),
        "pin" => pin(&paths, required_path(&args)?),
        "unpin" => unpin(&paths, required_path(&args)?),
        "release" => release(&paths, required_path(&args)?),
        "cache" if args.first().map(String::as_str) == Some("prune") => prune_cache(&paths),
        "status-path" => status_path(&paths, required_path(&args)?),
        "open-folder" => open_folder(&paths),
        "settings" => open_settings(),
        "init-mock" => init_mock(&paths),
        "mount-mock" => mount_mock(paths),
        "version" | "--version" | "-V" => {
            println!("twodrive {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "help" | "-h" | "--help" => {
            print_help();
            Ok(())
        }
        other => {
            print_help();
            anyhow::bail!("unknown command: {other}")
        }
    }
}

fn print_status(paths: &AppPaths) -> anyhow::Result<()> {
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

fn status_path(paths: &AppPaths, path: &str) -> anyhow::Result<()> {
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
    println!(
        "cache_path={}",
        record
            .cache_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default()
    );
    println!("emblem={}", emblem_for_state(record.state));
    Ok(())
}

fn open_folder(paths: &AppPaths) -> anyhow::Result<()> {
    Command::new("xdg-open").arg(&paths.mount_dir).spawn()?;
    Ok(())
}

fn open_settings() -> anyhow::Result<()> {
    Command::new("twodrive-settings").spawn()?;
    Ok(())
}

fn init_mock(paths: &AppPaths) -> anyhow::Result<()> {
    paths.ensure()?;
    let db = Database::new(paths.db_path.clone());
    db.init()?;
    let count = sync_metadata(&db, &MockBackend::new())?;
    println!(
        "loaded {count} mock metadata entries into {}",
        db.path().display()
    );
    Ok(())
}

fn sync(paths: &AppPaths) -> anyhow::Result<()> {
    paths.ensure()?;
    let db = Database::new(paths.db_path.clone());
    db.init()?;
    if use_mock_backend() {
        let backend = MockBackend::new();
        let deleted = recover_pending_deletes(&db, &backend)?;
        let count = sync_metadata(&db, &backend)?;
        let recovered = recover_dirty_uploads(&db, &backend)?;
        println!(
            "synced {count} mock metadata entries; recovered {deleted} delete(s), {recovered} upload(s); file contents were not downloaded"
        );
        return Ok(());
    }

    let backend = GraphBackend::from_paths(paths)?;
    let deleted = recover_pending_deletes(&db, &backend)?;
    let count = sync_delta_metadata(&db, &backend)?;
    let recovered = recover_dirty_uploads(&db, &backend)?;
    let hydrated = hydrate_pending_pins(&db, &paths.cache_dir, &backend)?;
    println!(
        "synced {count} metadata entries; recovered {deleted} delete(s), {recovered} upload(s); hydrated {hydrated} inherited pinned file(s)"
    );
    Ok(())
}

fn mount(paths: AppPaths) -> anyhow::Result<()> {
    if use_mock_backend() {
        mount_mock(paths)
    } else {
        mount_graph(paths)
    }
}

fn pin(paths: &AppPaths, path: &str) -> anyhow::Result<()> {
    paths.ensure()?;
    let db = Database::new(paths.db_path.clone());
    db.init()?;
    if use_mock_backend() {
        let backend = MockBackend::new();
        let count = pin_path(&db, &paths.cache_dir, &backend, path)?;
        println!("pinned {path}; hydrated {count} mock file(s)");
        return Ok(());
    }

    let backend = GraphBackend::from_paths(paths)?;
    let count = pin_path(&db, &paths.cache_dir, &backend, path)?;
    println!("pinned {path}; hydrated {count} file(s)");
    Ok(())
}

fn unpin(paths: &AppPaths, path: &str) -> anyhow::Result<()> {
    let db = Database::new(paths.db_path.clone());
    db.init()?;
    let count = unpin_path(&db, path)?;
    println!("unpinned {path}; cache was left in place for {count} item(s)");
    Ok(())
}

fn release(paths: &AppPaths, path: &str) -> anyhow::Result<()> {
    let db = Database::new(paths.db_path.clone());
    db.init()?;
    let count = db.release_path(path)?;
    println!("released {count} cached file(s) under {path}; cloud files were not deleted");
    if count == 0 {
        println!(
            "nothing changed: the item may already be online-only, pinned, hydrating, uploading, dirty, or busy"
        );
    }
    Ok(())
}

fn prune_cache(paths: &AppPaths) -> anyhow::Result<()> {
    let config = Config::load_or_create(paths)?;
    let db = Database::new(paths.db_path.clone());
    db.init()?;
    let count = db.prune_cache(config.cache_retain_seconds()?)?;
    println!(
        "pruned {count} cached file(s); pinned/dirty/uploading/hydrating files were preserved"
    );
    Ok(())
}

fn logout(paths: &AppPaths) -> anyhow::Result<()> {
    TokenStore::new(paths.token_path.clone()).delete()?;
    println!("logged out; local token fallback file removed");
    Ok(())
}

fn required_path(args: &[String]) -> anyhow::Result<&str> {
    args.first()
        .map(String::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing cloud path argument, for example /Documents/a.pdf"))
}

fn cloud_path_from_arg(paths: &AppPaths, value: &str) -> String {
    let path = Path::new(value);
    if path.is_absolute()
        && let Ok(stripped) = path.strip_prefix(&paths.mount_dir)
    {
        return normalize_cloud_path(&stripped.to_string_lossy());
    }
    normalize_cloud_path(value)
}

fn emblem_for_state(state: FileState) -> &'static str {
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

fn use_mock_backend() -> bool {
    env::var("TWODRIVE_BACKEND").as_deref() == Ok("mock")
}

fn print_help() {
    println!(
        "twodrive\n\nCommands:\n  login              Sign in to OneDrive with OAuth2 authorization code + PKCE\n  logout             Remove the local token fallback file\n  status             Show paths, token state, and delta_link state\n  sync               Run OneDrive delta sync and hydrate inherited pinned files\n  mount              Mount the OneDrive FUSE filesystem\n  pin <path>         Persist an always-keep policy and download existing files\n  unpin <path>       Remove this item's explicit policy without deleting cache\n  release <path>     Delete local cache for non-pinned files only\n  cache prune        Remove expired cached files, preserving pinned files\n  status-path <path> Show local SQLite state for Nautilus/tray integration\n  open-folder        Open the TwoDrive folder with xdg-open\n  settings           Open the GTK settings helper\n  init-mock          Initialize SQLite with mock metadata\n  mount-mock         Mount the mock OneDrive filesystem\n  version            Show the installed version"
    );
}
