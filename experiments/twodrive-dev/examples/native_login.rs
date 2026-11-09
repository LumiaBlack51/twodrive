//! Interactive, explicit-account sign-in into a separate native validation state.
//! Reuses the existing PKCE, callback and DPAPI token implementation.
fn main() {
    let result = (|| -> anyhow::Result<()> {
        let mut args = std::env::args_os().skip(1);
        let directory = std::path::PathBuf::from(
            args.next()
                .ok_or_else(|| anyhow::anyhow!("absolute independent state required"))?,
        );
        let hint = args
            .next()
            .ok_or_else(|| anyhow::anyhow!("login hint required"))?
            .to_string_lossy()
            .into_owned();
        anyhow::ensure!(
            directory.is_absolute() && args.next().is_none(),
            "invalid login arguments"
        );
        let paths = twodrive_dev::onedrive::paths(&directory)?;
        twodrive_backend::GraphBackend::login_select_account(
            &paths,
            &hint,
            &std::sync::atomic::AtomicBool::new(false),
        )?;
        println!("independent native account login completed; no cloud files changed");
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("{}", twodrive_backend::diagnostics::safe_error(&error));
        std::process::exit(1);
    }
}
