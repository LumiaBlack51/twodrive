use anyhow::{Context, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use fs2::FileExt;
use iroh::{EndpointAddr, SecretKey};
use rand::RngCore;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;

pub const VERSION: u8 = 1;

#[derive(Clone)]
pub struct State {
    pub dir: PathBuf,
}

fn persist_atomic(output: tempfile::NamedTempFile, destination: &Path) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        persist_windows(output, destination, std::thread::sleep)
    }
    #[cfg(not(windows))]
    {
        output.persist(destination)?;
        Ok(())
    }
}

#[cfg(windows)]
fn persist_windows(
    mut output: tempfile::NamedTempFile,
    destination: &Path,
    mut wait: impl FnMut(std::time::Duration),
) -> anyhow::Result<()> {
    for retry in 0..=6 {
        match output.persist(destination) {
            Ok(_) => return Ok(()),
            Err(error) => {
                // Windows readers may temporarily omit FILE_SHARE_DELETE.
                // Keep the same synced temporary file and old destination;
                // never delete the old file or retry permanent failures forever.
                if retry == 6 || !matches!(error.error.raw_os_error(), Some(5 | 32 | 33)) {
                    return Err(error.into());
                }
                output = error.file;
                wait(std::time::Duration::from_millis(25 * (retry + 1)));
            }
        }
    }
    unreachable!()
}

#[cfg(all(test, windows))]
mod atomic_windows_tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    #[test]
    fn temporary_delete_denial_recovers_without_removing_old_content() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("config.json");
        fs::write(&target, b"old content").unwrap();
        let mut lock = Some(
            OpenOptions::new()
                .read(true)
                .share_mode(3)
                .open(&target)
                .unwrap(),
        );
        let mut output = tempfile::NamedTempFile::new_in(root.path()).unwrap();
        output.write_all(b"new content").unwrap();
        output.as_file().sync_all().unwrap();
        // This is the prior unretried replacement behavior, against a real handle.
        let denied = output.persist(&target).unwrap_err();
        assert!(matches!(denied.error.raw_os_error(), Some(5 | 32 | 33)));
        assert_eq!(fs::read(&target).unwrap(), b"old content");
        let mut retries = 0;
        persist_windows(denied.file, &target, |_| {
            retries += 1;
            assert_eq!(fs::read(&target).unwrap(), b"old content");
            drop(lock.take());
        })
        .unwrap();
        assert_eq!(retries, 1);
        assert_eq!(fs::read(&target).unwrap(), b"new content");
    }

    #[test]
    fn persistent_delete_denial_is_bounded_and_keeps_previous_configuration() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("config.json");
        fs::write(&target, b"old content").unwrap();
        let _lock = OpenOptions::new()
            .read(true)
            .share_mode(3)
            .open(&target)
            .unwrap();
        let mut output = tempfile::NamedTempFile::new_in(root.path()).unwrap();
        output.write_all(b"new content").unwrap();
        let mut retries = 0;
        assert!(persist_windows(output, &target, |_| retries += 1).is_err());
        assert_eq!(retries, 6);
        assert_eq!(fs::read(&target).unwrap(), b"old content");
        let state = State::new(root.path().join("private")).unwrap();
        state
            .save("profiles.json", &serde_json::json!({"name":"old"}))
            .unwrap();
        let _state_lock = OpenOptions::new()
            .read(true)
            .share_mode(3)
            .open(state.dir.join("profiles.json"))
            .unwrap();
        assert!(
            state
                .save("profiles.json", &serde_json::json!({"name":"new"}))
                .is_err()
        );
        assert_eq!(
            state.read::<serde_json::Value>("profiles.json").unwrap()["name"],
            "old"
        );
    }
}

