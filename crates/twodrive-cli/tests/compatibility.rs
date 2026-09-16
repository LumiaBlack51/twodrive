use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        Self(
            std::env::temp_dir().join(format!(
                "twodrive-cli-contract-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )),
        )
    }
    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_twodrive"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("TWODRIVE_MOUNT_DIR", self.0.join("mount"))
            .env("TWODRIVE_BACKEND", "mock")
            .args(args)
            .output()
            .unwrap()
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn mock_commands_preserve_desktop_protocol_and_persistent_pin_state() {
    let sandbox = Sandbox::new();
    assert!(sandbox.run(&["init-mock"]).status.success());
    let state = |path: &str| {
        let output = sandbox.run(&["status-path", path]);
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    };
    let before = state("/README-cloud.txt");
    for line in [
        "path=/README-cloud.txt",
        "state=online_only",
        "emblem=emblem-twodrive-cloud",
        "local_id=file-readme",
        "metadata_operation_pending=false",
    ] {
        assert!(before.lines().any(|actual| actual == line), "{before}");
    }
    assert_eq!(
        state("/missing"),
        "path=/missing\nstate=unknown\nemblem=emblem-twodrive-error\n"
    );
    assert!(sandbox.run(&["pin", "/README-cloud.txt"]).status.success());
    assert!(state("/README-cloud.txt").contains("effective_pinned=true\n"));
    assert!(
        sandbox
            .run(&["unpin", "/README-cloud.txt"])
            .status
            .success()
    );
    assert!(state("/README-cloud.txt").contains("effective_pinned=false\n"));
    assert!(
        sandbox
            .run(&["release", "/README-cloud.txt"])
            .status
            .success()
    );
    assert!(state("/README-cloud.txt").contains("state=online_only\n"));
    let mount_path = sandbox.0.join("mount/README-cloud.txt");
    assert_eq!(
        state(mount_path.to_str().unwrap()),
        state("/README-cloud.txt")
    );
    assert!(!sandbox.0.join("config/twodrive/tokens.json").exists());
}

#[test]
fn help_version_and_errors_keep_their_exit_contract() {
    let sandbox = Sandbox::new();
    assert_eq!(sandbox.run(&[]).stdout, sandbox.run(&["--help"]).stdout);
    for flag in ["version", "--version", "-V"] {
        let output = sandbox.run(&[flag]);
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            concat!("twodrive ", env!("CARGO_PKG_VERSION"), "\n")
        );
    }
    assert!(!sandbox.run(&["invalid-command"]).status.success());
    assert!(!sandbox.run(&["pin"]).status.success());
    assert!(!sandbox.0.exists());
}
