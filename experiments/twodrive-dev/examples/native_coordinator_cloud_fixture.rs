//! Generated-only native coordinator -> personal OneDrive namespace validation.
//! No caller-selected cloud pathname, existing root or user document is accepted.
#[cfg(all(windows, feature = "native-sync", feature = "native-test-hooks"))]
fn run() -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use std::{fs, path::PathBuf};
    use twodrive_core::{Database, IndexedItem};
    use twodrive_dev::{
        model::CloudBackend,
        native_coordinator::NativeCoordinator,
        onedrive::{self, OneDrive},
    };
    use twodrive_local_files::catalogue::NamespacePlan;
    let mut args = std::env::args_os().skip(1);
    ensure!(
        args.next().as_deref() == Some(std::ffi::OsStr::new("--allow-generated-cloud-write")),
        "explicit generated write opt-in required"
    );
    let account = PathBuf::from(args.next().context("independent account required")?);
    let expected = args
        .next()
        .context("expected personal owner required")?
        .to_string_lossy()
        .into_owned();
    let root = PathBuf::from(args.next().context("new generated native root required")?);
    ensure!(
        account.is_absolute()
            && root.is_absolute()
            && !root.exists()
            && args.next().is_none()
            && root.file_name().is_some_and(|name| name
                .to_string_lossy()
                .starts_with("twodrive-cfapi-fixture-office-coordinator-")),
        "invalid generated fixture scope"
    );
    let state = tempfile::Builder::new()
        .prefix("twodrive-coordinator-cloud-")
        .tempdir()?;
    let private = state.path().canonicalize()?;
    let isolated = private.join("account");
    fs::create_dir(&isolated)?;
    for name in ["config.toml", "tokens.json"] {
        fs::copy(account.join(name), isolated.join(name))?;
    }
    let provider = OneDrive::open(&isolated, true)?;
    let identity = provider.graph.readonly_identity(&mut || Ok(()))?;
    ensure!(
        identity.account.eq_ignore_ascii_case(&expected),
        "personal owner mismatch"
    );
    let cloud_root = provider
        .get_metadata_by_path("/")?
        .context("cloud root missing")?;
    let scope = format!(
        "TwoDrive-native-coordinator-validation-{:032x}",
        rand::random::<u128>()
    );
    fs::write(
        private.join("scope.json"),
        serde_json::to_vec(
            &serde_json::json!({"native_root":root,"scope":scope,"generated_only":true}),
        )?,
    )?;
    let result = (|| -> anyhow::Result<()> {
        ensure!(
            provider
                .get_metadata_by_path(&format!("/{scope}"))?
                .is_none(),
            "generated cloud scope exists"
        );
        let plan = NamespacePlan::build(
            &[IndexedItem {
                id: cloud_root.remote_id,
                parent: None,
                name: "root".into(),
                kind: "folder".into(),
                size: 0,
                etag: cloud_root.etag,
                modified: None,
                deleted: false,
            }],
            &provider.get_metadata_by_path("/")?.unwrap().remote_id,
            100,
            twodrive_local_files::names::ordinal_ignorecase_cmp,
        )
        .map_err(anyhow::Error::msg)?;
        let cache = private.join("cache");
        fs::create_dir(&cache)?;
        let bridge = onedrive::open_local_session(
            Database::new(private.join("native.sqlite3")),
            &cache,
            &isolated,
            true,
        )?;
        let mut coordinator = NativeCoordinator::create_new(
            bridge,
            &plan,
            &root,
            b"twodrive-dev-native-v1:generated-office-only",
            &private.join("root-receipt.json"),
        )?;
        let relative = PathBuf::from(&scope)
            .join("generated-child")
            .join("generated.txt");
        let mut path = coordinator.root().join(&relative);
        fs::create_dir_all(path.parent().unwrap())?;
        let bytes = b"generated coordinator complete bytes through native directory ordering";
        fs::write(&path, bytes)?;
        let progress = coordinator.synchronize_once()?;
        ensure!(
            progress.created == 3
                && progress.deferred == 0
                && progress.conflicts == 0
                && !progress.needs_namespace,
            "generated coordinator creation deferred: {progress:?}"
        );
        let file_path = format!("/{scope}/generated-child/generated.txt");
        let file = provider
            .get_metadata_by_path(&file_path)?
            .context("generated cloud file missing")?;
        ensure!(
            !file.is_dir
                && provider.download(&file.remote_id)? == bytes
                && fs::read(&path)? == bytes,
            "generated full-byte readback mismatch"
        );
        let original_id = file.remote_id;
        fs::rename(
            coordinator.root().join(&scope).join("generated-child"),
            coordinator.root().join(&scope).join("renamed-child"),
        )?;
        let moved = coordinator.synchronize_once()?;
        ensure!(
            moved.renamed == 1 && moved.deferred == 0 && !moved.needs_namespace,
            "generated directory move deferred: {moved:?}"
        );
        path = coordinator
            .root()
            .join(&scope)
            .join("renamed-child")
            .join("generated.txt");
        let renamed = path.with_file_name("renamed.txt");
        fs::rename(&path, &renamed)?;
        path = renamed;
        let moved = coordinator.synchronize_once()?;
        ensure!(
            moved.renamed == 1 && moved.deferred == 0 && !moved.needs_namespace,
            "generated file move deferred: {moved:?}"
        );
        let file = provider
            .get_metadata(&original_id)?
            .context("generated moved object missing")?;
        ensure!(
            file.path == format!("/{scope}/renamed-child/renamed.txt")
                && provider.download(&original_id)? == bytes
                && fs::read(&path)? == bytes,
            "generated rename did not preserve ID/full bytes"
        );
        // A different process produces actual kernel delete notifications.
        use base64::Engine;
        use std::os::windows::process::CommandExt;
        let script = format!(
            "[IO.File]::Delete('{}')",
            path.to_string_lossy().replace('\'', "''")
        );
        let encoded = base64::engine::general_purpose::STANDARD.encode(
            script
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        let child = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded])
            .creation_flags(0x08000000)
            .output()?;
        ensure!(child.status.success(), "generated deletion child failed");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let deleted = loop {
            let progress = coordinator.synchronize_once()?;
            if progress.deleted == 1 || std::time::Instant::now() >= deadline {
                break progress;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        ensure!(
            deleted.deleted == 1
                && deleted.deferred == 0
                && provider.get_metadata(&original_id)?.is_none(),
            "generated native conditional recycle deferred: {deleted:?}"
        );
        for relative_dir in [
            PathBuf::from(&scope).join("renamed-child"),
            PathBuf::from(&scope),
        ] {
            let cloud_path = format!("/{}", relative_dir.to_string_lossy().replace('\\', "/"));
            ensure!(
                provider
                    .graph
                    .desktop_directory(&cloud_path, &mut || Ok(()))?
                    .is_empty(),
                "unexpected cloud child; cleanup stopped"
            );
            let directory = provider
                .get_metadata_by_path(&cloud_path)?
                .context("generated directory disappeared")?;
            ensure!(directory.is_dir, "generated directory kind changed");
            provider.graph.delete_item_bound(
                &identity,
                &directory.remote_id,
                &directory.etag,
                &mut || Ok(()),
            )?;
            fs::remove_dir(coordinator.root().join(relative_dir))?;
        }
        ensure!(
            provider
                .get_metadata_by_path(&format!("/{scope}"))?
                .is_none(),
            "cloud retirement unconfirmed"
        );
        coordinator.retire_empty_generated_fixture()?;
        drop(coordinator);
        fs::remove_dir(&root)?;
        println!(
            "{}",
            serde_json::json!({"actual_cfapi":true,"actual_personal_onedrive":true,"automatic_parent_first_directories":2,"automatic_create_only_file":true,"native_and_cloud_full_byte_readback":true,"directory_and_file_moves_preserve_id_and_bytes":true,"native_file_delete_conditional_recycle":true,"generated_scopes_retired":true,"production_takeover":false})
        );
        Ok(())
    })();
    drop(provider);
    if result.is_err() {
        eprintln!(
            "Generated recovery state retained at {}",
            state.keep().display()
        );
    } else {
        state.close()?;
    }
    result
}
#[cfg(all(windows, feature = "native-sync", feature = "native-test-hooks"))]
fn main() {
    if let Err(error) = run() {
        eprintln!("{}", twodrive_backend::diagnostics::safe_error(&error));
        std::process::exit(1);
    }
}
#[cfg(not(all(windows, feature = "native-sync", feature = "native-test-hooks")))]
fn main() {
    eprintln!(
        "requires Windows native-sync/native-test-hooks and explicit generated cloud write opt-in"
    );
    std::process::exit(2);
}
