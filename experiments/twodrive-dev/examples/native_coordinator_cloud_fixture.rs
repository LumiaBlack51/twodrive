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
        let mut bytes: &[u8] =
            b"generated coordinator complete bytes through native directory ordering";
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
        // A second client modifies only newly generated objects. The native
        // consumer then imports, invalidates, moves and removes those versions.
        let parent = provider
            .get_metadata_by_path(&format!("/{scope}"))?
            .context("generated scope missing")?;
        let remote_dir_path = format!("/{scope}/remote-child");
        let remote_dir = provider.graph.create_directory_bound(
            &identity,
            &parent.remote_id,
            &remote_dir_path,
            &mut || Ok(()),
        )?;
        let remote_path = format!("{remote_dir_path}/remote.txt");
        let remote_source = private.join("remote-source.bin");
        fs::write(&remote_source, b"generated first remote version")?;
        let remote_file = provider.upload_file_with_version(
            &remote_path,
            &remote_source,
            None,
            None,
            &mut |_, _| Ok(()),
        )?;
        let imported =
            coordinator.apply_remote_metadata(&[remote_dir.clone(), remote_file.clone()])?;
        ensure!(
            imported.created == 2 && imported.deferred == 0,
            "remote creation deferred: {imported:?}"
        );
        let remote_native = coordinator
            .root()
            .join(&scope)
            .join("remote-child/remote.txt");
        ensure!(
            fs::read(&remote_native)? == b"generated first remote version",
            "remote creation bytes mismatch"
        );
        fs::write(
            &remote_source,
            b"generated complete changed remote version, longer",
        )?;
        let remote_new = provider.upload_file_with_version(
            &remote_path,
            &remote_source,
            Some(&remote_file.remote_id),
            Some(&remote_file.etag),
            &mut |_, _| Ok(()),
        )?;
        let changed = coordinator.apply_remote_metadata(std::slice::from_ref(&remote_new))?;
        ensure!(
            changed.updated == 1 && changed.deferred == 0,
            "remote update deferred: {changed:?}"
        );
        ensure!(
            fs::read(&remote_native)? == fs::read(&remote_source)?,
            "remote updated bytes mismatch"
        );
        let current_dir = provider
            .get_metadata(&remote_dir.remote_id)?
            .context("remote directory missing")?;
        let moved_dir_path = format!("/{scope}/remote-child-renamed");
        let moved_dir = provider.graph.move_item_bound(
            &identity,
            &remote_dir.remote_id,
            &current_dir.etag,
            &parent.remote_id,
            &moved_dir_path,
            &mut || Ok(()),
        )?;
        let moved_file = provider
            .get_metadata(&remote_file.remote_id)?
            .context("remote moved child missing")?;
        let moved = coordinator.apply_remote_metadata(&[moved_dir, moved_file.clone()])?;
        ensure!(
            moved.moved == 1 && moved.deferred == 0,
            "remote directory move deferred: {moved:?}"
        );
        let moved_native = coordinator
            .root()
            .join(&scope)
            .join("remote-child-renamed/remote.txt");
        ensure!(
            !remote_native.exists() && fs::read(&moved_native)? == fs::read(&remote_source)?,
            "remote subtree bytes/path mismatch"
        );
        provider.graph.delete_item_bound(
            &identity,
            &moved_file.remote_id,
            &moved_file.etag,
            &mut || Ok(()),
        )?;
        let empty = provider
            .graph
            .empty_directory_version_bound(
                &identity,
                &remote_dir.remote_id,
                &parent.remote_id,
                &moved_dir_path,
                &mut || Ok(()),
            )?
            .context("generated remote directory not empty")?;
        provider
            .graph
            .delete_item_bound(&identity, &empty.remote_id, &empty.etag, &mut || Ok(()))?;
        let remaining = Database::new(private.join("native.sqlite3"))
            .all_records()?
            .into_iter()
            .filter_map(|r| r.cloud_remote_id)
            .filter(|id| id != &remote_file.remote_id && id != &remote_dir.remote_id)
            .collect();
        let removed = coordinator.apply_remote_removals(&remaining)?;
        ensure!(
            removed.removed == 2 && removed.deferred == 0,
            "remote removal deferred: {removed:?}"
        );
        ensure!(
            !moved_native.exists() && !moved_native.parent().unwrap().exists(),
            "remote deleted paths remain"
        );
        let settled = coordinator.synchronize_once()?;
        ensure!(
            settled.deferred == 0 && !settled.needs_namespace,
            "remote own-event settling deferred: {settled:?}"
        );

        let original_id = file.remote_id;
        let old_bytes = bytes;
        let temporary = path.with_file_name("generated-update.tmp");
        let backup_path = path.with_file_name("generated-backup.wbk");
        let script=r#"
Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class GeneratedReplaceFile { [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] public static extern bool ReplaceFileW(string target,string replacement,string backup,uint flags,IntPtr exclude,IntPtr reserved); }'
[IO.File]::WriteAllText('__TEMP__','generated retained backup replacement target')
if(![GeneratedReplaceFile]::ReplaceFileW('__TARGET__','__TEMP__','__BACKUP__',0,[IntPtr]::Zero,[IntPtr]::Zero)){exit 17}
"#.replace("__TEMP__",&temporary.to_string_lossy().replace('\'',"''")).replace("__TARGET__",&path.to_string_lossy().replace('\'',"''")).replace("__BACKUP__",&backup_path.to_string_lossy().replace('\'',"''"));
        use base64::Engine;
        use std::os::windows::process::CommandExt;
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
        ensure!(
            child.status.success(),
            "generated ReplaceFileW child failed"
        );
        let replaced = coordinator.synchronize_once()?;
        ensure!(
            replaced.deferred == 0 && !replaced.needs_namespace,
            "generated backup replacement deferred: {replaced:?}"
        );
        bytes = b"generated retained backup replacement target";
        let after = provider
            .get_metadata(&original_id)?
            .context("generated logical target missing")?;
        ensure!(
            after.path == file_path && provider.download(&original_id)? == bytes,
            "retained backup changed logical target ID/path/bytes"
        );
        let backup = provider
            .get_metadata_by_path(&format!("/{scope}/generated-child/generated-backup.wbk"))?
            .context("generated backup missing")?;
        ensure!(
            backup.remote_id != original_id && provider.download(&backup.remote_id)? == old_bytes,
            "generated backup identity/bytes differ"
        );

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
        let backup = provider
            .get_metadata(&backup.remote_id)?
            .context("generated moved backup missing")?;
        ensure!(
            backup.path == format!("/{scope}/renamed-child/generated-backup.wbk")
                && provider.download(&backup.remote_id)? == old_bytes,
            "generated moved backup changed"
        );

        let external = |script: String| -> anyhow::Result<()> {
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
            ensure!(child.status.success(), "generated namespace child failed");
            Ok(())
        };
        let backup_native = coordinator
            .root()
            .join(&scope)
            .join("renamed-child/generated-backup.wbk");
        external(format!(
            "[IO.File]::Delete('{}')",
            backup_native.to_string_lossy().replace('\'', "''")
        ))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let progress = coordinator.synchronize_once()?;
            if progress.deleted == 1 {
                break;
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "generated backup recycle timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let child_cloud = format!("/{scope}/renamed-child");
        let child = provider
            .get_metadata_by_path(&child_cloud)?
            .context("generated child missing")?;
        let parent = provider
            .get_metadata_by_path(&child.parent_path)?
            .context("generated parent missing")?;
        ensure!(
            provider
                .graph
                .empty_directory_version_bound(
                    &identity,
                    &child.remote_id,
                    &parent.remote_id,
                    &child_cloud,
                    &mut || Ok(())
                )?
                .is_some(),
            "generated child not empty"
        );
        let late = provider.graph.create_directory_bound(
            &identity,
            &child.remote_id,
            &format!("{child_cloud}/generated-late-child"),
            &mut || Ok(()),
        )?;
        ensure!(
            provider
                .graph
                .empty_directory_version_bound(
                    &identity,
                    &child.remote_id,
                    &parent.remote_id,
                    &child_cloud,
                    &mut || Ok(())
                )?
                .is_none()
                && provider.get_metadata(&late.remote_id)?.is_some(),
            "observed nonempty directory was not preserved"
        );
        let late = provider
            .get_metadata(&late.remote_id)?
            .context("generated late child disappeared")?;
        let late_stamp = provider
            .graph
            .empty_directory_version_bound(
                &identity,
                &late.remote_id,
                &child.remote_id,
                &late.path,
                &mut || Ok(()),
            )?
            .context("generated late child not empty")?;
        provider.graph.delete_item_bound(
            &identity,
            &late.remote_id,
            &late_stamp.etag,
            &mut || Ok(()),
        )?;
        let child_native = coordinator.root().join(&scope).join("renamed-child");
        let scope_native = coordinator.root().join(&scope);
        external(format!(
            "[IO.Directory]::Delete('{}');[IO.Directory]::Delete('{}')",
            child_native.to_string_lossy().replace('\'', "''"),
            scope_native.to_string_lossy().replace('\'', "''")
        ))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        let mut directory_deletes = 0;
        loop {
            let progress = coordinator.synchronize_once()?;
            directory_deletes += progress.deleted;
            if directory_deletes == 2 {
                break;
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "generated directory recycle timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
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
            serde_json::json!({"actual_cfapi":true,"actual_personal_onedrive":true,"remote_create_update_subtree_move_delete_full_native_readback":true,"automatic_parent_first_directories":2,"automatic_create_only_file":true,"native_and_cloud_full_byte_readback":true,"directory_and_file_moves_preserve_id_and_bytes":true,"native_file_delete_conditional_recycle":true,"native_directory_delete_object_version_recycle":true,"retained_backup_keeps_original_target_id_and_backup_bytes":true,"observed_nonempty_directory_is_not_recycled":true,"generated_scopes_retired":true,"production_takeover":false})
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
