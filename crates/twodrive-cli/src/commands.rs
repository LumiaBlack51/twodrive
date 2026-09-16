use std::env;
use twodrive_backend::{GraphBackend, MockBackend};
use twodrive_core::{AppPaths, Config, Database, TokenStore};
use twodrive_fs::{
    hydrate_pending_pins, mount_graph, mount_mock, pin_path, recover_dirty_uploads,
    recover_pending_deletes, recover_pending_metadata_operations, sync_delta_metadata,
    sync_metadata, unpin_path,
};

pub(crate) fn init_mock(paths: &AppPaths) -> anyhow::Result<()> {
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

pub(crate) fn sync(paths: &AppPaths) -> anyhow::Result<()> {
    paths.ensure()?;
    let db = Database::new(paths.db_path.clone());
    db.init()?;
    if use_mock_backend() {
        let backend = MockBackend::new();
        let deleted = recover_pending_deletes(&db, &backend)?;
        let count = sync_metadata(&db, &backend)?;
        let metadata = recover_pending_metadata_operations(&db, &backend)?;
        let recovered = recover_dirty_uploads(&db, &backend)?;
        println!(
            "synced {count} mock metadata entries; recovered {deleted} delete(s), {metadata} metadata operation(s), {recovered} upload(s); file contents were not downloaded"
        );
        return Ok(());
    }

    let backend = GraphBackend::from_paths(paths)?;
    let deleted = recover_pending_deletes(&db, &backend)?;
    let count = sync_delta_metadata(&db, &backend)?;
    let metadata = recover_pending_metadata_operations(&db, &backend)?;
    let recovered = recover_dirty_uploads(&db, &backend)?;
    let hydrated = hydrate_pending_pins(&db, &paths.cache_dir, &backend)?;
    println!(
        "synced {count} metadata entries; recovered {deleted} delete(s), {metadata} metadata operation(s), {recovered} upload(s); hydrated {hydrated} inherited pinned file(s)"
    );
    Ok(())
}

pub(crate) fn mount(paths: AppPaths) -> anyhow::Result<()> {
    if use_mock_backend() {
        mount_mock(paths)
    } else {
        mount_graph(paths)
    }
}

pub(crate) fn pin(paths: &AppPaths, path: &str) -> anyhow::Result<()> {
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

pub(crate) fn unpin(paths: &AppPaths, path: &str) -> anyhow::Result<()> {
    let db = Database::new(paths.db_path.clone());
    db.init()?;
    let count = unpin_path(&db, path)?;
    println!("unpinned {path}; cache was left in place for {count} item(s)");
    Ok(())
}

pub(crate) fn release(paths: &AppPaths, path: &str) -> anyhow::Result<()> {
    let db = Database::new(paths.db_path.clone());
    db.init()?;
    let count = db.release_path(path)?;
    println!(
        "released {count} cached file(s) or cancelled download(s) under {path}; cloud files were not deleted"
    );
    let pending = db.pending_release_count(path)?;
    println!("queued {pending} file(s) for release after successful sync and closing open handles");
    if count == 0 && pending == 0 {
        println!("nothing changed: items are already online-only or pinned");
    }
    Ok(())
}

pub(crate) fn prune_cache(paths: &AppPaths) -> anyhow::Result<()> {
    let config = Config::load_or_create(paths)?;
    let db = Database::new(paths.db_path.clone());
    db.init()?;
    let count = db.prune_cache(config.cache_retain_seconds()?)?;
    println!(
        "pruned {count} cached file(s); pinned/dirty/uploading/hydrating files were preserved"
    );
    Ok(())
}

pub(crate) fn logout(paths: &AppPaths) -> anyhow::Result<()> {
    TokenStore::new(paths.token_path.clone()).delete()?;
    println!("logged out; local token fallback file removed");
    Ok(())
}

pub(crate) fn use_mock_backend() -> bool {
    env::var("TWODRIVE_BACKEND").as_deref() == Ok("mock")
}
