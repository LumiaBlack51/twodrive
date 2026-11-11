//! Desktop policy uses the same compiled filename rules as stable Linux.
use serde::{Deserialize, Serialize};
use std::path::Path;
use twodrive_core::NameExclusions;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SyncPolicy {
    pub exclude_name_regexes: NameExclusions,
    pub exclude_directory_names: Vec<String>,
}
impl Default for SyncPolicy {
    fn default() -> Self {
        Self {
            exclude_name_regexes: NameExclusions::default(),
            exclude_directory_names: vec!["node_modules".into()],
        }
    }
}
impl SyncPolicy {
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        if !root.join("sync-policy.json").exists() {
            return Ok(Self::default());
        }
        let path = root.join("sync-policy.json");
        anyhow::ensure!(
            std::fs::metadata(&path)?.len() <= 1024 * 1024,
            "同步排除规则过大"
        );
        let policy: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        policy.validate()?;
        Ok(policy)
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.exclude_directory_names.len() <= 256,
            "最多 256 个排除目录名"
        );
        anyhow::ensure!(
            self.exclude_directory_names.iter().all(|s| !s.is_empty()
                && s != "."
                && s != ".."
                && !s.contains(['/', '\\', '\0'])
                && s.len() <= 255),
            "排除目录必须填写单个目录名"
        );
        Ok(())
    }
    pub fn excludes_file(&self, path: &str) -> bool {
        self.exclude_name_regexes
            .matches(path.rsplit('/').next().unwrap_or(path))
    }
    pub fn excludes_backup_entry(&self, name: &str, directory: bool) -> bool {
        if directory {
            self.exclude_directory_names.iter().any(|n| n == name)
        } else {
            self.exclude_name_regexes.matches(name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn linux_policy_roundtrip_and_whole_subtree_selection() {
        let root = tempfile::tempdir().unwrap();
        let policy: SyncPolicy = serde_json::from_str(r#"{"exclude_name_regexes":["(?i)\\.bak$"],"exclude_directory_names":["node_modules","build"]}"#).unwrap();
        policy.validate().unwrap();
        std::fs::write(
            root.path().join("sync-policy.json"),
            serde_json::to_vec(&policy).unwrap(),
        )
        .unwrap();
        let restored = SyncPolicy::load(root.path()).unwrap();
        assert!(restored.excludes_file("/nested/~$report.docx"));
        assert!(restored.excludes_file("/notes.BAK"));
        assert!(restored.excludes_backup_entry("build", true));
        assert!(!restored.excludes_backup_entry("build.txt", false));
        assert!(serde_json::from_str::<SyncPolicy>(r#"{"exclude_name_regexes":["["]}"#).is_err());
        assert!(SyncPolicy::default().excludes_backup_entry("node_modules", true));
    }
}
