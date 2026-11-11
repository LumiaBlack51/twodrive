//! Read-only preflight for a caller-selected independent OneDrive login state.
//! No root registration, document reads, cloud writes or token output.
fn main() {
    let result = (|| -> anyhow::Result<()> {
        let mut args = std::env::args_os().skip(1);
        let state = std::path::PathBuf::from(
            args.next()
                .ok_or_else(|| anyhow::anyhow!("absolute account state required"))?,
        );
        let expected = args.next().map(|v| v.to_string_lossy().into_owned());
        anyhow::ensure!(
            state.is_absolute() && args.next().is_none(),
            "absolute state and optional expected personal CID required"
        );
        let provider = twodrive_dev::onedrive::OneDrive::open(&state, false)?;
        let identity = provider.graph.readonly_identity(&mut || Ok(()))?;
        let account = provider.graph.readonly_account_name(&mut || Ok(()))?;
        println!(
            "{}",
            serde_json::json!({"authenticated":true,"provider":"onedrive",
                "account_name":account,"drive_id":identity.drive,
                "owner_matches_expected_personal_cid":expected.as_ref().map(|v|identity.account.eq_ignore_ascii_case(v)),
                "cloud_writes":false})
        );
        anyhow::ensure!(
            expected
                .as_ref()
                .is_none_or(|v| identity.account.eq_ignore_ascii_case(v)),
            "authenticated owner does not match the expected personal client identity; no takeover authorized"
        );
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("{}", twodrive_backend::diagnostics::safe_error(&error));
        std::process::exit(1);
    }
}