#[cfg(test)]
mod export_tests {
    use super::*;
    #[test]
    fn exported_connection_is_private_single_use_and_replacement_is_explicit() {
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path().join("private")).unwrap();
        let shared = root.path().join("shared");
        fs::create_dir(&shared).unwrap();
        state
            .save(
                "share.json",
                &Share {
                    root: shared.clone(),
                    writable: true,
                    address: EndpointAddr::new(state.identity().unwrap().public()),
                },
            )
            .unwrap();
        let output = root.path().join("connection.json");
        state.export_connection(&output, "My PC", false).unwrap();
        let original = fs::read(&output).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        assert_eq!(value["connection"]["name"], "My PC");
        assert_eq!(value["connection"]["writable"], true);
        assert!(
            !String::from_utf8(original.clone())
                .unwrap()
                .contains(&root.path().to_string_lossy().to_string())
        );
        let first: Invitation = serde_json::from_slice(&original).unwrap();
        assert!(state.export_connection(&output, "My PC", false).is_err());
        assert_eq!(fs::read(&output).unwrap(), original);
        assert!(state.authorize("first", Some(&first.secret)).unwrap());
        assert!(!state.authorize("second", Some(&first.secret)).unwrap());
        state.export_connection(&output, "My PC", true).unwrap();
        let second: Invitation = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
        assert_ne!(first.secret, second.secret);
        assert!(!state.authorize("second", Some(&first.secret)).unwrap());
        assert!(state.authorize("second", Some(&second.secret)).unwrap());
        assert!(!state.authorize("third", Some(&second.secret)).unwrap());
        assert!(
            state
                .export_connection(&shared.join("leak.json"), "My PC", false)
                .is_err()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(output).unwrap().permissions().mode() & 0o077,
                0
            );
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Invitation {
    pub version: u8,
    pub address: EndpointAddr,
    pub secret: String,
    pub expires: u64,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Control {
    pub peers: BTreeSet<String>,
    pub invitation: Option<InvitationHash>,
}

#[derive(Serialize, Deserialize)]
pub struct InvitationHash {
    hash: [u8; 32],
    expires: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Share {
    pub root: PathBuf,
    pub writable: bool,
    pub address: EndpointAddr,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn random_secret() -> String {
    let mut bytes = [0; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn private_dir(path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(path)?;
    }
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && !meta.is_symlink(),
        "state must be a real private directory"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            meta.permissions().mode() & 0o077 == 0,
            "state directory must have mode 0700"
        );
    }
    Ok(())
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true).read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
}

pub fn write_new_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    let mut file = private_options().create_new(true).open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    Ok(())
}

impl State {
    pub fn new(dir: PathBuf) -> anyhow::Result<Self> {
        private_dir(&dir)?;
        Ok(Self {
            dir: dir.canonicalize()?,
        })
    }

    pub fn read<T: DeserializeOwned>(&self, name: &str) -> anyhow::Result<T> {
        let path = self.dir.join(name);
        ensure!(
            !fs::symlink_metadata(&path)?.is_symlink(),
            "state file must not be a symlink"
        );
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }

    pub fn save(&self, name: &str, value: &impl Serialize) -> anyhow::Result<()> {
        self.write_bytes(name, &serde_json::to_vec_pretty(value)?)
    }

