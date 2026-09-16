use anyhow::{Context, ensure};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use twodrive_core::AppPaths;

pub fn atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().context("missing parent")?;
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
pub fn save<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    atomic(path, &serde_json::to_vec_pretty(value)?)
}
pub fn read(path: &Path, max: usize) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= max, "local input too large");
    Ok(bytes)
}
pub fn load<T: DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    Ok(serde_json::from_slice(&read(path, 1024 * 1024)?)?)
}
pub fn default_home() -> anyhow::Result<PathBuf> {
    #[cfg(windows)]
    let root = PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA missing")?);
    #[cfg(not(windows))]
    let root = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")))
        .context("HOME missing")?;
    Ok(root.join("twodrive-peer-lab"))
}
pub fn prepare(home: &Path) -> anyhow::Result<PathBuf> {
    ensure!(
        !home.join("twodrive.sqlite3").exists() && !home.join("config.toml").exists(),
        "refusing an existing TwoDrive data/config directory"
    );
    let marker = home.join("peer-state-v1");
    if home.exists() && !marker.exists() {
        ensure!(
            fs::read_dir(home)?.next().is_none(),
            "state directory must be empty or an existing peer state directory"
        );
    }
    if marker.exists() {
        ensure!(
            read(&marker, 64)? == b"TwoDrive Peer state v1\n",
            "invalid peer state marker"
        );
    }
    fs::create_dir_all(home)?;
    atomic(&marker, b"TwoDrive Peer state v1\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(home, fs::Permissions::from_mode(0o700))?;
    }
    for folder in ["outbox", "updates"] {
        fs::create_dir_all(home.join(folder))?;
    }
    Ok(fs::canonicalize(home)?)
}
pub fn app_paths(home: &Path) -> AppPaths {
    AppPaths {
        config_dir: home.join("auth"),
        config_path: home.join("auth/config.toml"),
        data_dir: home.join("auth"),
        cache_dir: home.join("unused-cache"),
        db_path: home.join("unused.sqlite3"),
        mount_dir: home.join("unused-mount"),
        token_path: home.join("auth/tokens.dat"),
    }
}
pub fn lock(home: &Path, name: &str) -> anyhow::Result<fs::File> {
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join(name))?;
    fs2::FileExt::try_lock_exclusive(&file).context("another peer process is using this state")?;
    Ok(file)
}
// This phase exposes no remote filesystem verbs. Root selection is local-only.
// No traversal, scan or file access is performed by a cloud control message.
pub fn select_root(home: &Path, root: &Path) -> anyhow::Result<()> {
    let root = fs::canonicalize(root)?;
    ensure!(root.is_dir(), "select an existing directory");
    let path = home.join("selected-roots.json");
    let mut roots: Vec<PathBuf> = if path.exists() {
        load(&path)?
    } else {
        Vec::new()
    };
    if !roots.contains(&root) {
        roots.push(root);
    }
    save(&path, &roots)
}
