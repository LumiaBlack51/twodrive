//! Lightweight provider checks shared with the isolated CFAPI compile fixtures.
//! No account setup, password derivation, network stack or recovery spool.
use crate::model::{CloudBackend, MetadataEntry};
use anyhow::ensure;
use std::path::Path;

pub(crate) fn write_permit_allowed(path: &Path, token: &str) -> bool {
    let current = path
        .parent()
        .and_then(|p| std::fs::read(p.join("write-run.json")).ok())
        .and_then(|b| serde_json::from_slice::<String>(&b).ok());
    if !current.is_some_and(|run| token.starts_with(&format!("{run}:"))) {
        return false;
    }
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<String>(&b).ok())
        .is_some_and(|v| v == token)
}

pub(crate) fn snapshot(
    backend: &dyn CloudBackend,
    path: &str,
    id: Option<&str>,
    version: Option<&str>,
) -> anyhow::Result<Option<MetadataEntry>> {
    let current = backend.get_metadata_by_path(path)?;
    match &current {
        Some(m) => ensure!(
            !m.is_dir && Some(m.remote_id.as_str()) == id && Some(m.etag.as_str()) == version,
            "云端文件已改变，未开始覆盖；请刷新后重试"
        ),
        None => ensure!(
            id.is_none() && version.is_none(),
            "云端文件已移除，未重新创建"
        ),
    }
    Ok(current)
}
