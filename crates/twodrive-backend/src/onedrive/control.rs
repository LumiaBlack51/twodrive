use super::{GraphBackend, paths::percent_encode_path_segment};
use crate::control::{
    ControlObject, ControlStore, MAX_CONTROL_BYTES, MAX_CONTROL_OBJECTS, validate_component,
};
use anyhow::ensure;
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
fn decoded_segments(path: &str) -> anyhow::Result<Vec<String>> {
    path.split('/')
        .map(|segment| {
            Ok(percent_encoding::percent_decode_str(segment)
                .decode_utf8()?
                .into_owned())
        })
        .collect()
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
            && decoded_segments(u.path())? == decoded_segments(expected_path)?,
        "unsafe Graph pagination URL"
    );
    Ok(())
}
// This provider never formats reqwest/serde errors or remote bodies. Only these
// typed, locally constructed diagnostics may cross the peer CLI log boundary.
impl GraphBackend {
    fn control_request(
        &self,
        operation: &'static str,
        method: reqwest::Method,
        url: &str,
        body: Option<Vec<u8>>,
        allowed: &[u16],
    ) -> anyhow::Result<reqwest::blocking::Response> {
        use crate::control::ControlDiagnostic;
        let diagnostic = |status, code| ControlDiagnostic {
            operation,
            status,
            code,
        };
        let token = self
            .access_token()
            .map_err(|_| diagnostic(None, "token-acquisition-or-refresh"))?;
        for attempt in 1..=3 {
            let mut request = self.client.request(method.clone(), url).bearer_auth(&token);
            request = control_body(request, &method, body.as_deref());
            let response = match request.send() {
                Ok(response) => response,
                Err(error) => {
                    let code = if error.is_timeout() {
                        "timeout"
                    } else if error.is_connect() {
                        "connect-or-tls"
                    } else if error.is_redirect() {
                        "redirect"
                    } else {
                        "transport"
                    };
                    return Err(diagnostic(None, code).into());
                }
            };
            let status = response.status().as_u16();
            if std::env::var_os("TWODRIVE_PEER_DIAGNOSTICS").is_some() {
                eprintln!("control operation={operation} status={status} attempt={attempt}");
            }
            if response.status().is_success() || allowed.contains(&status) {
                return Ok(response);
            }
            let delay = super::http::retry_after_delay(response.headers(), attempt);
            let code = safe_graph_code(&bounded(response, MAX_CONTROL_BYTES).unwrap_or_default());
            if attempt < 3 && (status == 429 || status >= 500) {
                eprintln!("{}; retrying", diagnostic(Some(status), code));
                std::thread::sleep(delay);
            } else {
                return Err(diagnostic(Some(status), code).into());
            }
        }
        unreachable!()
    }
    fn control_json<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::blocking::Response,
        operation: &'static str,
        max: usize,
    ) -> anyhow::Result<T> {
        let status = Some(response.status().as_u16());
        let bytes = bounded(response, max).map_err(|_| crate::control::ControlDiagnostic {
            operation,
            status,
            code: "response-read-or-size",
        })?;
        serde_json::from_slice(&bytes).map_err(|_| {
            crate::control::ControlDiagnostic {
                operation,
                status,
                code: "response-schema",
            }
            .into()
        })
    }
    fn control_folder(&self, parent: &str, name: &str) -> anyhow::Result<String> {
        validate_component(name)?;
        let lookup = if name == ROOT {
            "namespace.lookup"
        } else {
            "bucket.lookup"
        };
        let create_op = if name == ROOT {
            "namespace.create"
        } else {
            "bucket.create"
        };
        let url = format!("{GRAPH}/me/drive/items/{}:/{}", segment(parent), name);
        let response = self.control_request(lookup, reqwest::Method::GET, &url, None, &[404])?;
        let item: Item = if response.status().as_u16() != 404 {
            self.control_json(response, lookup, MAX_CONTROL_BYTES)?
        } else {
            let create = format!("{GRAPH}/me/drive/items/{}/children", segment(parent));
            let body = serde_json::to_vec(
                &serde_json::json!({"name":name,"folder":{},"@microsoft.graph.conflictBehavior":"fail"}),
            )?;
            let response = self.control_request(
                create_op,
                reqwest::Method::POST,
                &create,
                Some(body),
                &[409],
            )?;
            if response.status().as_u16() == 409 {
                let response =
                    self.control_request(lookup, reqwest::Method::GET, &url, None, &[])?;
                self.control_json(response, lookup, MAX_CONTROL_BYTES)?
            } else {
                self.control_json(response, create_op, MAX_CONTROL_BYTES)?
            }
        };
        ensure!(item.folder.is_some(), "control namespace is not a folder");
        Ok(item.id)
    }
    fn control_bucket(&self, bucket: &str) -> anyhow::Result<String> {
        validate_component(bucket)?;
        if let Some((time, id)) = self
            .control_folders
            .lock()
            .map_err(|_| anyhow::anyhow!("control cache lock poisoned"))?
            .get(bucket)
            && time.elapsed() < std::time::Duration::from_secs(60)
        {
            return Ok(id.clone());
        }
        // Graph creates AppFolder on this first GET; no separate POST is required.
        let response = self.control_request(
            "approot.resolve-or-create",
            reqwest::Method::GET,
            &format!("{GRAPH}/me/drive/special/approot"),
            None,
            &[],
        )?;
        let root: Item = self.control_json(response, "approot.decode", MAX_CONTROL_BYTES)?;
        ensure!(root.folder.is_some(), "app root is not a folder");
        let namespace = self.control_folder(&root.id, ROOT)?;
        let id = self.control_folder(&namespace, bucket)?;
        let mut cache = self
            .control_folders
            .lock()
            .map_err(|_| anyhow::anyhow!("control cache lock poisoned"))?;
        cache.retain(|_, (time, _)| time.elapsed() < std::time::Duration::from_secs(60));
        ensure!(cache.len() < 128, "control bucket cache full");
        cache.insert(bucket.into(), (std::time::Instant::now(), id.clone()));
        Ok(id)
    }
    /// Live probe only in the isolated peer namespace; never prints tokens or IDs.
    pub fn control_doctor(&self) -> anyhow::Result<()> {
        println!(
            "auth configured_appfolder={} (configuration is not proof of granted scope)",
            self.config
                .graph
                .scopes
                .iter()
                .any(|s| s == "Files.ReadWrite.AppFolder")
        );
        if let Ok(token) = self.token.lock() {
            println!(
                "auth expired_or_expiring={} refresh_present={}",
                token.expires_at_unix <= twodrive_core::now_unix() + 60,
                token.refresh_token.is_some()
            );
        }
        let name = format!(
            "probe-{}.json",
            super::auth::random_string(24).to_ascii_lowercase()
        );
        let bucket = "diagnostics";
        let bytes = b"{\"twodrive_peer_probe\":1}";
        self.put(bucket, &name, bytes)?;
        println!("doctor put=ok");
        let read = self.get(bucket, &name);
        // Delete only the unique object created by this invocation, even if GET fails.
        let cleanup = self.delete(bucket, &name);
        match &cleanup {
            Ok(()) => println!("doctor delete=ok"),
            Err(error) => eprintln!(
                "doctor cleanup failed: {}",
                crate::control::safe_diagnostic(error)
            ),
        }
        let read = read?;
        ensure!(read == bytes, "probe content mismatch");
        println!("doctor get=ok content_match=true");
        cleanup?;
        ensure!(
            !self.list(bucket)?.iter().any(|item| item.name == name),
            "probe deletion not visible"
        );
        println!("doctor list=ok deleted_object_absent=true");
        Ok(())
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
            let response = self.control_request("list", reqwest::Method::GET, &url, None, &[])?;
            let page: Page = self.control_json(response, "list.decode", 256 * 1024)?;
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
        let url = format!(
            "{GRAPH}/me/drive/items/{}:/{name}:/content",
            segment(&folder)
        );
        let response =
            self.control_request("get.content", reqwest::Method::GET, &url, None, &[])?;
        bounded(response, MAX_CONTROL_BYTES).map_err(|_| {
            crate::control::ControlDiagnostic {
                operation: "get.content",
                status: None,
                code: "response-read-or-size",
            }
            .into()
        })
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
        self.control_request("put", reqwest::Method::PUT, &url, Some(bytes.to_vec()), &[])?;
        Ok(())
    }
    fn delete(&self, bucket: &str, name: &str) -> anyhow::Result<()> {
        validate_component(name)?;
        let folder = self.control_bucket(bucket)?;
        let url = format!("{GRAPH}/me/drive/items/{}:/{name}", segment(&folder));
        let response =
            self.control_request("delete.lookup", reqwest::Method::GET, &url, None, &[404])?;
        if response.status().as_u16() == 404 {
            return Ok(());
        }
        let item: Item = self.control_json(response, "delete.lookup.decode", MAX_CONTROL_BYTES)?;
        #[derive(Deserialize)]
        struct Drive {
            id: String,
        }
        let response = self.control_request(
            "delete.drive",
            reqwest::Method::GET,
            &format!("{GRAPH}/me/drive?$select=id"),
            None,
            &[],
        )?;
        let drive: Drive = self.control_json(response, "delete.drive.decode", MAX_CONTROL_BYTES)?;
        if !self
            .control_permanent_delete_unavailable
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            match self.control_request(
                "delete.permanent",
                reqwest::Method::POST,
                &permanent_delete_url(&drive.id, &item.id),
                None,
                &[404],
            ) {
                Ok(_) => return Ok(()),
                Err(error) if permanent_delete_unavailable(&error) => {
                    self.control_permanent_delete_unavailable
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    eprintln!(
                        "control delete.permanent unsupported (HTTP 400 API not found); using recycle-bin deletion for peer control objects only"
                    );
                }
                Err(error) => return Err(error),
            }
        }
        let url = format!(
            "{GRAPH}/drives/{}/items/{}",
            segment(&drive.id),
            segment(&item.id)
        );
        self.control_request(
            "delete.recycle",
            reqwest::Method::DELETE,
            &url,
            None,
            &[404],
        )?;
        Ok(())
    }
}
fn permanent_delete_unavailable(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<crate::control::ControlDiagnostic>()
        .is_some_and(|d| {
            d.operation == "delete.permanent" && d.status == Some(400) && d.code == "apiNotFound"
        })
}
fn control_body(
    mut request: reqwest::blocking::RequestBuilder,
    method: &reqwest::Method,
    body: Option<&[u8]>,
) -> reqwest::blocking::RequestBuilder {
    if let Some(body) = body {
        request = request
            .header(
                "Content-Type",
                if *method == reqwest::Method::POST {
                    "application/json"
                } else {
                    "application/octet-stream"
                },
            )
            .body(body.to_vec());
    }
    // Graph's front door rejects an empty POST without an explicit length (411).
    if *method == reqwest::Method::POST && body.is_none() {
        request = request.header(reqwest::header::CONTENT_LENGTH, 0);
    }
    request
}
fn safe_graph_code(bytes: &[u8]) -> &'static str {
    let value: serde_json::Value = serde_json::from_slice(bytes).unwrap_or_default();
    // Do not echo arbitrary error.code strings: an attacker could put secrets there.
    match value.pointer("/error/code").and_then(|v| v.as_str()) {
        Some("accessDenied") => "accessDenied",
        Some("InvalidAuthenticationToken") => "InvalidAuthenticationToken",
        Some("invalidRequest")
            if value.pointer("/error/message").and_then(|v| v.as_str())
                == Some("API not found") =>
        {
            "apiNotFound"
        }
        Some("invalidRequest") => "invalidRequest",
        Some("itemNotFound") => "itemNotFound",
        Some("notSupported") => "notSupported",
        Some("nameAlreadyExists") => "nameAlreadyExists",
        Some("activityLimitReached") => "activityLimitReached",
        Some("quotaLimitReached") => "quotaLimitReached",
        Some("generalException") => "generalException",
        _ => "unrecognized-or-omitted",
    }
}
fn permanent_delete_url(drive: &str, item: &str) -> String {
    format!(
        "{GRAPH}/drives/{}/items/{}/permanentDelete",
        segment(drive),
        segment(item)
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_explicit_api_not_found_allows_recycle_fallback() {
        use crate::control::ControlDiagnostic;
        let body = br#"{"error":{"code":"invalidRequest","message":"API not found"}}"#;
        assert_eq!(safe_graph_code(body), "apiNotFound");
        for (op, status, code, allowed) in [
            ("delete.permanent", 400, "apiNotFound", true),
            ("delete.permanent", 400, "invalidRequest", false),
            ("delete.permanent", 403, "apiNotFound", false),
            ("delete.permanent", 401, "InvalidAuthenticationToken", false),
            ("delete.permanent", 429, "activityLimitReached", false),
            ("delete.permanent", 500, "generalException", false),
            ("put", 400, "apiNotFound", false),
        ] {
            assert_eq!(
                permanent_delete_unavailable(
                    &ControlDiagnostic {
                        operation: op,
                        status: Some(status),
                        code
                    }
                    .into()
                ),
                allowed
            );
        }
    }
    #[test]
    fn empty_permanent_delete_post_has_explicit_zero_content_length() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}/permanentDelete", server.server_addr());
        let thread = std::thread::spawn(move || {
            let request = server
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()
                .unwrap();
            let length = request
                .headers()
                .iter()
                .find(|h| h.field.equiv("Content-Length"))
                .map(|h| h.value.as_str().to_string());
            request
                .respond(tiny_http::Response::empty(
                    if length.as_deref() == Some("0") {
                        204
                    } else {
                        411
                    },
                ))
                .unwrap();
            length
        });
        let response = control_body(
            reqwest::blocking::Client::new().post(url),
            &reqwest::Method::POST,
            None,
        )
        .send()
        .unwrap();
        let length = thread.join().unwrap();
        assert_eq!(length.as_deref(), Some("0"));
        assert_eq!(response.status().as_u16(), 204);
    }
    #[test]
    fn diagnostic_never_echoes_untrusted_error_fields() {
        let body = br#"{"error":{"code":"secret-token-and-url","message":"private response","innerError":{"token":"secret"}}}"#;
        assert_eq!(safe_graph_code(body), "unrecognized-or-omitted");
        assert_eq!(
            safe_graph_code(br#"{"error":{"code":"accessDenied","message":"secret"}}"#),
            "accessDenied"
        );
        let error = anyhow::anyhow!("Bearer secret https://private/url");
        assert!(!crate::control::safe_diagnostic(&error).contains("secret"));
    }
    #[test]
    fn permanent_delete_uses_documented_drive_route_with_encoded_ids() {
        assert_eq!(
            permanent_delete_url("drive", "item"),
            "https://graph.microsoft.com/v1.0/drives/drive/items/item/permanentDelete"
        );
        assert!(!permanent_delete_url("drive/elsewhere", "item?query=x").contains("item?query"));
    }
    #[test]
    fn pagination_accepts_equivalent_onedrive_id_encoding() {
        assert!(
            validate_next(
                "https://graph.microsoft.com/v1.0/me/drive/items/ABC!123/children?$skiptoken=x",
                "/v1.0/me/drive/items/ABC%21123/children"
            )
            .is_ok()
        );
    }
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
