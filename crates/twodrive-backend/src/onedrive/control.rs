use super::{GraphBackend, http::retry_request, paths::percent_encode_path_segment};
use crate::control::{
    ControlObject, ControlStore, MAX_CONTROL_BYTES, MAX_CONTROL_OBJECTS, validate_component,
};
use anyhow::{Context, ensure};
use serde::Deserialize;
use std::io::Read;

const GRAPH: &str = "https://graph.microsoft.com/v1.0";
const ROOT: &str = "peer-control-v1";
#[derive(Deserialize)]
struct Item {
    id: String,
    name: String,
    #[serde(default)]
    size: u64,
    folder: Option<serde_json::Value>,
}
#[derive(Deserialize)]
struct Page {
    value: Vec<Item>,
    #[serde(rename = "@odata.nextLink")]
    next: Option<String>,
}

fn bounded(mut response: reqwest::blocking::Response, max: usize) -> anyhow::Result<Vec<u8>> {
    ensure!(
        response.content_length().unwrap_or(0) <= max as u64,
        "control response too large"
    );
    let mut bytes = Vec::new();
    (&mut response)
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= max, "control response too large");
    Ok(bytes)
}
fn segment(s: &str) -> String {
    percent_encode_path_segment(s)
}
fn validate_next(url: &str, expected_path: &str) -> anyhow::Result<()> {
    let u = url::Url::parse(url)?;
    ensure!(
        u.scheme() == "https"
            && u.host_str() == Some("graph.microsoft.com")
            && u.port_or_known_default() == Some(443)
            && u.username().is_empty()
            && u.password().is_none()
            && u.fragment().is_none()
            && u.path() == expected_path,
        "unsafe Graph pagination URL"
    );
    Ok(())
}
impl GraphBackend {
    fn control_folder(&self, parent: &str, name: &str) -> anyhow::Result<String> {
        validate_component(name)?;
        let url = format!("{GRAPH}/me/drive/items/{}:/{}", segment(parent), name);
        let token = self.access_token()?;
        let response = self.client.get(&url).bearer_auth(&token).send()?;
        let item: Item = if response.status().is_success() {
            serde_json::from_slice(&bounded(response, MAX_CONTROL_BYTES)?)?
        } else {
            ensure!(
                response.status().as_u16() == 404,
                "Graph control folder lookup failed: {}",
                response.status()
            );
            let create = format!("{GRAPH}/me/drive/items/{}/children", segment(parent));
            let response = self
                .client
                .post(create)
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "name": name, "folder": {}, "@microsoft.graph.conflictBehavior": "fail"
                }))
                .send()?;
            if response.status().as_u16() == 409 {
                serde_json::from_slice(&bounded(self.get_with_retry(&url)?, MAX_CONTROL_BYTES)?)?
            } else {
                ensure!(
                    response.status().is_success(),
                    "Graph control folder creation failed: {}",
                    response.status()
                );
                serde_json::from_slice(&bounded(response, MAX_CONTROL_BYTES)?)?
            }
        };
        ensure!(item.folder.is_some(), "control namespace is not a folder");
        Ok(item.id)
    }
    fn control_bucket(&self, bucket: &str) -> anyhow::Result<String> {
        validate_component(bucket)?;
        let root: Item = serde_json::from_slice(&bounded(
            self.get_with_retry(&format!("{GRAPH}/me/drive/special/approot"))?,
            MAX_CONTROL_BYTES,
        )?)?;
        ensure!(root.folder.is_some(), "app root is not a folder");
        let namespace = self.control_folder(&root.id, ROOT)?;
        self.control_folder(&namespace, bucket)
    }
}
impl ControlStore for GraphBackend {
    fn list(&self, bucket: &str) -> anyhow::Result<Vec<ControlObject>> {
        let folder = self.control_bucket(bucket)?;
        let path = format!("/v1.0/me/drive/items/{}/children", segment(&folder));
        let mut url =
            format!("https://graph.microsoft.com{path}?$select=id,name,size,folder&$top=200");
        let mut out = Vec::new();
        for _ in 0..8 {
            validate_next(&url, &path)?;
            let page: Page =
                serde_json::from_slice(&bounded(self.get_with_retry(&url)?, 256 * 1024)?)?;
            for item in page.value {
                ensure!(
                    out.len() < MAX_CONTROL_OBJECTS,
                    "control mailbox full; manual cleanup required"
                );
                if item.folder.is_none() && validate_component(&item.name).is_ok() {
                    out.push(ControlObject {
                        name: item.name,
                        size: item.size,
                    });
                }
            }
            match page.next {
                Some(next) => url = next,
                None => return Ok(out),
            }
        }
        anyhow::bail!("control pagination limit reached")
    }
    fn get(&self, bucket: &str, name: &str) -> anyhow::Result<Vec<u8>> {
        validate_component(name)?;
        let folder = self.control_bucket(bucket)?;
        bounded(
            self.get_with_retry(&format!(
                "{GRAPH}/me/drive/items/{}:/{name}:/content",
                segment(&folder)
            ))?,
            MAX_CONTROL_BYTES,
        )
    }
    fn put(&self, bucket: &str, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
        validate_component(name)?;
        ensure!(
            bytes.len() <= MAX_CONTROL_BYTES,
            "control message too large"
        );
        let items = self.list(bucket)?;
        ensure!(
            items.len() < MAX_CONTROL_OBJECTS || items.iter().any(|i| i.name == name),
            "control mailbox full"
        );
        let folder = self.control_bucket(bucket)?;
        let url = format!(
            "{GRAPH}/me/drive/items/{}:/{name}:/content",
            segment(&folder)
        );
        let token = self.access_token()?;
        retry_request(|| {
            self.client
                .put(&url)
                .bearer_auth(&token)
                .header("Content-Type", "application/octet-stream")
                .body(bytes.to_vec())
        })?;
        Ok(())
    }
    fn delete(&self, bucket: &str, name: &str) -> anyhow::Result<()> {
        validate_component(name)?;
        let folder = self.control_bucket(bucket)?;
        let url = format!("{GRAPH}/me/drive/items/{}:/{name}", segment(&folder));
        let token = self.access_token()?;
        let response = self.client.get(&url).bearer_auth(&token).send()?;
        if response.status().as_u16() == 404 {
            return Ok(());
        }
        ensure!(
            response.status().is_success(),
            "control delete lookup failed"
        );
        let item: Item = serde_json::from_slice(&bounded(response, MAX_CONTROL_BYTES)?)?;
        let delete_url = format!(
            "{GRAPH}/me/drive/items/{}/permanentDelete",
            segment(&item.id)
        );
        retry_request(|| self.client.post(&delete_url).bearer_auth(&token))
            .context("control permanent delete failed")?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pagination_cannot_exfiltrate_bearer_or_escape_bucket() {
        let path = "/v1.0/me/drive/items/abc/children";
        assert!(
            validate_next(
                &format!("{GRAPH}/me/drive/items/abc/children?$skiptoken=x"),
                path
            )
            .is_ok()
        );
        for url in [
            "https://evil.test/v1.0/me/drive/items/abc/children",
            "http://graph.microsoft.com/v1.0/me/drive/items/abc/children",
            "https://graph.microsoft.com/v1.0/me/drive/root/children",
        ] {
            assert!(validate_next(url, path).is_err());
        }
    }
}
