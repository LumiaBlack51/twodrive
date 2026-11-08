//! Explicit generated Office-to-cloud validation. No existing roots or arbitrary
//! cloud targets are accepted; the companion host owns all generated documents.
#[cfg(all(windows, feature = "native-sync", feature = "native-test-hooks"))]
fn run() -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use std::{
        fs,
        io::{BufRead, Write},
        path::{Path, PathBuf},
        sync::{Arc, atomic::AtomicBool},
    };
    use twodrive_dev::{
        local_files::no_progress,
        model::CloudBackend,
        onedrive::{self, OneDrive},
    };
    use twodrive_local_files::native::{HydrationSource, persistent::PersistentSession};
    struct NoData;
    impl HydrationSource for NoData {
        fn size(&self, _: &[u8]) -> Result<usize, String> {
            Err("generated documents are resident".into())
        }
        fn read_range(
            &self,
            _: &[u8],
            _: usize,
            _: usize,
            _: &AtomicBool,
        ) -> Result<Vec<u8>, String> {
            Err("no unrelated hydration".into())
        }
    }
    let mut args = std::env::args_os().skip(1);
    ensure!(
        args.next().as_deref() == Some(std::ffi::OsStr::new("--allow-generated-cloud-write")),
        "explicit generated write flag required"
    );
    let account = PathBuf::from(args.next().context("account state required")?);
    let expected = args
        .next()
        .context("expected personal owner required")?
        .to_string_lossy()
        .into_owned();
    let root = PathBuf::from(args.next().context("NEW native fixture required")?);
    ensure!(
        args.next().is_none()
            && account.is_absolute()
            && root.is_absolute()
            && !root.exists()
            && root.file_name().is_some_and(|name| name
                .to_string_lossy()
                .starts_with("twodrive-cfapi-fixture-office-cloud-")),
        "invalid fixture scope"
    );
    let provider = OneDrive::open(&account, true)?;
    let identity = provider.graph.readonly_identity(&mut || Ok(()))?;
    ensure!(
        identity.account.eq_ignore_ascii_case(&expected),
        "personal owner mismatch"
    );
    let state = tempfile::Builder::new()
        .prefix("twodrive-office-cloud-state-")
        .tempdir()?;
    let private = state.path().canonicalize()?;
    let cache = private.join("cache");
    fs::create_dir(&cache)?;
    let folder_name = format!(
        "twodrive-cfapi-fixture-office-cloud-{:032x}",
        rand::random::<u128>()
    );
    let cloud_folder = format!("/{folder_name}");
    fs::write(
        private.join("scope.json"),
        serde_json::to_vec(
            &serde_json::json!({"cloud_folder":cloud_folder,"native_root":root,"generated_only":true}),
        )?,
    )?;
    let result = (|| -> anyhow::Result<()> {
        ensure!(
            provider.get_metadata_by_path(&cloud_folder)?.is_none(),
            "generated namespace exists"
        );
        let folder = provider.create_folder(&cloud_folder)?;
        let mut owner = PersistentSession::create_new(
            &root,
            b"twodrive-dev-native-v1:generated-office-only",
            &private.join("receipt.json"),
            Arc::new(NoData),
        )
        .map_err(anyhow::Error::msg)?;
        let office_folder = owner
            .publish_placeholder(
                Path::new(&folder_name),
                folder.remote_id.as_bytes(),
                0,
                true,
                1,
            )
            .map_err(anyhow::Error::msg)?;
        println!(
            "{}",
            serde_json::json!({"ready":true,"office_folder":root.join(&folder_name)})
        );
        std::io::stdout().flush()?;
        let mut commands = std::io::stdin().lock().lines();
        let command = commands.next().context("verification command required")??;
        let command: serde_json::Value = serde_json::from_str(&command)?;
        let baseline = PathBuf::from(command["baseline"].as_str().context("baseline required")?);
        ensure!(
            baseline.is_absolute()
                && baseline.file_name().is_some_and(|name| name
                    .to_string_lossy()
                    .starts_with("twodrive-msoffice-baseline-")),
            "invalid generated baseline"
        );
        ensure!(
            baseline.canonicalize()?.parent()
                == Some(std::env::temp_dir().canonicalize()?.as_path()),
            "baseline is outside temporary scope"
        );
        let names = ["generated.docx", "generated.xlsx", "generated.pptx"];
        for directory in [&baseline, &office_folder] {
            let actual = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
            ensure!(
                actual.len() == 3
                    && actual.iter().all(|entry| names
                        .iter()
                        .any(|name| entry.file_name() == std::ffi::OsStr::new(name))),
                "unexpected Office fixture entry"
            );
        }
        let bridge = onedrive::open_local_session(
            twodrive_core::Database::new(private.join("journal.sqlite3")),
            &cache,
            &account,
            true,
        )?;
        let mut receipts = Vec::new();
        for name in names {
            let relative = Path::new(&folder_name).join(name);
            let path = format!("{cloud_folder}/{name}");
            let initial = baseline.join(name);
            let visible = office_folder.join(name);
            ensure!(
                fs::metadata(&initial)?.len() <= 16 * 1024 * 1024
                    && fs::metadata(&visible)?.len() <= 16 * 1024 * 1024,
                "generated fixture unexpectedly large"
            );
            let final_bytes = fs::read(&visible)?;
            ensure!(
                fs::read(&initial)? != final_bytes,
                "Office timer did not change the generated baseline"
            );
            let remote =
                provider
                    .upload_file_with_version(&path, &initial, None, None, &mut |_, _| Ok(()))?;
            let local_id = bridge.hydrate(&remote.remote_id)?;
            bridge.capture_native_replacement(&owner, &relative, &local_id)?;
            ensure!(
                bridge.upload(&local_id, no_progress())?,
                "Office snapshot not acknowledged"
            );
            ensure!(
                provider.download(&remote.remote_id)? == final_bytes,
                "complete Office cloud readback differs"
            );
            ensure!(
                bridge.acknowledge_native(&visible, &local_id)?,
                "native Office sync acknowledgement refused"
            );
            let current = provider
                .get_metadata(&remote.remote_id)?
                .context("generated Office object missing")?;
            ensure!(
                current.path == path && current.etag != remote.etag,
                "conditional Office version did not advance"
            );
            receipts.push(current);
            println!("verified:{name}");
            std::io::stdout().flush()?;
        }
        // Exercise the real create-only path too: its local identity must
        // remain stable after the service assigns a different opaque item ID.
        let new_name = "generated-new.txt";
        let new_relative = Path::new(&folder_name).join(new_name);
        let new_visible = office_folder.join(new_name);
        let new_bytes = b"generated native creation; no previous cloud object";
        fs::write(&new_visible, new_bytes)?;
        let new_id = "local-upload-native-generated-creation";
        bridge.create_native_file(&owner, &new_relative, new_id)?;
        ensure!(
            bridge.upload(new_id, no_progress())?,
            "new local file was not acknowledged"
        );
        ensure!(
            bridge.acknowledge_native(&new_visible, new_id)?,
            "new native identity was not acknowledged"
        );
        let created = bridge
            .journal()
            .database()
            .get_by_remote_id(new_id)?
            .context("creation disappeared")?;
        let cloud_id = created
            .cloud_remote_id
            .context("creation did not bind a cloud ID")?;
        ensure!(
            provider.download(&cloud_id)? == new_bytes,
            "created cloud file differs"
        );
        receipts.push(
            provider
                .get_metadata(&cloud_id)?
                .context("created item missing")?,
        );
        fs::remove_file(&new_visible)?;
        println!("verified:native-create-only-file");
        println!("verified:all-office-documents");
        std::io::stdout().flush()?;
        ensure!(
            commands.next().context("retirement command required")?? == "retire",
            "explicit retirement required"
        );
        ensure!(
            fs::read_dir(&office_folder)?.next().is_none(),
            "Office generated files are still present"
        );
        fs::remove_dir(&office_folder)?;
        owner
            .retire_empty_office_fixture()
            .map_err(anyhow::Error::msg)?;
        drop(owner);
        fs::remove_dir(&root)?;
        for receipt in receipts {
            let current = provider
                .get_metadata(&receipt.remote_id)?
                .context("generated cloud file vanished")?;
            ensure!(
                current.path == receipt.path && current.etag == receipt.etag,
                "generated cloud file changed; cleanup stopped"
            );
            provider.delete(&receipt.remote_id)?;
        }
        ensure!(
            provider
                .graph
                .desktop_directory(&cloud_folder, &mut || Ok(()))?
                .is_empty(),
            "unexpected generated cloud contents"
        );
        provider.delete(&folder.remote_id)?;
        ensure!(
            provider.get_metadata_by_path(&cloud_folder)?.is_none(),
            "cloud fixture retirement unconfirmed"
        );
        drop(bridge);
        println!("finished:office-cloud-and-native-scopes-retired");
        Ok(())
    })();
    if result.is_err() {
        let retained = state.keep();
        eprintln!(
            "Generated validation state retained: {}",
            retained.display()
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
        "requires Windows native-sync/native-test-hooks and explicit generated-cloud-write flag"
    );
    std::process::exit(2);
}
