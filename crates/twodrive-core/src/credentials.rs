use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenData {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at_unix: i64,
}

#[derive(Debug, Clone)]
pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self) -> anyhow::Result<Option<TokenData>> {
        if !self.path.exists() {
            return Ok(None);
        }

        let data = String::from_utf8(crate::private_file::read_secret(&self.path)?)?;
        Ok(Some(serde_json::from_str(&data)?))
    }

    pub fn save(&self, token: &TokenData) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        crate::private_file::write_secret(&self.path, &serde_json::to_vec_pretty(token)?)?;
        Ok(())
    }

    pub fn delete(&self) -> anyhow::Result<()> {
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        Ok(())
    }
}
