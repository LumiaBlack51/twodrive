// Native child fixture for updater process lifecycle tests, never distributed.
fn main() {
    let mut version = semver::Version::parse(twodrive_peer::VERSION).unwrap();
    version.patch += 1;
    let args: Vec<_> = std::env::args_os().collect();
    assert_eq!(args[1], "health-check");
    assert_eq!(args[2], "--output");
    std::fs::write(
        &args[3],
        format!(
            r#"{{"ok":true,"version":"{}","platform":"{}"}}"#,
            version,
            twodrive_peer::identity::platform()
        ),
    )
    .unwrap();
}
