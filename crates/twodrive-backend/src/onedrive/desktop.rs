//! Bounded shallow metadata for the provider desktop; unlike list_all, this only
//! queries the selected folder and keeps opaque cloud IDs separate from paths.
use super::{GraphBackend, model::GraphDriveItem, paths::encode_graph_path};
use std::collections::HashSet;
use twodrive_core::{MetadataEntry, join_cloud_path};
impl GraphBackend {
    pub fn desktop_directory(
        &self,
        path: &str,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<Vec<MetadataEntry>> {
        super::paths::validate_graph_file_path(path)?;
        let base = if path == "/" {
            "https://graph.microsoft.com/v1.0/me/drive/root/children".to_string()
        } else {
            format!(
                "https://graph.microsoft.com/v1.0/me/drive/root:/{}:/children",
                encode_graph_path(path)
            )
        };
        let mut url = format!("{base}?$top=100");
        let mut seen = HashSet::new();
        let mut entries = Vec::new();
        loop {
            anyhow::ensure!(
                seen.insert(url.clone()) && seen.len() <= 10000,
                "invalid directory continuation"
            );
            let value = self.browse_json(&url, check)?;
            for item in value["value"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("invalid directory response"))?
            {
                if item.get("remoteItem").is_some() || item.get("package").is_some() {
                    continue;
                }
                let model: GraphDriveItem = serde_json::from_value(item.clone())
                    .map_err(|_| anyhow::anyhow!("invalid file metadata"))?;
                let name = model
                    .name
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("missing file name"))?;
                anyhow::ensure!(
                    !name.is_empty()
                        && !name.contains(['/', '\\', '\0'])
                        && name != "."
                        && name != "..",
                    "invalid cloud file name"
                );
                let target = join_cloud_path(path, name);
                entries.push(
                    model
                        .into_metadata_at_path(&target)
                        .ok_or_else(|| anyhow::anyhow!("invalid file metadata"))?,
                );
                anyhow::ensure!(entries.len() <= 100000, "directory metadata limit exceeded");
            }
            match value["@odata.nextLink"].as_str() {
                None => break,
                Some(next) => {
                    anyhow::ensure!(
                        super::browse::trusted_graph(next)?.path()
                            == super::browse::trusted_graph(&base)?.path(),
                        "invalid directory continuation"
                    );
                    url = next.into();
                }
            }
        }
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CloudBackend;
    use tiny_http::{Response, Server};
    use twodrive_core::{AppPaths, TokenData, TokenStore, now_unix};
    struct Sandbox(std::path::PathBuf);
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn backend(server: &Server) -> (Sandbox, GraphBackend) {
        let root =
            std::env::temp_dir().join(format!("td-graph-desktop-{:x}", rand::random::<u64>()));
        std::fs::create_dir(&root).unwrap();
        let p = AppPaths {
            config_dir: root.clone(),
            config_path: root.join("config.toml"),
            data_dir: root.clone(),
            cache_dir: root.join("cache"),
            db_path: root.join("db"),
            mount_dir: root.join("mount"),
            token_path: root.join("tokens.json"),
        };
        TokenStore::new(&p.token_path)
            .save(&TokenData {
                access_token: "synthetic-desktop".into(),
                refresh_token: None,
                expires_at_unix: now_unix() + 3600,
            })
            .unwrap();
        let mut graph = GraphBackend::from_paths(&p).unwrap();
        graph.test_endpoint = Some(format!("http://{}", server.server_addr()));
        (Sandbox(root), graph)
    }
    #[test]
    fn mount_metadata_uses_root_resource_and_preserves_child_paths_and_absence() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let (_sandbox, graph) = backend(&server);
        let serving = std::thread::spawn(move || {
            for expected in [
                "/v1.0/me/drive/root",
                "/v1.0/me/drive/root:/Course%20Work/notes%20%231.txt",
                "/v1.0/me/drive/root:/missing.txt",
            ] {
                let request = server
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap()
                    .unwrap();
                let matched = request.method().as_str() == "GET" && request.url() == expected;
                let (status, body) = if !matched {
                    (400, serde_json::json!({"error":{"code":"invalidRequest"}}))
                } else if expected.ends_with("/root") {
                    (
                        200,
                        serde_json::json!({"id":"opaque-root","name":"root","folder":{},"size":-1,"eTag":"root-version"}),
                    )
                } else if expected.ends_with("/missing.txt") {
                    (404, serde_json::json!({"error":{"code":"itemNotFound"}}))
                } else {
                    (
                        200,
                        serde_json::json!({"id":"opaque-child","name":"notes #1.txt","file":{},"size":7,"eTag":"child-version"}),
                    )
                };
                request
                    .respond(Response::from_string(body.to_string()).with_status_code(status))
                    .unwrap();
                if !matched {
                    return false;
                }
            }
            true
        });
        let root = graph.get_metadata_by_path("/").unwrap().unwrap();
        assert!(root.is_dir);
        assert_eq!(root.remote_id, "opaque-root");
        assert_eq!(root.path, "/");
        assert_eq!(root.size, 0);
        let file = graph
            .get_metadata_by_path("/Course Work/notes #1.txt")
            .unwrap()
            .unwrap();
        assert_eq!(file.remote_id, "opaque-child");
        assert_eq!(file.path, "/Course Work/notes #1.txt");
        assert_eq!(file.parent_path, "/Course Work");
        assert_eq!(file.size, 7);
        assert!(
            graph
                .get_metadata_by_path("/missing.txt")
                .unwrap()
                .is_none()
        );
        assert!(serving.join().unwrap());
    }

