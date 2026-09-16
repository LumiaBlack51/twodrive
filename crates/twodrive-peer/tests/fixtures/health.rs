// Native child fixture for updater process lifecycle tests, never distributed.
fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    assert_eq!(args[1], "health-check");
    assert_eq!(args[2], "--output");
    std::fs::write(
        &args[3],
        format!(
            r#"{{"ok":true,"version":"0.1.1","platform":"{}"}}"#,
            twodrive_peer::identity::platform()
        ),
    )
    .unwrap();
}
