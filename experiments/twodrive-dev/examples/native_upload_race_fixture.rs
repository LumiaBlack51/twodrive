//! Generated-only upload-session version race. Uses an isolated credential copy
//! and never accepts a user cloud path. Prints no upload URL or account ID.
#[cfg(windows)]
fn run() -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use std::{fs, path::PathBuf};
    use twodrive_dev::{model::CloudBackend, onedrive::OneDrive};
    let mut args = std::env::args_os().skip(1);
    ensure!(
        args.next().as_deref() == Some(std::ffi::OsStr::new("--allow-generated-cloud-write")),
        "explicit generated write opt-in required"
    );
    let account = PathBuf::from(args.next().context("account state required")?);
    let expected = args
        .next()
        .context("expected owner required")?
        .to_string_lossy()
        .into_owned();
    ensure!(
        account.is_absolute() && args.next().is_none(),
        "invalid fixture scope"
    );
    let original = OneDrive::open(&account, false)?;
    ensure!(
        original
            .graph
            .readonly_identity(&mut || Ok(()))?
            .account
            .eq_ignore_ascii_case(&expected),
        "personal owner mismatch"
    );
    drop(original);
    let state = tempfile::Builder::new()
        .prefix("twodrive-native-upload-race-")
        .tempdir()?;
    let auth = state.path().join("auth");
    fs::create_dir(&auth)?;
    for name in ["config.toml", "tokens.json"] {
        fs::copy(account.join(name), auth.join(name))?;
    }
    let provider = OneDrive::open(&auth, true)?;
    let identity = provider.graph.readonly_identity(&mut || Ok(()))?;
    ensure!(
        identity.account.eq_ignore_ascii_case(&expected),
        "copied credential owner mismatch"
    );
    let scope = format!(
        "/TwoDrive-native-upload-race-{:032x}",
        rand::random::<u128>()
    );
    let cloud_path = format!("{scope}/generated.bin");
    fs::write(
        state.path().join("scope.json"),
        serde_json::to_vec(&serde_json::json!({"scope":scope,"file":cloud_path}))?,
    )?;
    let result = (|| -> anyhow::Result<bool> {
        ensure!(
            provider.get_metadata_by_path(&scope)?.is_none(),
            "generated scope exists"
        );
        let folder = provider.create_folder(&scope)?;
        let initial = state.path().join("initial.bin");
        fs::write(&initial, b"generated initial version")?;
        let file =
            provider
                .upload_file_with_version(&cloud_path, &initial, None, None, &mut |_, _| Ok(()))?;
        fs::write(
            state.path().join("receipt.json"),
            serde_json::to_vec(
                &serde_json::json!({"folder":folder.remote_id,"file":file.remote_id}),
            )?,
        )?;
        let bytes = vec![0x51u8; 20 * 1024 * 1024 + 123];
        let source = state.path().join("large.bin");
        fs::write(&source, &bytes)?;
        let winner = state.path().join("concurrent.bin");
        let winning = b"generated concurrent version must survive";
        fs::write(&winner, winning)?;
        let mut changed = false;
        let mut concurrent_version = None;
        let transfer = provider.upload_file_with_version(
            &cloud_path,
            &source,
            Some(&file.remote_id),
            Some(&file.etag),
            &mut |done, total| {
                if !changed && done > 0 && done < total {
                    concurrent_version = Some(provider.upload_file_with_version(
                        &cloud_path,
                        &winner,
                        Some(&file.remote_id),
                        Some(&file.etag),
                        &mut |_, _| Ok(()),
                    )?);
                    changed = true;
                }
                Ok(())
            },
        );
        ensure!(changed, "fixture never reached its mid-session checkpoint");
        let actual = provider.download(&file.remote_id)?;
        let initially_preserved = actual == winning;
        let version_conflict = transfer
            .as_ref()
            .err()
            .is_some_and(|error| provider.is_conflict_error(error));
        let failure_http_status = transfer.as_ref().err().and_then(|error| {
            let text = error.to_string();
            let start = text.find("HTTP ")? + 5;
            text.get(start..start + 3)?.parse::<u16>().ok()
        });
        ensure!(
            initially_preserved || actual == bytes || actual == b"generated initial version",
            "unexpected generated data; recovery retained"
        );
        let retry = provider.upload_file_with_version(
            &cloud_path,
            &source,
            Some(&file.remote_id),
            Some(&file.etag),
            &mut |_, _| Ok(()),
        );
        let retry_conflict = retry
            .as_ref()
            .err()
            .is_some_and(|error| provider.is_conflict_error(error));
        let after_retry = provider.download(&file.remote_id)?;
        ensure!(
            after_retry == winning || after_retry == bytes,
            "unexpected retry data; recovery retained"
        );
        let current = provider
            .get_metadata(&file.remote_id)?
            .context("generated item missing after race")?;
        let preserved = initially_preserved
            && after_retry == winning
            && concurrent_version
                .as_ref()
                .is_some_and(|version| version.etag == current.etag);
        // Cancel only a remaining session for this exact generated file key.
        let session_file = auth.join("upload-sessions.json");
        if session_file.exists() {
            let sessions: serde_json::Value = serde_json::from_slice(&fs::read(&session_file)?)?;
            if let Some(url) = sessions
                .pointer("/sessions")
                .and_then(|map| map.get(&cloud_path))
                .and_then(|item| item.get("upload_url"))
                .and_then(|url| url.as_str())
            {
                let parsed = url::Url::parse(url)?;
                ensure!(
                    parsed.scheme() == "https",
                    "invalid generated session target"
                );
                let status = reqwest::blocking::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()?
                    .delete(url)
                    .send()
                    .map_err(reqwest::Error::without_url)?
                    .status();
                ensure!(
                    status.is_success() || matches!(status.as_u16(), 404 | 410),
                    "generated session cancellation unconfirmed"
                );
            }
        }
        let current = provider
            .get_metadata(&file.remote_id)?
            .context("generated file missing before cleanup")?;
        ensure!(
            current.path == cloud_path,
            "generated file moved; cleanup stopped"
        );
        provider.graph.delete_item_bound(
            &identity,
            &current.remote_id,
            &current.etag,
            &mut || Ok(()),
        )?;
        ensure!(
            provider
                .graph
                .desktop_directory(&scope, &mut || Ok(()))?
                .is_empty(),
            "unexpected generated child; cleanup stopped"
        );
        let current = provider
            .get_metadata(&folder.remote_id)?
            .context("generated folder missing")?;
        ensure!(
            current.path == scope && current.is_dir,
            "generated folder moved"
        );
        provider.graph.delete_item_bound(
            &identity,
            &current.remote_id,
            &current.etag,
            &mut || Ok(()),
        )?;
        ensure!(
            provider.get_metadata_by_path(&scope)?.is_none(),
            "generated scope cleanup unconfirmed"
        );
        println!(
            "{}",
            serde_json::json!({"actual_personal_onedrive":true,"bytes":bytes.len(),"concurrent_update_during_session":true,
            "large_upload_accepted":transfer.is_ok(),"failure_http_status":failure_http_status,"version_conflict_reported":version_conflict,
            "original_etag_retry_rejected":retry_conflict,"concurrent_version_preserved":preserved,"generated_scope_cleaned":true})
        );
        Ok(preserved && retry_conflict)
    })();
    drop(provider);
    match result {
        Ok(preserved) => {
            state.close()?;
            ensure!(
                preserved,
                "generated probe found a late upload-session overwrite"
            );
            Ok(())
        }
        Err(error) => {
            eprintln!(
                "generated upload-race recovery retained at {}",
                state.keep().display()
            );
            Err(error)
        }
    }
}
#[cfg(windows)]
fn main() {
    if let Err(error) = run() {
        eprintln!("{}", twodrive_backend::diagnostics::safe_error(&error));
        std::process::exit(1);
    }
}
#[cfg(not(windows))]
fn main() {
    eprintln!("requires Windows and an explicit generated personal-cloud write scope");
    std::process::exit(2);
}
