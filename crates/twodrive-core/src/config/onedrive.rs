//! Legacy OneDrive configuration schema. Kept in core to preserve the public
//! Config type and on-disk TOML without a dependency cycle with the provider.
//! OAuth and all Graph operations belong to twodrive_backend::onedrive.
use crate::{AppPaths, Config};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphConfig {
    #[serde(deserialize_with = "deserialize_client_id")]
    pub client_id: String,
    pub tenant: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
}

// Public desktop application identifier, not a client secret.
pub const DEFAULT_GRAPH_CLIENT_ID: &str = "178705ac-2286-441b-9652-1a4d86be2c51";

fn deserialize_client_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    Ok(match value.trim() {
        "" | "PASTE_AZURE_APP_CLIENT_ID_HERE" | "YOUR_AZURE_APP_CLIENT_ID" => {
            DEFAULT_GRAPH_CLIENT_ID.to_string()
        }
        _ => value,
    })
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            client_id: DEFAULT_GRAPH_CLIENT_ID.to_string(),
            tenant: "common".to_string(),
            redirect_uri: "http://localhost:53682".to_string(),
            scopes: vec![
                "Files.ReadWrite".to_string(),
                "User.Read".to_string(),
                "offline_access".to_string(),
                "openid".to_string(),
                "profile".to_string(),
            ],
        }
    }
}

impl Config {
    pub fn validate_graph_login(&self) -> anyhow::Result<()> {
        if self.graph.client_id.trim().is_empty()
            || self.graph.client_id == "PASTE_AZURE_APP_CLIENT_ID_HERE"
        {
            anyhow::bail!(
                "set graph.client_id in {} before running login",
                AppPaths::discover()?.config_path.display()
            );
        }
        Ok(())
    }
}
