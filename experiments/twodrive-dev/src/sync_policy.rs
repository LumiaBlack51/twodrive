//! Desktop policy uses the same compiled filename rules as stable Linux.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use twodrive_core::NameExclusions;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SyncPolicy {
    pub exclude_name_regexes: NameExclusions,
    pub exclude_directory_names: Vec<String>,
    pub exclude_directory_paths: Vec<PathBuf>,
    pub exclude_filenames: Vec<String>,
    pub exclude_extensions: Vec<String>,
    pub exclude_wildcards: Vec<String>,
}
impl Default for SyncPolicy {
    fn default() -> Self {
        Self {
            exclude_name_regexes: NameExclusions::default(),
            exclude_directory_names: vec!["node_modules".into()],
            exclude_directory_paths: vec![],
            exclude_filenames: vec![],
            exclude_extensions: vec![],
            exclude_wildcards: vec![],
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
        anyhow::ensure!(
            self.exclude_directory_paths.len() <= 256
                && self.exclude_directory_paths.iter().all(|p| p.is_absolute()),
            "请选择绝对目录"
        );
        for names in [
            &self.exclude_filenames,
            &self.exclude_extensions,
            &self.exclude_wildcards,
        ] {
            anyhow::ensure!(
                names.len() <= 256
                    && names
                        .iter()
                        .all(|n| !n.is_empty() && n.len() <= 255 && !n.contains(['/', '\\', '\0'])),
                "文件排除规则无效"
            );
        }
        Ok(())
    }
    pub fn canonical_backup_exclusions(&self) -> Vec<PathBuf> {
        self.exclude_directory_paths
            .iter()
            .map(|p| backup_path_key(p))
            .collect()
    }

    pub fn excludes_file(&self, path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        self.exclude_name_regexes.matches(name)
            || self.exclude_filenames.iter().any(|n| n == name)
            || self.exclude_extensions.iter().any(|ext| {
                name.get(name.len().saturating_sub(ext.len() + 1)..)
                    .and_then(|s| s.strip_prefix('.'))
                    .is_some_and(|s| s.eq_ignore_ascii_case(ext))
            })
            || self
                .exclude_wildcards
                .iter()
                .any(|pattern| glob_matches(pattern, name))
    }
    pub fn excludes_backup_entry(&self, name: &str, directory: bool) -> bool {
        if directory {
            self.exclude_directory_names.iter().any(|n| n == name)
        } else {
            self.excludes_file(name)
        }
    }
}

// A saved exclusion can disappear, become a file, or become inaccessible.
// Resolve its reachable prefix so its original location remains excluded.
pub fn backup_path_key(path: &Path) -> PathBuf {
    for ancestor in path.ancestors() {
        if let Ok(resolved) = ancestor.canonicalize() {
            if ancestor == path {
                return resolved;
            }
            if let Ok(tail) = path.strip_prefix(ancestor) {
                return resolved.join(tail);
            }
        }
    }
    path.to_path_buf()
}

// Match a complete filename. Only '*' and '?' have special meaning.
fn glob_matches(pattern: &str, name: &str) -> bool {
    let mut rest = name;
    let mut pat = pattern;
    let mut star = None;
    while !rest.is_empty() {
        let first = rest.chars().next().unwrap();
        match pat.chars().next() {
            Some('*') => {
                pat = &pat[1..];
                star = Some((pat, rest));
            }
            Some(c) if c == '?' || c == first => {
                pat = &pat[c.len_utf8()..];
                rest = &rest[first.len_utf8()..];
            }
            _ => match star {
                Some((suffix, start)) if !start.is_empty() => {
                    rest = &start[start.chars().next().unwrap().len_utf8()..];
                    pat = suffix;
                    star = Some((suffix, rest));
                }
                _ => return false,
            },
        }
    }
    pat.trim_matches('*').is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_rules_match_literal_names_extensions_and_unicode_wildcards() {
        let policy: SyncPolicy = serde_json::from_value(serde_json::json!({
            "exclude_filenames":["草稿[1].txt"],"exclude_extensions":["bak"],
            "exclude_wildcards":["报告-?.*"]
        }))
        .unwrap();
        for name in ["草稿[1].txt", "note.BAK", "报告-甲.pdf"] {
            assert!(policy.excludes_file(name));
        }
        for name in ["草稿1.txt", "note.bak.txt", "报告-甲乙.pdf"] {
            assert!(!policy.excludes_file(name));
        }
    }
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
