use std::path::Path;
use twodrive_core::{AppPaths, normalize_cloud_path};

pub(crate) fn required_path(args: &[String]) -> anyhow::Result<&str> {
    args.first()
        .map(String::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing cloud path argument, for example /Documents/a.pdf"))
}

pub(crate) fn cloud_path_from_arg(paths: &AppPaths, value: &str) -> String {
    let path = Path::new(value);
    if path.is_absolute()
        && let Ok(stripped) = path.strip_prefix(&paths.mount_dir)
    {
        return normalize_cloud_path(&stripped.to_string_lossy());
    }
    normalize_cloud_path(value)
}
