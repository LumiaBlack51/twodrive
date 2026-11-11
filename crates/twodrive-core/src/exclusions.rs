use regex::{RegexSet, RegexSetBuilder};
use serde::{Deserialize, Serialize};

/// Compiled once on configuration load. Matching uses a basename, not a path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "Vec<String>", into = "Vec<String>")]
pub struct NameExclusions {
    patterns: Vec<String>,
    matcher: RegexSet,
}

impl NameExclusions {
    pub fn new(patterns: Vec<String>) -> anyhow::Result<Self> {
        anyhow::ensure!(
            patterns.len() <= 256,
            "too many filename exclusion regexes (maximum 256)"
        );
        let matcher = RegexSetBuilder::new(&patterns)
            .size_limit(1024 * 1024)
            .build()?;
        Ok(Self { patterns, matcher })
    }

    pub fn matches(&self, name: &str) -> bool {
        // OneDrive rejects Office owner filenames. This rule cannot be disabled.
        name.starts_with("~$") || self.matcher.is_match(name)
    }
}

impl Default for NameExclusions {
    fn default() -> Self {
        Self::new(Vec::new()).expect("empty exclusion set is valid")
    }
}

impl TryFrom<Vec<String>> for NameExclusions {
    type Error = anyhow::Error;
    fn try_from(patterns: Vec<String>) -> Result<Self, Self::Error> {
        Self::new(patterns)
    }
}

impl From<NameExclusions> for Vec<String> {
    fn from(value: NameExclusions) -> Self {
        value.patterns
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filename_regexes_validate_roundtrip_and_keep_owner_rule() {
        let config: crate::Config = toml::from_str(
            r#"[sync]
exclude_name_regexes = ['(?i)\.bak$', '^scratch-[0-9]+\.txt$']
"#,
        )
        .unwrap();
        let rules = &config.sync.exclude_name_regexes;
        for name in [
            "notes.BAK",
            "scratch-42.txt",
            "~$report.docx",
            "~$report.xlsx",
            "~$report.pptx",
        ] {
            assert!(rules.matches(name), "{name}");
        }
        for name in ["notes.bak.docx", "scratch-x.txt", "report.pptx"] {
            assert!(!rules.matches(name));
        }
        let text = toml::to_string(&config).unwrap();
        let roundtrip: crate::Config = toml::from_str(&text).unwrap();
        assert!(roundtrip.sync.exclude_name_regexes.matches("notes.BAK"));
        assert!(toml::from_str::<crate::Config>("[sync]\nexclude_name_regexes = ['[']").is_err());
        assert!(NameExclusions::default().matches("~$report.docx"));
    }
}
