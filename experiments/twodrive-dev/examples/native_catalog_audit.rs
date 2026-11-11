//! Read-only, account-verified catalogue preflight. Only aggregate counts are
//! printed; names/IDs stay in a caller-selected private local database.
#[cfg(all(windows, feature = "native-local-fixture"))]
fn run() -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use std::{
        path::PathBuf,
        time::{Duration, Instant},
    };
    let mut args = std::env::args_os().skip(1);
    let account = PathBuf::from(args.next().context("account state required")?);
    let expected = args
        .next()
        .context("expected personal owner required")?
        .to_string_lossy()
        .into_owned();
    let output = PathBuf::from(
        args.next()
            .context("NEW private audit directory required")?,
    );
    ensure!(
        args.next().is_none() && account.is_absolute() && output.is_absolute() && !output.exists(),
        "invalid preflight scope"
    );
    let provider = twodrive_dev::onedrive::OneDrive::open(&account, false)?;
    let identity = provider.graph.readonly_identity(&mut || Ok(()))?;
    ensure!(
        identity.account.eq_ignore_ascii_case(&expected),
        "current personal owner mismatch"
    );
    let directory = twodrive_local_files::native::GeneratedDirectory::create(&output)
        .map_err(anyhow::Error::msg)?;
    let db = twodrive_core::Database::new(directory.path().join("catalog.sqlite3"));
    db.init()?;
    db.init_cloud()?;
    let deadline = Instant::now() + Duration::from_secs(300);
    let (cursor, replace, pages) = provider.graph.stage_delta(&db, &identity, &mut || {
        ensure!(
            Instant::now() < deadline,
            "catalogue preflight deadline reached"
        );
        Ok(())
    })?;
    ensure!(
        provider.graph.readonly_identity(&mut || Ok(()))? == identity,
        "account changed during catalogue preflight"
    );
    db.cloud_commit(&identity, &cursor, replace)?;
    let entries = db.cloud_items(&identity)?;
    let files = entries.iter().filter(|item| item.kind == "file").count();
    let folders = entries.iter().filter(|item| item.kind == "folder").count();
    let unsupported = entries
        .iter()
        .filter(|item| item.kind == "unsupported")
        .count();
    let logical_bytes =
        entries
            .iter()
            .filter(|item| item.kind == "file")
            .try_fold(0u64, |sum, item| {
                sum.checked_add(item.size)
                    .context("catalogue size overflow")
            })?;
    println!(
        "{}",
        serde_json::json!({"owner_matches_current_personal_client":true,"pages":pages,"entries":entries.len(),"files":files,"folders_including_root":folders,"unsupported_objects":unsupported,"logical_file_bytes":logical_bytes,"content_downloaded":false,"cloud_mutations":false,"native_root_registered":false,"private_catalogue":db.path()})
    );
    Ok(())
}
#[cfg(all(windows, feature = "native-local-fixture"))]
fn main() {
    if let Err(error) = run() {
        eprintln!("{}", twodrive_backend::diagnostics::safe_error(&error));
        std::process::exit(1);
    }
}
#[cfg(not(all(windows, feature = "native-local-fixture")))]
fn main() {
    eprintln!(
        "requires Windows native-local-fixture and explicit independent account/private output"
    );
    std::process::exit(2);
}
