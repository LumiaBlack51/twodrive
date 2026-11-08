//! Conditional namespace operations against immutable drive/parent/item IDs.
//! Callers retain durable intents and reconcile ambiguous responses by identity.
use super::{
    GraphBackend,
    browse::{endpoint, trusted_graph},
    model::GraphDriveItem,
};
use anyhow::{Context, ensure};
use reqwest::{Method, header::IF_MATCH};
use serde_json::{Value, json};
use twodrive_core::{CloudIdentity, MetadataEntry};

impl GraphBackend {
    fn namespace_request(
        &self,
        method: Method,
        url: &str,
        version: Option<&str>,
        body: Option<&Value>,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<reqwest::blocking::Response> {
        trusted_graph(url)?;
        #[cfg(test)]
        let test_url = self
            .test_endpoint
            .as_ref()
            .map(|base| format!("{base}{}", &url["https://graph.microsoft.com".len()..]));
        #[cfg(test)]
        let url = test_url.as_deref().unwrap_or(url);
        super::http::retry_request_built_checked(
            || {
                let request = self
                    .browse_client
                    .request(method.clone(), url)
                    .bearer_auth(self.access_token()?);
                let request = if let Some(version) = version {
                    request.header(IF_MATCH, version)
                } else {
                    request
                };
                Ok(if let Some(body) = body {
                    request.json(body)
                } else {
                    request
                })
            },
            check,
        )
    }

    /// Bind every mutation to the explicitly verified account and drive. No
    /// path lookup is used to resolve a moving or replaced destination parent.
    fn namespace_authority(
        &self,
        identity: &CloudIdentity,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        ensure!(
            !identity.account.is_empty() && !identity.drive.is_empty(),
            "namespace authority missing"
        );
        ensure!(
            self.readonly_identity(check)? == *identity,
            "namespace account changed"
        );
        self.access_token()?;
        Ok(())
    }

    pub fn create_directory_bound(
        &self,
        identity: &CloudIdentity,
        parent_id: &str,
        target_path: &str,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        super::paths::validate_graph_file_path(target_path)?;
        let (_, name) = super::paths::split_cloud_parent_name(target_path)?;
        self.namespace_authority(identity, check)?;
        let url = endpoint(&["drives", &identity.drive, "items", parent_id, "children"])?;
        let body = json!({"name":name,"folder":{},"@microsoft.graph.conflictBehavior":"fail"});
        let value: Value = self
            .namespace_request(Method::POST, &url, None, Some(&body), check)?
            .json()?;
        let drive_verified = self.verify_namespace_drive(&value, identity, check)?;
        namespace_receipt(value, None, parent_id, target_path, true, drive_verified)
    }

    pub fn move_item_bound(
        &self,
        identity: &CloudIdentity,
        item_id: &str,
        version: &str,
        parent_id: &str,
        target_path: &str,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        ensure!(!version.is_empty(), "conditional move requires a version");
        super::paths::validate_graph_file_path(target_path)?;
        let (_, name) = super::paths::split_cloud_parent_name(target_path)?;
        self.namespace_authority(identity, check)?;
        let url = endpoint(&["drives", &identity.drive, "items", item_id])?;
        let body = json!({"name":name,"parentReference":{"driveId":identity.drive,"id":parent_id},"@microsoft.graph.conflictBehavior":"fail"});
        let value: Value = self
            .namespace_request(Method::PATCH, &url, Some(version), Some(&body), check)?
            .json()?;
        let directory = value.get("folder").is_some();
        let drive_verified = self.verify_namespace_drive(&value, identity, check)?;
        namespace_receipt(
            value,
            Some(item_id),
            parent_id,
            target_path,
            directory,
            drive_verified,
        )
    }

    fn verify_namespace_drive(
        &self,
        value: &Value,
        identity: &CloudIdentity,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<bool> {
        if value
            .pointer("/parentReference/driveId")
            .and_then(Value::as_str)
            == Some(identity.drive.as_str())
        {
            return Ok(true);
        }
        // Resource IDs are case-sensitive. Personal OneDrive has returned a
        // different casing in parentReference than /me/drive. Do not normalize
        // opaque IDs: independently retrieve this exact object inside the bound
        // drive, then compare its object/name/parent before accepting the drive
        // binding. Keep the operation's own ETag: a later readback must not
        // acknowledge a concurrent content version on behalf of this operation.
        let id = value["id"]
            .as_str()
            .context("missing namespace receipt ID")?;
        let url = endpoint(&["drives", &identity.drive, "items", id])?;
        let observed = self.browse_json(&url, check)?;
        let matches = ["id", "name"]
            .iter()
            .all(|key| observed[*key] == value[*key])
            && observed.pointer("/parentReference/id") == value.pointer("/parentReference/id")
            && observed.get("folder").is_some() == value.get("folder").is_some()
            && observed.get("deleted").is_none()
            && observed.get("remoteItem").is_none()
            && observed.get("package").is_none();
        ensure!(
            matches,
            "namespace readback mismatch: id={}, name={}, etag={}, parent={}, kind={}, facets={}",
            observed["id"] == value["id"],
            observed["name"] == value["name"],
            observed["eTag"] == value["eTag"],
            observed.pointer("/parentReference/id") == value.pointer("/parentReference/id"),
            observed.get("folder").is_some() == value.get("folder").is_some(),
            observed.get("deleted").is_none()
                && observed.get("remoteItem").is_none()
                && observed.get("package").is_none()
        );
        Ok(true)
    }

    /// Graph's ordinary delete uses the service recycle bin. 404/412 and lost
    /// responses remain errors for the durable caller to reconcile by item ID.
    pub fn delete_item_bound(
        &self,
        identity: &CloudIdentity,
        item_id: &str,
        version: &str,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        ensure!(!version.is_empty(), "conditional delete requires a version");
        self.namespace_authority(identity, check)?;
        let url = endpoint(&["drives", &identity.drive, "items", item_id])?;
        self.namespace_request(Method::DELETE, &url, Some(version), None, check)?;
        Ok(())
    }
}

fn namespace_receipt(
    value: Value,
    id: Option<&str>,
    parent: &str,
    path: &str,
    directory: bool,
    drive_verified: bool,
) -> anyhow::Result<MetadataEntry> {
    let name = path.rsplit('/').next().context("missing namespace leaf")?;
    ensure!(
        value.get("deleted").is_none()
            && value.get("remoteItem").is_none()
            && value.get("package").is_none()
            && value["id"].as_str().is_some_and(
                |actual| !actual.is_empty() && id.is_none_or(|expected| actual == expected)
            )
            && value["name"].as_str() == Some(name)
            && value.pointer("/parentReference/id").and_then(Value::as_str) == Some(parent)
            && drive_verified
            && value.get("folder").is_some() == directory
            && (directory || value["size"].as_u64().is_some())
            && value["eTag"].as_str().is_some_and(|s| !s.is_empty()),
        "namespace receipt identity mismatch: id={}, name={}, parent={}, drive_bound={}, kind={}, version={}",
        value["id"].as_str().is_some_and(
            |actual| !actual.is_empty() && id.is_none_or(|expected| actual == expected)
        ),
        value["name"].as_str() == Some(name),
        value.pointer("/parentReference/id").and_then(Value::as_str) == Some(parent),
        drive_verified,
        value.get("folder").is_some() == directory,
        value["eTag"].as_str().is_some_and(|s| !s.is_empty())
    );
    serde_json::from_value::<GraphDriveItem>(value)?
        .into_metadata_at_path(path)
        .context("invalid namespace receipt")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_http::{Response, Server};
    fn authority() -> CloudIdentity {
        CloudIdentity {
            account: "owner-a".into(),
            drive: "drive-a".into(),
        }
    }
    fn identity(server: &Server) {
        let request = server.recv().unwrap();
        assert!(request.url().starts_with("/v1.0/me/drive?"));
        request
            .respond(Response::from_string(
                r#"{"id":"drive-a","owner":{"user":{"id":"owner-a"}}}"#,
            ))
            .unwrap();
    }
    #[test]
    fn namespace_uses_opaque_drive_parent_ids_versions_and_refuses_foreign_receipts() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let (_sandbox, graph) = super::super::desktop::tests::backend(&server);
        let worker = std::thread::spawn(move || {
            identity(&server);
            let mut request = server.recv().unwrap();
            assert_eq!(request.method().as_str(), "POST");
            assert_eq!(
                request.url(),
                "/v1.0/drives/drive-a/items/parent-id/children"
            );
            let body: Value = serde_json::from_reader(request.as_reader()).unwrap();
            assert_eq!(body["@microsoft.graph.conflictBehavior"], "fail");
            request.respond(Response::from_string(r#"{"id":"new-dir","name":"新目录","folder":{},"eTag":"v1","parentReference":{"id":"parent-id","driveId":"drive-a"}}"#)).unwrap();
            identity(&server);
            let mut request = server.recv().unwrap();
            assert_eq!(request.method().as_str(), "PATCH");
            assert_eq!(request.url(), "/v1.0/drives/drive-a/items/opaque%2Fid");
            assert!(
                request
                    .headers()
                    .iter()
                    .any(|h| h.field.equiv("If-Match") && h.value.as_str() == "old-tag")
            );
            let body: Value = serde_json::from_reader(request.as_reader()).unwrap();
            assert_eq!(body["parentReference"]["id"], "parent-id");
            request.respond(Response::from_string(r#"{"id":"wrong-object","name":"renamed.txt","size":5,"eTag":"v2","parentReference":{"id":"parent-id","driveId":"drive-a"}}"#)).unwrap();
            identity(&server);
            let request = server.recv().unwrap();
            assert_eq!(request.method().as_str(), "DELETE");
            assert!(
                request
                    .headers()
                    .iter()
                    .any(|h| h.field.equiv("If-Match") && h.value.as_str() == "stale")
            );
            request
                .respond(Response::from_string("stale").with_status_code(412))
                .unwrap();
        });
        assert!(
            graph
                .create_directory_bound(
                    &authority(),
                    "parent-id",
                    "/generated/新目录",
                    &mut || Ok(())
                )
                .unwrap()
                .is_dir
        );
        assert!(
            graph
                .move_item_bound(
                    &authority(),
                    "opaque/id",
                    "old-tag",
                    "parent-id",
                    "/generated/renamed.txt",
                    &mut || Ok(())
                )
                .is_err()
        );
        let error = graph
            .delete_item_bound(&authority(), "opaque/id", "stale", &mut || Ok(()))
            .unwrap_err();
        assert!(crate::onedrive::is_conflict_error(&error));
        worker.join().unwrap();
    }
    #[test]
    fn namespace_account_switch_cannot_reach_a_mutation() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let (_sandbox, graph) = super::super::desktop::tests::backend(&server);
        let worker = std::thread::spawn(move || {
            identity(&server);
            assert!(
                server
                    .recv_timeout(std::time::Duration::from_millis(100))
                    .unwrap()
                    .is_none()
            );
        });
        let wrong = CloudIdentity {
            account: "owner-b".into(),
            drive: "drive-a".into(),
        };
        assert!(
            graph
                .delete_item_bound(&wrong, "item", "tag", &mut || Ok(()))
                .is_err()
        );
        worker.join().unwrap();
    }
    #[test]
    fn parent_drive_variance_requires_independent_exact_drive_readback() {
        for changed_parent in [false, true] {
            let server = Server::http("127.0.0.1:0").unwrap();
            let (_sandbox, graph) = super::super::desktop::tests::backend(&server);
            let worker = std::thread::spawn(move || {
                identity(&server);
                let request = server.recv().unwrap();
                request.respond(Response::from_string(r#"{"id":"new-dir","name":"child","folder":{},"eTag":"v1","parentReference":{"id":"parent-id","driveId":"DRIVE-A"}}"#)).unwrap();
                let request = server.recv().unwrap();
                assert_eq!(request.url(), "/v1.0/drives/drive-a/items/new-dir");
                let parent = if changed_parent {
                    "foreign-parent"
                } else {
                    "parent-id"
                };
                request.respond(Response::from_string(json!({"id":"new-dir","name":"child","folder":{},"eTag":"later-version","parentReference":{"id":parent,"driveId":"DRIVE-A"}}).to_string())).unwrap();
            });
            let result = graph.create_directory_bound(
                &authority(),
                "parent-id",
                "/generated/child",
                &mut || Ok(()),
            );
            assert_eq!(result.is_ok(), !changed_parent);
            if let Ok(receipt) = result {
                assert_eq!(
                    receipt.etag, "v1",
                    "authority readback must not acknowledge a later version"
                );
            }
            worker.join().unwrap();
        }
    }
}
