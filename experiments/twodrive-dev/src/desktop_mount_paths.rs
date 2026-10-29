//! Human-readable defaults. IDs remain private stable account/cache identities.
use super::*;

pub(super) fn default_path(
    home: &Path,
    profile: &Profile,
    existing: &[Profile],
) -> anyhow::Result<PathBuf> {
    let label = match profile.kind.as_str() {
        "onedrive" => "OneDrive",
        "icloud" => "iCloud Drive",
        "baidu" => "百度网盘",
        "google" => "Google Drive",
        "peer" => "Shared Drive",
        _ => "WebDAV",
    };
    let base = home.join("TwoDrive Development");
    for index in 1..=10000 {
        let name = if index == 1 {
            label.into()
        } else {
            format!("{label} ({index})")
        };
        let target = base.join(name);
        if existing
            .iter()
            .any(|p| p.id != profile.id && p.mount_path == target)
        {
            continue;
        }
        match fs::symlink_metadata(&target) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(target),
            Ok(_) => continue,
            Err(error) => return Err(error).context("无法检查默认挂载目录"),
        }
    }
    anyhow::bail!("无法分配独立挂载目录，请手动选择")
}
