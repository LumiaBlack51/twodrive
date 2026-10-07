use std::process::Command;
#[test]
fn release_health_command_writes_relative_output_without_state_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_twodrive-peer"))
        .current_dir(dir.path())
        .args(["health-check", "--output", "health.json"])
        .status()
        .unwrap();
    assert!(status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("health.json")).unwrap()).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["version"], twodrive_peer::VERSION);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
