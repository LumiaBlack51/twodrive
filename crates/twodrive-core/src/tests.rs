use rusqlite::Connection;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

#[test]
fn graph_login_defaults_and_legacy_configs_use_shared_app() {
    let default = Config::default();
    assert_eq!(default.graph.client_id, DEFAULT_GRAPH_CLIENT_ID);
    default.validate_graph_login().unwrap();
    for input in [
        "",
        "[graph]",
        "[graph]\nclient_id = \"\"",
        "[graph]\nclient_id = \"PASTE_AZURE_APP_CLIENT_ID_HERE\"",
        "[graph]\nclient_id = \"YOUR_AZURE_APP_CLIENT_ID\"",
    ] {
        let config: Config = toml::from_str(input).unwrap();
        assert_eq!(config.graph.client_id, DEFAULT_GRAPH_CLIENT_ID);
        config.validate_graph_login().unwrap();
    }
}

#[test]
fn graph_custom_registration_survives_config_round_trip() {
    let config: Config = toml::from_str(
        r#"
            [graph]
            client_id = "11111111-2222-3333-4444-555555555555"
            tenant = "organizations"
            redirect_uri = "http://localhost:54321"
            scopes = ["Files.ReadWrite"]
        "#,
    )
    .unwrap();
    let loaded: Config = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
    assert_eq!(
        loaded.graph.client_id,
        "11111111-2222-3333-4444-555555555555"
    );
    assert_eq!(loaded.graph.tenant, "organizations");
    assert_eq!(loaded.graph.redirect_uri, "http://localhost:54321");
    assert_eq!(loaded.graph.scopes, ["Files.ReadWrite"]);
}

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
fn local_create_is_atomic_and_can_reuse_a_pending_delete_path() {
    let test = TestDatabase::new("local-create");
    add_file(&test.db, "old-cloud", "/file");
    let old = test.db.get_by_path("/file").unwrap().unwrap();
    test.db.queue_pending_delete(&old).unwrap();
    let cache = test.root.join("new-cache");
    fs::write(&cache, b"").unwrap();
    let record = test
        .db
        .create_local_file("local-upload-new", "/file", &cache)
        .unwrap();
    assert_eq!(record.state, FileState::Writing);
    assert_eq!(record.cache_path, Some(cache.clone()));
    assert!(record.cloud_remote_id.is_none());
    assert!(
        test.db
            .create_local_file("local-upload-collision", "/file", &cache)
            .is_err()
    );
    assert!(
        test.db
            .get_by_remote_id("local-upload-collision")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        test.db
            .get_by_path("/file")
            .unwrap()
            .unwrap()
            .metadata
            .remote_id,
        "local-upload-new"
    );
    assert_eq!(test.db.pending_deletes().unwrap().len(), 1);
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
    let reopened = Database::new(test.db.path());
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
    let restarted = Database::new(test.db.path());
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
    test.db.set_explicit_pin("cloud", true).unwrap();
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
fn explicit_release_overrides_pin_but_prune_preserves_it() {
    let test = TestDatabase::new("release-pinned");
    add_dir(&test.db, "parent", "/folder");
    add_file(&test.db, "file", "/folder/file");
    add_file(&test.db, "sibling", "/folder/sibling");
    let cache = test.root.join("content");
    fs::write(&cache, b"safe").unwrap();
    test.db.mark_cached("file", &cache).unwrap();
    test.db.set_explicit_pin("parent", true).unwrap();
    test.db.set_explicit_pin("file", true).unwrap();
    assert_eq!(test.db.prune_cache(-1).unwrap(), 0);
    assert!(cache.exists());
    assert_eq!(test.db.release_path("/folder/file").unwrap(), 1);
    test.db.recompute_pin_inheritance().unwrap();
    let released = test.db.get_by_path("/folder/file").unwrap().unwrap();
    assert!(!released.effective_pinned());
    assert!(released.pin_inheritance_blocked);
    assert_eq!(released.state, FileState::OnlineOnly);
    assert!(!cache.exists());
    assert!(
        test.db
            .get_by_path("/folder")
            .unwrap()
            .unwrap()
            .effective_pinned()
    );
    assert!(
        test.db
            .get_by_path("/folder/sibling")
            .unwrap()
            .unwrap()
            .effective_pinned()
    );
    test.db.set_explicit_pin("file", true).unwrap();
    assert!(
        test.db
            .get_by_path("/folder/file")
            .unwrap()
            .unwrap()
            .effective_pinned()
    );
}

#[test]
fn folder_release_cancels_nested_pins_and_downloads_but_keeps_dirty_data() {
    let test = TestDatabase::new("release-pinned-folder");
    add_dir(&test.db, "parent", "/folder");
    add_dir(&test.db, "nested", "/folder/nested");
    add_file(&test.db, "file", "/folder/nested/file");
    add_file(&test.db, "dirty", "/folder/dirty");
    add_file(&test.db, "outside", "/folder-other");
    test.db.set_explicit_pin("parent", true).unwrap();
    test.db.set_explicit_pin("nested", true).unwrap();
    test.db.set_explicit_pin("outside", true).unwrap();
    let generation = test.db.download_generation("file").unwrap();
    assert!(test.db.begin_hydration("file", generation).unwrap());
    let cache = test.root.join("dirty");
    fs::write(&cache, b"save").unwrap();
    test.db.mark_cached("dirty", &cache).unwrap();
    test.db.mark_state("dirty", FileState::Dirty).unwrap();
    assert_eq!(test.db.release_path("/folder").unwrap(), 1);
    test.db.recompute_pin_inheritance().unwrap();
    for path in [
        "/folder",
        "/folder/nested",
        "/folder/nested/file",
        "/folder/dirty",
    ] {
        assert!(
            !test
                .db
                .get_by_path(path)
                .unwrap()
                .unwrap()
                .effective_pinned()
        );
    }
    assert!(!test.db.begin_hydration("file", generation).unwrap());
    assert_eq!(test.db.pending_release_count("/folder").unwrap(), 1);
    assert_eq!(test.db.finish_pending_releases().unwrap(), 0);
    assert_eq!(fs::read(&cache).unwrap(), b"save");
    assert!(
        test.db
            .get_by_path("/folder-other")
            .unwrap()
            .unwrap()
            .effective_pinned()
    );
    test.db.mark_cached("dirty", &cache).unwrap();
    assert_eq!(test.db.finish_pending_releases().unwrap(), 1);
    assert!(!cache.exists());
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
    assert_eq!(pinned_root.local_mode, None);
    assert_eq!(
        db.get_by_remote_id("child").unwrap().unwrap().local_mode,
        None
    );
    db.set_local_mode("child", 0o750).unwrap();
    db.init().unwrap();
    assert_eq!(
        db.get_by_remote_id("child").unwrap().unwrap().local_mode,
        Some(0o750)
    );
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
fn local_permissions_survive_cloud_updates_release_and_replacement() {
    let test = TestDatabase::new("local-permissions");
    let cache = test.root.join("program.cache");
    fs::write(&cache, b"program").unwrap();
    let record = test
        .db
        .create_local_file_with_mode("local-upload-program", "/program", &cache, 0o750)
        .unwrap();
    assert_eq!(record.local_mode, Some(0o750));
    test.db
        .set_local_mode(&record.metadata.remote_id, 0o751)
        .unwrap();
    assert_eq!(
        test.db.get_by_path("/program").unwrap().unwrap().state,
        FileState::Writing
    );
    test.db
        .mark_state(&record.metadata.remote_id, FileState::Uploading)
        .unwrap();
    let mut remote = MetadataEntry::new_file("cloud-program", "/program", 7, 2, "v1");
    let uploaded = test
        .db
        .commit_uploaded(&record.metadata.remote_id, "/program", &remote, &cache)
        .unwrap();
    assert_eq!(uploaded.local_mode, Some(0o751));
    remote.etag = "v2".to_string();
    test.db.upsert_metadata(&remote).unwrap();
    remote.etag = "v3".to_string();
    test.db.upsert_metadata_batch([&remote]).unwrap();
    test.db.release_path("/program").unwrap();
    test.db.init().unwrap();
    let released = test.db.get_by_path("/program").unwrap().unwrap();
    assert_eq!(released.local_mode, Some(0o751));
    assert_eq!(released.state, FileState::OnlineOnly);
    assert!(released.cache_path.is_none());
    let replacement_cache = test.root.join("replacement.cache");
    fs::write(&replacement_cache, b"replacement").unwrap();
    test.db
        .create_local_file_with_mode(
            "local-upload-replacement",
            "/replacement",
            &replacement_cache,
            0o640,
        )
        .unwrap();
    let replaced = test
        .db
        .replace_file_locally(
            "local-upload-replacement",
            &record.metadata.remote_id,
            "/program",
        )
        .unwrap();
    assert_eq!(replaced.local_mode, Some(0o640));
    test.db
        .remove_by_remote_id(&record.metadata.remote_id)
        .unwrap();
    test.db.upsert_metadata(&remote).unwrap();
    assert_eq!(
        test.db.get_by_path("/program").unwrap().unwrap().local_mode,
        None
    );
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
fn parent_move_retargets_jobs_and_protects_children_from_delta() {
    for batch in [false, true] {
        let test = TestDatabase::new(if batch {
            "parent-move-batch"
        } else {
            "parent-move-single"
        });
        let parent = MetadataEntry::new_dir("folder", "/Old", 1, "dir-etag");
        let child = MetadataEntry::new_file("child", "/Old/paper.pdf", 12, 1, "etag");
        test.db.upsert_metadata(&parent).unwrap();
        test.db.upsert_metadata(&child).unwrap();
        test.db
            .create_local_directory("local-upload-sub", "/Old/sub")
            .unwrap();
        test.db
            .move_subtree_and_queue("child", "/Old/renamed.pdf")
            .unwrap();
        test.db.move_subtree_and_queue("folder", "/New").unwrap();
        assert_eq!(
            test.db
                .pending_metadata_operation("local-upload-sub")
                .unwrap()
                .unwrap()
                .path,
            "/New/sub"
        );
        assert_eq!(
            test.db
                .pending_metadata_operation("child")
                .unwrap()
                .unwrap()
                .path,
            "/New/renamed.pdf"
        );
        // Also protect a child with no operation of its own.
        test.db
            .complete_metadata_operation("child", "/New/renamed.pdf", &child)
            .unwrap();
        if batch {
            test.db.upsert_metadata_batch([&child]).unwrap();
        } else {
            test.db.upsert_metadata(&child).unwrap();
        }
        assert_eq!(
            test.db
                .get_by_remote_id("child")
                .unwrap()
                .unwrap()
                .metadata
                .path,
            "/New/renamed.pdf"
        );
        assert!(
            test.db
                .has_pending_metadata_at_or_above("/New/renamed.pdf")
                .unwrap()
        );
        assert!(
            !test
                .db
                .has_pending_metadata_at_or_above("/Newish/paper.pdf")
                .unwrap()
        );
    }
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
