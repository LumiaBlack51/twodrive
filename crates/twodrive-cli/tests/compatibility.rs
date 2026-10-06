use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
                "twodrive-cli-contract-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )))
    }
    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_twodrive"))
            .env("HOME", self.0.join("home"))
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

#[test]
fn status_exposes_missing_upload_source_after_local_rename() {
    let sandbox = Sandbox::new();
    let config_dir = sandbox.0.join("config/twodrive");
    fs::create_dir_all(&config_dir).unwrap();
    let old_source = sandbox.0.join("下载");
    fs::create_dir_all(&old_source).unwrap();
    fs::rename(&old_source, sandbox.0.join("Downloads")).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        format!(
            "[known_folders]\nenabled = true\n[[known_folders.folders]]\nlocal = {:?}\nremote = \"/Downloads\"\n",
            old_source.to_str().unwrap()
        ),
    )
    .unwrap();
    let output = sandbox.run(&["status"]);
    assert!(output.status.success());
    let status = String::from_utf8(output.stdout).unwrap();
    assert!(status.contains("known folder source: missing"), "{status}");
    assert!(status.contains("TwoDrive Settings"), "{status}");
}

#[test]
fn choosing_source_preserves_destinations_unknown_settings_and_rejects_invalid_paths() {
    let sandbox = Sandbox::new();
    let config_dir = sandbox.0.join("config/twodrive");
    fs::create_dir_all(&config_dir).unwrap();
    let source = sandbox.0.join("Downloads");
    fs::create_dir_all(&source).unwrap();
    let config_path = config_dir.join("config.toml");
    fs::write(
        &config_path,
        r#"
[future_settings]
preserve_me = "yes"
[known_folders]
enabled = true
rescan_interval = "0s"
[[known_folders.folders]]
local = "~/下载"
remote = "/Downloads"
future_folder_option = 42
[[known_folders.folders]]
local = "~/图片"
remote = "/Pictures"
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).unwrap();
    let original = fs::read(&config_path).unwrap();
    let path = source.to_str().unwrap();
    for args in [
        vec!["known-folders", "set-source", "0", "~"],
        vec!["known-folders", "set-source", "0", "/"],
        vec!["known-folders", "set-source", "0", "~/missing"],
        vec!["known-folders", "set-source", "9", path],
        vec![
            "known-folders",
            "set-source",
            "0",
            path,
            "--expected-local",
            "~/already-changed",
        ],
    ] {
        assert!(!sandbox.run(&args).status.success(), "{args:?}");
        assert_eq!(fs::read(&config_path).unwrap(), original);
    }
    let output = sandbox.run(&[
        "known-folders",
        "set-source",
        "0",
        path,
        "--expected-local",
        "~/下载",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Restart TwoDrive"));
    let updated: toml::Value = toml::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(
        updated["known_folders"]["folders"][0]["local"].as_str(),
        Some(path)
    );
    assert_eq!(
        updated["known_folders"]["folders"][0]["remote"].as_str(),
        Some("/Downloads")
    );
    assert_eq!(
        updated["known_folders"]["folders"][0]["future_folder_option"].as_integer(),
        Some(42)
    );
    assert_eq!(
        updated["known_folders"]["folders"][1]["local"].as_str(),
        Some("~/图片")
    );
    assert_eq!(
        updated["future_settings"]["preserve_me"].as_str(),
        Some("yes")
    );
    assert_eq!(
        fs::metadata(&config_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(fs::read_dir(config_dir).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
}

#[test]
fn source_diagnostics_distinguish_broken_link_and_xdg_fallback_without_mutating_config() {
    let sandbox = Sandbox::new();
    let config_dir = sandbox.0.join("config/twodrive");
    let home = sandbox.0.join("home");
    fs::create_dir_all(&config_dir).unwrap();
    fs::create_dir_all(home.join("图片")).unwrap();
    std::os::unix::fs::symlink(home.join("old-drive/Image"), home.join("Pictures")).unwrap();
    let user_dirs = sandbox.0.join("config/user-dirs.dirs");
    let user_dirs_data = "XDG_PICTURES_DIR=\"$HOME/\"\nXDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\n";
    fs::write(&user_dirs, user_dirs_data).unwrap();
    let config_path = config_dir.join("config.toml");
    let config = "[known_folders]\nenabled=true\n[[known_folders.folders]]\nlocal=\"~/图片\"\nremote=\"/Pictures\"\n[[known_folders.folders]]\nlocal=\"~/Pictures\"\nremote=\"/OldPictures\"\n[[known_folders.folders]]\nlocal=\"~/下载\"\nremote=\"/Downloads\"\n";
    fs::write(&config_path, config).unwrap();
    let output = sandbox.run(&["known-folders", "status"]);
    assert!(output.status.success());
    let data: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(data["folders"][0]["state"], "ready");
    assert!(
        data["folders"][0]["warnings"][0]
            .as_str()
            .unwrap()
            .contains("home directory")
    );
    assert_eq!(data["folders"][1]["state"], "broken_symlink");
    assert_eq!(data["folders"][2]["state"], "missing");
    assert!(
        data["folders"][2]["warnings"][0]
            .as_str()
            .unwrap()
            .contains("differs")
    );
    assert_eq!(fs::read_to_string(config_path).unwrap(), config);
    assert_eq!(fs::read_to_string(user_dirs).unwrap(), user_dirs_data);
    assert!(home.join("Pictures").is_symlink());
    assert!(!home.join("下载").exists());
}
