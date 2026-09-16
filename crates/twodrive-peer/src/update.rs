//! Independent release trust. OneDrive supplies a tag hint only, never a key, URL or command.
use crate::{
    identity::{bytes32, valid_id, verify},
    local,
};
use anyhow::{Context, ensure};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
const KEY: &str = include_str!("release-public-key.hex");
const MAX_EXE: usize = 128 * 1024 * 1024;
const REPOSITORY: &str = "https://github.com/LumiaBlack51/twodrive/releases/download";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    pub schema: u8,
    pub product: String,
    pub version: Version,
    pub platform: String,
    pub file: String,
    pub size: u64,
    pub sha256: String,
    pub signature: String,
}
impl Release {
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut r = self.clone();
        r.signature.clear();
        let mut b = b"TwoDrive/release/v1\0".to_vec();
        b.extend(serde_json::to_vec(&r).unwrap());
        b
    }
    fn verify_key(&self, key: &str, platform: &str, floor: &Version) -> anyhow::Result<()> {
        ensure!(
            self.schema == 1 && self.product == "twodrive-peer",
            "wrong update product/schema"
        );
        ensure!(
            self.platform == platform && self.file == executable_name(platform),
            "wrong update platform/file"
        );
        ensure!(
            &self.version > floor && self.version.pre.is_empty() && self.version.build.is_empty(),
            "update must be a newer stable version"
        );
        ensure!(
            self.size > 0 && self.size <= MAX_EXE as u64 && valid_id(&self.sha256),
            "invalid update size/hash"
        );
        bytes32(key)?;
        verify(key, &self.signing_bytes(), &self.signature).context("release signature rejected")
    }
    fn check_bytes(&self, bytes: &[u8]) -> anyhow::Result<()> {
        ensure!(
            bytes.len() as u64 == self.size && hex::encode(Sha256::digest(bytes)) == self.sha256,
            "release file hash/size mismatch"
        );
        Ok(())
    }
}
fn executable_name(platform: &str) -> &'static str {
    if platform == "windows-x86_64" {
        "twodrive-peer.exe"
    } else {
        "twodrive-peer"
    }
}
pub fn validate_tag(tag: &str) -> anyhow::Result<Version> {
    ensure!(tag.len() < 64, "invalid release tag");
    let v = Version::parse(
        tag.strip_prefix("peer-v")
            .context("invalid release tag prefix")?,
    )?;
    ensure!(
        v.pre.is_empty() && v.build.is_empty() && tag == format!("peer-v{v}"),
        "invalid release tag"
    );
    Ok(v)
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateState {
    pub active: Option<Version>,
    pub previous: Option<Version>,
    pub pending: Option<Version>,
    pub highwater: Option<Version>,
}
impl UpdateState {
    pub fn load(home: &Path) -> anyhow::Result<Self> {
        let path = home.join("updates/state.json");
        if path.exists() {
            local::load(&path)
        } else {
            Ok(Self::default())
        }
    }
    pub fn save(&self, home: &Path) -> anyhow::Result<()> {
        local::save(&home.join("updates/state.json"), self)
    }
    pub fn floor(&self) -> Version {
        self.highwater
            .clone()
            .unwrap_or_else(|| Version::parse(crate::VERSION).unwrap())
            .max(Version::parse(crate::VERSION).unwrap())
    }
}
fn version_dir(home: &Path, version: &Version) -> PathBuf {
    home.join("updates").join(version.to_string())
}
fn read_release(path: &Path) -> anyhow::Result<Release> {
    Ok(serde_json::from_slice(&local::read(path, 8192)?)?)
}
fn fetch(url: &str, max: usize) -> anyhow::Result<Vec<u8>> {
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(180))
        .build()?;
    let response = client.get(url).send()?.error_for_status()?;
    ensure!(
        response.content_length().unwrap_or(0) <= max as u64,
        "release download too large"
    );
    let mut bytes = Vec::new();
    response.take(max as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= max, "release download too large");
    Ok(bytes)
}
pub fn download_and_stage(home: &Path, tag: &str) -> anyhow::Result<()> {
    let version = validate_tag(tag)?;
    let platform = crate::identity::platform();
    ensure!(
        platform == "windows-x86_64",
        "automatic install currently supports Windows x86-64"
    );
    let state = UpdateState::load(home)?;
    ensure!(version > state.floor(), "release notification is not newer");
    let root = format!("{REPOSITORY}/{tag}");
    let manifest = fetch(&format!("{root}/twodrive-peer-windows-x86_64.json"), 8192)?;
    let release: Release = serde_json::from_slice(&manifest)?;
    release.verify_key(KEY.trim(), platform, &state.floor())?;
    ensure!(release.version == version, "release tag/version mismatch");
    let bytes = fetch(&format!("{root}/twodrive-peer.exe"), release.size as usize)?;
    stage(home, &release, &bytes, KEY.trim(), platform)
}
pub fn stage_local(home: &Path, manifest: &Path, binary: &Path) -> anyhow::Result<()> {
    let r = read_release(manifest)?;
    r.verify_key(
        KEY.trim(),
        crate::identity::platform(),
        &UpdateState::load(home)?.floor(),
    )?;
    stage(
        home,
        &r,
        &local::read(binary, MAX_EXE)?,
        KEY.trim(),
        crate::identity::platform(),
    )
}
fn stage(
    home: &Path,
    release: &Release,
    bytes: &[u8],
    key: &str,
    platform: &str,
) -> anyhow::Result<()> {
    let _lock = local::lock(home, "update.lock")?;
    let mut state = UpdateState::load(home)?;
    release.verify_key(key, platform, &state.floor())?;
    release.check_bytes(bytes)?;
    let updates = home.join("updates");
    fs::create_dir_all(&updates)?;
    let dir = tempfile::tempdir_in(&updates)?;
    local::atomic(&dir.path().join(&release.file), bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            dir.path().join(&release.file),
            fs::Permissions::from_mode(0o700),
        )?;
    }
    local::save(&dir.path().join("release.json"), release)?;
    let dest = version_dir(home, &release.version);
    // Retrying an already staged identical version is safe. Never overwrite a running executable.
    if dest.exists() {
        let existing = local::read(&dest.join(&release.file), MAX_EXE)?;
        release.check_bytes(&existing)?;
        ensure!(
            read_release(&dest.join("release.json"))?.signature == release.signature,
            "existing update manifest differs"
        );
    } else {
        fs::rename(dir.path(), &dest)?;
    }
    state.pending = Some(release.version.clone());
    state.save(home)
}
fn verify_installed(
    home: &Path,
    version: &Version,
    key: &str,
    platform: &str,
) -> anyhow::Result<PathBuf> {
    let dir = version_dir(home, version);
    let r = read_release(&dir.join("release.json"))?;
    // An installed version need not exceed the anti-rollback floor, but must retain its release proof.
    r.verify_key(key, platform, &Version::new(0, 0, 0))?;
    ensure!(&r.version == version, "installed version mismatch");
    let path = dir.join(&r.file);
    r.check_bytes(&local::read(&path, MAX_EXE)?)?;
    Ok(path)
}
pub fn health_check(path: &Path, version: &Version) -> anyhow::Result<()> {
    let output = tempfile::NamedTempFile::new()?;
    let mut child = Command::new(path)
        .arg("health-check")
        .arg("--output")
        .arg(output.path())
        .spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "candidate health check failed");
            break;
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("candidate health check timeout");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let actual: serde_json::Value = local::load(output.path())?;
    ensure!(
        actual["version"] == version.to_string()
            && actual["platform"] == crate::identity::platform()
            && actual["ok"] == true,
        "candidate health response mismatch"
    );
    Ok(())
}
fn activate<F>(home: &Path, key: &str, platform: &str, health: F) -> anyhow::Result<()>
where
    F: FnOnce(&Path, &Version) -> anyhow::Result<()>,
{
    let _lock = local::lock(home, "update.lock")?;
    let mut state = UpdateState::load(home)?;
    let Some(version) = state.pending.clone() else {
        return Ok(());
    };
    let result = (|| {
        ensure!(version > state.floor(), "pending rollback rejected");
        let path = verify_installed(home, &version, key, platform)?;
        health(&path, &version)?;
        Ok(())
    })();
    state.pending = None;
    if result.is_ok() {
        state.previous = state.active.clone();
        state.active = Some(version.clone());
        state.highwater = Some(version);
    }
    state.save(home)?;
    result
}
pub fn activate_pending(home: &Path) -> anyhow::Result<()> {
    activate(home, KEY.trim(), crate::identity::platform(), health_check)
}
pub fn active_executable(home: &Path, bootstrap: &Path) -> anyhow::Result<PathBuf> {
    match UpdateState::load(home)?.active {
        Some(v) => verify_installed(home, &v, KEY.trim(), crate::identity::platform()),
        None => Ok(bootstrap.to_path_buf()),
    }
}
pub fn rollback(home: &Path) -> anyhow::Result<()> {
    let _lock = local::lock(home, "update.lock")?;
    let mut state = UpdateState::load(home)?;
    state.active = state.previous.take();
    state.pending = None;
    state.save(home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;
    fn package() -> (SigningKey, Release, Vec<u8>) {
        let key = SigningKey::generate(&mut OsRng);
        let bytes = b"synthetic executable fixture".to_vec();
        let mut r = Release {
            schema: 1,
            product: "twodrive-peer".into(),
            version: Version::new(0, 1, 1),
            platform: "windows-x86_64".into(),
            file: "twodrive-peer.exe".into(),
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(&bytes)),
            signature: String::new(),
        };
        r.signature = hex::encode(key.sign(&r.signing_bytes()).to_bytes());
        (key, r, bytes)
    }
    #[test]
    fn normal_update_and_failed_health_preserve_previous_and_highwater() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join("updates")).unwrap();
        let (key, mut r, bytes) = package();
        let public = hex::encode(key.verifying_key().as_bytes());
        stage(home.path(), &r, &bytes, &public, "windows-x86_64").unwrap();
        activate(home.path(), &public, "windows-x86_64", |_, _| Ok(())).unwrap();
        assert_eq!(
            UpdateState::load(home.path()).unwrap().active,
            Some(r.version.clone())
        );
        assert!(stage(home.path(), &r, &bytes, &public, "windows-x86_64").is_err());
        r.version = Version::new(0, 1, 2);
        r.signature = hex::encode(key.sign(&r.signing_bytes()).to_bytes());
        stage(home.path(), &r, &bytes, &public, "windows-x86_64").unwrap();
        assert!(
            activate(
                home.path(),
                &public,
                "windows-x86_64",
                |_, _| anyhow::bail!("simulated launch failure")
            )
            .is_err()
        );
        let state = UpdateState::load(home.path()).unwrap();
        assert_eq!(state.active, Some(Version::new(0, 1, 1)));
        assert_eq!(state.highwater, state.active);
        assert!(state.pending.is_none());
        assert!(
            version_dir(home.path(), &Version::new(0, 1, 1))
                .join("twodrive-peer.exe")
                .exists()
        );
    }
    #[test]
    fn invalid_signature_corruption_wrong_platform_version_and_product_are_rejected() {
        let home = tempfile::tempdir().unwrap();
        let (key, r, bytes) = package();
        let public = hex::encode(key.verifying_key().as_bytes());
        let mut bad = r.clone();
        bad.signature = "00".repeat(64);
        assert!(stage(home.path(), &bad, &bytes, &public, "windows-x86_64").is_err());
        assert!(stage(home.path(), &r, b"damaged", &public, "windows-x86_64").is_err());
        assert!(stage(home.path(), &r, &bytes, &public, "linux-x86_64").is_err());
        for mutate in 0..4 {
            let mut bad = r.clone();
            match mutate {
                0 => bad.version = Version::new(0, 0, 1),
                1 => bad.file = "../evil.exe".into(),
                2 => bad.product = "other".into(),
                _ => bad.schema = 2,
            }
            bad.signature = hex::encode(key.sign(&bad.signing_bytes()).to_bytes());
            assert!(stage(home.path(), &bad, &bytes, &public, "windows-x86_64").is_err());
        }
        assert!(!home.path().join("updates/state.json").exists());
    }
    #[test]
    fn corruption_after_staging_and_interrupted_stage_do_not_replace_active() {
        let home = tempfile::tempdir().unwrap();
        let (key, r, bytes) = package();
        let public = hex::encode(key.verifying_key().as_bytes());
        stage(home.path(), &r, &bytes, &public, "windows-x86_64").unwrap();
        fs::write(version_dir(home.path(), &r.version).join(&r.file), b"bad").unwrap();
        assert!(
            activate(home.path(), &public, "windows-x86_64", |_, _| panic!(
                "must not execute"
            ))
            .is_err()
        );
        assert!(UpdateState::load(home.path()).unwrap().active.is_none());
        assert!(active_executable(home.path(), Path::new("bootstrap.exe")).is_ok());
    }
    #[test]
    fn tag_cannot_choose_url_or_path() {
        for tag in [
            "https://evil",
            "peer-v../x",
            "peer-v0.2.0/evil",
            "peer-v0.2.0+foo",
            "peer-v0.2.0-beta",
        ] {
            assert!(validate_tag(tag).is_err());
        }
        assert_eq!(validate_tag("peer-v0.2.0").unwrap(), Version::new(0, 2, 0));
    }
    #[test]
    #[ignore = "requires TWODRIVE_TEST_UPDATE_EXE native fixture; explicitly run in native CI"]
    fn native_process_install_health_and_start_failure_rollback() {
        let exe = std::env::var_os("TWODRIVE_TEST_UPDATE_EXE").expect("native fixture required");
        let bytes = std::fs::read(exe).unwrap();
        let (key, mut release, _) = package();
        let public = hex::encode(key.verifying_key().as_bytes());
        release.platform = crate::identity::platform().into();
        release.file = executable_name(&release.platform).into();
        release.size = bytes.len() as u64;
        release.sha256 = hex::encode(Sha256::digest(&bytes));
        release.signature = hex::encode(key.sign(&release.signing_bytes()).to_bytes());
        let home = tempfile::tempdir().unwrap();
        stage(home.path(), &release, &bytes, &public, &release.platform).unwrap();
        activate(home.path(), &public, &release.platform, health_check).unwrap();
        assert_eq!(
            UpdateState::load(home.path()).unwrap().active,
            Some(Version::new(0, 1, 1))
        );
        // A correctly signed but non-executable next package must fail actual spawning.
        let broken = b"validly signed, cannot execute";
        release.version = Version::new(0, 1, 2);
        release.size = broken.len() as u64;
        release.sha256 = hex::encode(Sha256::digest(broken));
        release.signature = hex::encode(key.sign(&release.signing_bytes()).to_bytes());
        stage(home.path(), &release, broken, &public, &release.platform).unwrap();
        assert!(activate(home.path(), &public, &release.platform, health_check).is_err());
        assert_eq!(
            UpdateState::load(home.path()).unwrap().active,
            Some(Version::new(0, 1, 1))
        );
        // Simulate post-activation worker failure and retain anti-rollback highwater.
        let mut state = UpdateState::load(home.path()).unwrap();
        state.previous = state.active.clone();
        state.active = Some(Version::new(0, 1, 2));
        state.highwater = state.active.clone();
        state.save(home.path()).unwrap();
        rollback(home.path()).unwrap();
        let state = UpdateState::load(home.path()).unwrap();
        assert_eq!(state.active, Some(Version::new(0, 1, 1)));
        assert_eq!(state.highwater, Some(Version::new(0, 1, 2)));
    }
}