    pub(crate) fn write_bytes(&self, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
        let mut output = tempfile::NamedTempFile::new_in(&self.dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            output
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        output.write_all(bytes)?;
        output.as_file().sync_all()?;
        persist_atomic(output, &self.dir.join(name))?;
        #[cfg(unix)]
        File::open(&self.dir)?.sync_all()?;
        Ok(())
    }

    fn lock(&self, name: &str) -> anyhow::Result<File> {
        let file = private_options()
            .create(true)
            .truncate(false)
            .open(self.dir.join(name))?;
        Ok(file)
    }

    pub fn run_lock(&self) -> anyhow::Result<File> {
        let file = self.lock("run.lock")?;
        file.try_lock_exclusive().context(
            "this state is already running; choose a separate state for each device/mount",
        )?;
        Ok(file)
    }

    pub fn control<T>(
        &self,
        action: impl FnOnce(&mut Control) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let guard = self.lock("control.lock")?;
        guard.lock_exclusive()?;
        let mut control: Control = if self.dir.join("control.json").exists() {
            self.read("control.json")?
        } else {
            Control::default()
        };
        let before = serde_json::to_vec(&control)?;
        let result = action(&mut control)?;
        if serde_json::to_vec(&control)? != before {
            self.save("control.json", &control)?;
        }
        Ok(result)
    }

    pub fn identity(&self) -> anyhow::Result<SecretKey> {
        let guard = self.lock("identity.lock")?;
        guard.lock_exclusive()?;
        if self.dir.join("identity.json").exists() {
            let bytes: [u8; 32] = self.read("identity.json")?;
            Ok(SecretKey::from_bytes(&bytes))
        } else {
            let key = SecretKey::generate();
            self.save("identity.json", &key.to_bytes())?;
            Ok(key)
        }
    }

    pub fn credentials(&self) -> anyhow::Result<Credentials> {
        if self.dir.join("credentials.json").exists() {
            return self.read("credentials.json");
        }
        let credentials = Credentials {
            username: "twodrive".into(),
            password: random_secret(),
        };
        self.save("credentials.json", &credentials)?;
        Ok(credentials)
    }

    pub fn invite(&self, output: &Path) -> anyhow::Result<()> {
        self.issue_invitation(output, None, false).map(|_| ())
    }

    /// Export a backwards-compatible invitation with public client setup hints.
    /// Never include the host path, private identity or an existing peer's keys.
    pub fn export_connection(
        &self,
        output: &Path,
        name: &str,
        replace: bool,
    ) -> anyhow::Result<u64> {
        ensure!(
            !name.trim().is_empty() && name.len() <= 120 && !name.chars().any(char::is_control),
            "invalid share name"
        );
        self.issue_invitation(output, Some(name), replace)
    }

    fn issue_invitation(
        &self,
        output: &Path,
        name: Option<&str>,
        replace: bool,
    ) -> anyhow::Result<u64> {
        let share: Share = self.read("share.json")?;
        let parent = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        ensure!(
            !parent
                .canonicalize()?
                .starts_with(share.root.canonicalize()?),
            "invitation must be outside the shared directory"
        );
        let invitation = Invitation {
            version: VERSION,
            address: share.address,
            secret: random_secret(),
            expires: now() + 600,
        };
        self.control(|control| {
            if let Some(name) = name {
                let mut value = serde_json::to_value(&invitation)?;
                value["connection"] =
                    serde_json::json!({"schema":1,"name":name,"writable":share.writable});
                if output.exists() {
                    let metadata = fs::symlink_metadata(output)?;
                    ensure!(
                        replace && metadata.is_file() && !metadata.is_symlink(),
                        "连接文件已存在，请选择新文件或确认替换"
                    );
                }
                let mut file = tempfile::NamedTempFile::new_in(parent)?;
                file.write_all(&serde_json::to_vec_pretty(&value)?)?;
                file.as_file().sync_all()?;
                if replace {
                    persist_atomic(file, output)?;
                } else {
                    file.persist_noclobber(output)?;
                }
            } else {
                write_new_json(output, &invitation)?;
            }
            control.invitation = Some(InvitationHash {
                hash: Sha256::digest(invitation.secret.as_bytes()).into(),
                expires: invitation.expires,
            });
            Ok(invitation.expires)
        })
    }

    pub fn authorize(&self, id: &str, invitation: Option<&str>) -> anyhow::Result<bool> {
        self.control(|control| {
            if control.peers.contains(id) {
                return Ok(true);
            }
            let Some(secret) = invitation else {
                return Ok(false);
            };
            let Some(expected) = &control.invitation else {
                return Ok(false);
            };
            let actual: [u8; 32] = Sha256::digest(secret.as_bytes()).into();
            if now() >= expected.expires || !bool::from(actual.ct_eq(&expected.hash)) {
                return Ok(false);
            }
            control.peers.insert(id.to_owned());
            control.invitation = None;
            Ok(true)
        })
    }
}
