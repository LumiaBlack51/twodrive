//! Explicit empty CFAPI fixture for the companion Microsoft Office integration
//! test. Never serves cloud content or accepts existing directories.
#[cfg(all(windows, feature = "native-sync", feature = "native-test-hooks"))]
fn main() -> anyhow::Result<()> {
    use std::{
        io::Write,
        path::PathBuf,
        sync::{Arc, atomic::AtomicBool},
    };
    use twodrive_cfapi_probe::native::{HydrationSource, persistent::PersistentSession};
    struct NoCloud;
    impl HydrationSource for NoCloud {
        fn size(&self, _: &[u8]) -> Result<usize, String> {
            Err("no cloud in Office fixture".into())
        }
        fn read_range(
            &self,
            _: &[u8],
            _: usize,
            _: usize,
            _: &AtomicBool,
        ) -> Result<Vec<u8>, String> {
            Err("no cloud in Office fixture".into())
        }
    }
    let mut args = std::env::args_os().skip(1);
    let root = PathBuf::from(
        args.next()
            .ok_or_else(|| anyhow::anyhow!("explicit NEW fixture root required"))?,
    );
    anyhow::ensure!(
        args.next().is_none()
            && root.is_absolute()
            && !root.exists()
            && root.file_name().is_some_and(|n| n
                .to_string_lossy()
                .starts_with("twodrive-cfapi-fixture-office-")),
        "invalid fixture root"
    );
    let mut owner = PersistentSession::create_new(
        &root,
        b"twodrive-dev-native-v1:generated-office-only",
        Arc::new(NoCloud),
    )
    .map_err(anyhow::Error::msg)?;
    println!("ready:generated-office-only");
    std::io::stdout().flush()?;
    let mut command = String::new();
    std::io::stdin().read_line(&mut command)?;
    anyhow::ensure!(
        command.trim() == "finish",
        "explicit finish required; root retained"
    );
    owner
        .retire_empty_office_fixture()
        .map_err(anyhow::Error::msg)?;
    drop(owner);
    std::fs::remove_dir(&root)?;
    println!("finished:unregistered-empty-root");
    Ok(())
}
#[cfg(not(all(windows, feature = "native-sync", feature = "native-test-hooks")))]
fn main() {
    eprintln!("requires Windows native-sync and native-test-hooks");
    std::process::exit(2);
}
