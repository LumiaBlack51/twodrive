//! Explicit, disposable real-OneDrive validation. Never accepts an existing
//! native root or an arbitrary cloud destination. No user work files are used.
#[cfg(all(windows, feature = "native-sync"))]
fn run() -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use sha2::{Digest, Sha256};
    use std::{
        fs,
        io::Write,
        path::{Path, PathBuf},
        sync::Arc,
    };
    use twodrive_dev::{
        local_files::no_progress,
        model::CloudBackend,
        onedrive::{self, OneDrive},
    };
    use twodrive_local_files::native::Session;
    let mut args = std::env::args_os().skip(1);
    ensure!(
        args.next().as_deref() == Some(std::ffi::OsStr::new("--allow-generated-cloud-write")),
        "explicit generated-cloud-write flag required"
    );
    let account = PathBuf::from(args.next().context("account state required")?);
    let expected = args
        .next()
        .context("expected personal owner required")?
        .to_string_lossy()
        .into_owned();
    let root = PathBuf::from(args.next().context("NEW native root required")?);
    ensure!(
        args.next().is_none()
            && account.is_absolute()
            && root.is_absolute()
            && !root.exists()
            && root.file_name().is_some_and(|n| n
                .to_string_lossy()
                .starts_with("twodrive-cfapi-fixture-live-")),
        "invalid validation scope"
    );
    let provider = OneDrive::open(&account, true)?;
    let identity = provider.graph.readonly_identity(&mut || Ok(()))?;
    ensure!(
        identity.account.eq_ignore_ascii_case(&expected),
        "current personal account mismatch; cloud writes refused"
    );
    let state = tempfile::Builder::new()
        .prefix("twodrive-native-cloud-")
        .tempdir()?;
    let private = state.path().canonicalize()?;
    let cache = private.join("cache");
    fs::create_dir(&cache)?;
    let cloud_folder = format!(
        "/TwoDrive-native-validation-{:032x}",
        rand::random::<u128>()
    );
    let cloud_file = format!("{cloud_folder}/generated.bin");
    let initial = (0..(1024 * 1024 + 123))
        .map(|i| (i % 251) as u8)
        .collect::<Vec<_>>();
    let edited = initial.iter().map(|b| b ^ 0x5a).collect::<Vec<_>>();
    let input = private.join("generated-input.bin");
    fs::write(&input, &initial)?;
    // Private recovery evidence precedes any cloud mutation. It names only this
    // generated scope and never includes tokens or upload-session URLs.
    fs::write(
        private.join("scope.json"),
        serde_json::to_vec(
            &serde_json::json!({"cloud_folder":cloud_folder,"cloud_file":cloud_file,
        "initial_sha256":format!("{:x}",Sha256::digest(&initial)),"edited_sha256":format!("{:x}",Sha256::digest(&edited))}),
        )?,
    )?;
    let result = (|| -> anyhow::Result<()> {
        ensure!(
            provider.get_metadata_by_path(&cloud_folder)?.is_none(),
            "validation namespace already exists"
        );
        let folder = provider.create_folder(&cloud_folder)?;
        let remote =
            provider
                .upload_file_with_version(&cloud_file, &input, None, None, &mut |_, _| Ok(()))?;
        let bridge = onedrive::open_local_session(
            twodrive_core::Database::new(private.join("journal.sqlite3")),
            &cache,
            &account,
            true,
        )?;
        bridge.journal().database().upsert_metadata(&remote)?;
        let published = bridge
            .journal()
            .database()
            .get_by_cloud_remote_id(&remote.remote_id)?
            .context("missing generated publication")?;
        let source =
            twodrive_dev::native_source::PublishedSource::new(bridge.clone(), vec![published])?;
        let mut session =
            Session::create_with_source(&root, Arc::new(source)).map_err(anyhow::Error::msg)?;
        let visible = session
            .placeholder_for(
                Path::new("generated.bin"),
                remote.remote_id.as_bytes(),
                initial.len(),
            )
            .map_err(anyhow::Error::msg)?;
        let operation = (|| -> anyhow::Result<()> {
            ensure!(
                fs::read(&visible)? == initial,
                "native hydration bytes differ"
            );
            for content in [
                b"short".to_vec(),
                Vec::new(),
                vec![0xa7; initial.len() + 8193],
                edited.clone(),
            ] {
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&visible)?;
                file.write_all(&content)?;
                file.sync_all()?;
                drop(file);
                let record = bridge
                    .journal()
                    .database()
                    .get_by_cloud_remote_id(&remote.remote_id)?
                    .context("missing captured identity")?;
                bridge.capture_native_edit(&visible, &record.metadata.remote_id)?;
                ensure!(
                    bridge.upload(&record.metadata.remote_id, no_progress())?,
                    "native generation not acknowledged"
                );
                ensure!(
                    provider.download(&remote.remote_id)? == content,
                    "real OneDrive resized readback differs"
                );
                ensure!(
                    bridge.acknowledge_native(&visible, &record.metadata.remote_id)?,
                    "native sync state not acknowledged"
                );
            }
            let stale = provider.upload_file_with_version(
                &cloud_file,
                &input,
                Some(&remote.remote_id),
                Some(&remote.etag),
                &mut |_, _| Ok(()),
            );
            ensure!(
                stale.as_ref().is_err_and(|e| provider.is_conflict_error(e)),
                "stale version was not rejected"
            );
            ensure!(
                provider.download(&remote.remote_id)? == edited,
                "stale attempt changed real cloud bytes"
            );
            ensure!(fs::read(&visible)? == edited, "local native bytes changed");
            Ok(())
        })();
        let cleanup = session.finish().map_err(anyhow::Error::msg);
        drop(session);
        operation?;
        cleanup?;
        twodrive_local_files::native::verify_unregistered(&root).map_err(anyhow::Error::msg)?;
        // Delete only the exact generated object and then an empty generated
        // folder. Never recursively delete an uninspected cloud directory.
        let observed = provider
            .get_metadata(&remote.remote_id)?
            .context("generated object disappeared before cleanup")?;
        ensure!(
            observed.path == cloud_file,
            "generated object moved; cleanup stopped"
        );
        provider.delete(&remote.remote_id)?;
        ensure!(
            provider
                .graph
                .desktop_directory(&cloud_folder, &mut || Ok(()))?
                .is_empty(),
            "validation folder contains unexpected data; retained"
        );
        provider.delete(&folder.remote_id)?;
        ensure!(
            provider.get_metadata_by_path(&cloud_folder)?.is_none(),
            "cloud validation folder cleanup unconfirmed"
        );
        ensure!(fs::read(&visible)? == edited, "retained local bytes differ");
        fs::remove_file(&visible)?;
        fs::remove_dir(&root)?;
        drop(bridge);
        println!(
            "{}",
            serde_json::json!({"actual_personal_onedrive":true,"bytes":edited.len(),"native_hydration":true,
            "native_edit_capture":true,"conditional_upload":true,"full_cloud_readback":true,
            "tested_sizes":[5,0,initial.len()+8193,edited.len()],"native_in_sync_state_confirmed":true,
            "stale_version_rejected":true,"cloud_fixture_removed":true,"native_fixture_unregistered_and_removed":true,
            "production_root_migration":false,"native_sync_icons_verified":false})
        );
        Ok(())
    })();
    if result.is_err() {
        let retained = state.keep();
        eprintln!(
            "generated validation recovery state retained at {}",
            retained.display()
        );
    } else {
        ensure!(
            private.starts_with(std::env::temp_dir().canonicalize()?)
                && state.path().canonicalize()? == private,
            "unexpected generated cleanup path"
        );
        state.close()?;
    }
    result
}
#[cfg(all(windows, feature = "native-sync"))]
fn main() {
    if let Err(error) = run() {
        eprintln!("{}", twodrive_backend::diagnostics::safe_error(&error));
        std::process::exit(1);
    }
}
#[cfg(not(all(windows, feature = "native-sync")))]
fn main() {
    eprintln!("requires Windows native-local-fixture and explicit generated-cloud-write opt-in");
    std::process::exit(2);
}
