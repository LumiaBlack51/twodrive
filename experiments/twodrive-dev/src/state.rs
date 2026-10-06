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
        let mut output = tempfile::NamedTempFile::new_in(&self.dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            output
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        output.write_all(&serde_json::to_vec_pretty(value)?)?;
        output.as_file().sync_all()?;
        output.persist(self.dir.join(name))?;
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
        let share: Share = self.read("share.json")?;
        let parent = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        ensure!(
            !parent.canonicalize()?.starts_with(&share.root),
            "invitation must be outside the shared directory"
        );
        let invitation = Invitation {
            version: VERSION,
            address: share.address,
            secret: random_secret(),
            expires: now() + 600,
        };
        self.control(|control| {
            write_new_json(output, &invitation)?;
            control.invitation = Some(InvitationHash {
                hash: Sha256::digest(invitation.secret.as_bytes()).into(),
                expires: invitation.expires,
            });
            Ok(())
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