    #[test]
    fn shallow_directory_preserves_ids_versions_and_bounded_pagination() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let (_sandbox, graph) = backend(&server);
        let worker = std::thread::spawn(move || {
            let q = server.recv().unwrap();
            assert!(q.url().starts_with("/v1.0/me/drive/root/children"));
            q.respond(Response::from_string(r#"{"value":[{"id":"opaque-folder","name":"folder","folder":{},"eTag":"version-a"}],"@odata.nextLink":"https://graph.microsoft.com/v1.0/me/drive/root/children?$skiptoken=two"}"#)).unwrap();
            let q = server.recv().unwrap();
            assert!(q.url().contains("skiptoken=two"));
            q.respond(Response::from_string(
                r#"{"value":[{"id":"opaque-file","name":"file.txt","size":4,"eTag":"version-b"}]}"#,
            ))
            .unwrap();
        });
        let entries = graph.desktop_directory("/", &mut || Ok(())).unwrap();
        worker.join().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].remote_id, "opaque-file");
        assert_eq!(entries[1].etag, "version-b");
        assert_eq!(entries[1].path, "/file.txt");
    }
    #[test]
    fn desktop_create_only_requests_fail_on_name_races_and_updates_require_version() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let (sandbox, graph) = backend(&server);
        let source = sandbox.0.join("source");
        std::fs::write(&source, b"data").unwrap();
        let worker = std::thread::spawn(move || {
            let q = server.recv().unwrap();
            assert!(q.url().contains("conflictBehavior=fail"));
            assert!(
                q.headers()
                    .iter()
                    .any(|h| h.field.equiv("If-None-Match") && h.value.as_str() == "*")
            );
            q.respond(Response::from_string("sanitized conflict").with_status_code(409))
                .unwrap();
            let q = server.recv().unwrap();
            assert!(q.url().contains("/items/opaque-id/content"));
            assert!(
                q.headers()
                    .iter()
                    .any(|h| h.field.equiv("If-Match") && h.value.as_str() == "\"old\"")
            );
            q.respond(Response::from_string(
                r#"{"id":"opaque-id","name":"file","size":4,"eTag":"new"}"#,
            ))
            .unwrap();
        });
        assert!(
            graph
                .upload_desktop_file("/file", &source, None, None, &mut |_, _| Ok(()))
                .is_err()
        );
        assert!(
            graph
                .upload_desktop_file(
                    "/file",
                    &source,
                    Some("opaque-id"),
                    None,
                    &mut |_, _| Ok(())
                )
                .is_err()
        );
        assert!(
            graph
                .upload_desktop_file(
                    "/file",
                    &source,
                    Some("opaque-id"),
                    Some("*"),
                    &mut |_, _| Ok(())
                )
                .is_err()
        );
        let m = graph
            .upload_desktop_file(
                "/file",
                &source,
                Some("opaque-id"),
                Some("\"old\""),
                &mut |_, _| Ok(()),
            )
            .unwrap();
        assert_eq!(m.etag, "new");
        worker.join().unwrap();
        assert_eq!(
            super::super::upload::upload_session_request_body_for(true)["item"]["@microsoft.graph.conflictBehavior"],
            "fail"
        );
        assert_eq!(
            super::super::upload::upload_session_request_body_for(false)["item"]["@microsoft.graph.conflictBehavior"],
            "replace"
        );
    }
}
