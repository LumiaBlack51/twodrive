//! Provider-neutral account envelope. Provider-specific fields remain opaque and
//! flattened to preserve existing private profiles.json without a migration.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub id: String,
    pub revision: String,
    pub name: String,
    pub kind: String,
    pub writable: bool,
    pub mount_path: PathBuf,
    pub network_route: String,
    #[serde(flatten)]
    pub options: BTreeMap<String, Value>,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            id: String::new(),
            revision: String::new(),
            name: String::new(),
            kind: "dav".into(),
            writable: true,
            mount_path: PathBuf::new(),
            network_route: "auto".into(),
            options: BTreeMap::new(),
        }
    }
}
impl Profile {
    pub fn option(&self, key: &str) -> &str {
        self.options
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
}
