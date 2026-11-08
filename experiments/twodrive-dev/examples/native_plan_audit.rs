//! Revalidate the private committed catalogue against a verified drive root and
//! Windows namespace rules. Metadata only; no registration or cloud writes.
#[cfg(all(windows, feature = "native-local-fixture"))]
fn run() -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use std::{os::windows::ffi::OsStrExt, path::PathBuf};
    use twodrive_dev::{model::CloudBackend, onedrive::OneDrive};
    use twodrive_local_files::{catalogue::NamespacePlan, names::ordinal_ignorecase_cmp};
    let mut args = std::env::args_os().skip(1);
    let account = PathBuf::from(args.next().context("independent account state required")?);
    let owner = args
        .next()
        .context("expected personal owner required")?
        .to_string_lossy()
        .into_owned();
    let catalogue = PathBuf::from(args.next().context("existing private catalogue required")?);
    let target = PathBuf::from(
        args.next()
            .context("intended absolute target root required")?,
    );
    ensure!(
        args.next().is_none()
            && account.is_absolute()
            && catalogue.is_absolute()
            && catalogue.is_file()
            && target.is_absolute(),
        "invalid metadata plan scope"
    );
    let provider = OneDrive::open(&account, false)?;
    let identity = provider.graph.readonly_identity(&mut || Ok(()))?;
    ensure!(
        identity.account.eq_ignore_ascii_case(&owner),
        "personal owner mismatch"
    );
    let root = provider
        .get_metadata_by_path("/")?
        .context("verified cloud root missing")?;
    ensure!(root.is_dir, "cloud root is not a directory");
    let db = twodrive_core::Database::new(&catalogue);
    let entries = db.cloud_items(&identity)?;
    ensure!(
        db.cloud_delta(&identity)?.is_some(),
        "catalogue has no committed cursor"
    );
    let plan = NamespacePlan::build(
        &entries,
        &root.remote_id,
        target.as_os_str().encode_wide().count(),
        ordinal_ignorecase_cmp,
    )
    .map_err(anyhow::Error::msg)?;
    ensure!(
        provider.graph.readonly_identity(&mut || Ok(()))? == identity,
        "account changed during metadata planning"
    );
    println!(
        "{}",
        serde_json::json!({"planned_entries":plan.entries.len(),"max_depth":plan.max_depth,
        "max_full_path_utf16":plan.max_full_path_utf16,"paths_at_least_260_utf16":plan.paths_at_least_260_utf16,
        "verified_drive_root":true,"native_name_and_sibling_comparison":true,"parent_first_plan":true,
        "content_read":false,"cloud_mutations":false,"root_registered":false,"catalogue_snapshot_only":true})
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
        "requires Windows native-local-fixture and an independent personal account/catalogue"
    );
    std::process::exit(2);
}
