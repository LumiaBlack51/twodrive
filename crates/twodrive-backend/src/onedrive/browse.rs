//! Read-only, bounded Graph metadata queries. Continuations never leave Graph.
use super::GraphBackend;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudItem {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub size: Option<u64>,
    pub modified: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryPage {
    pub account_id: String,
    pub drive_id: String,
    pub item_id: String,
    pub items: Vec<CloudItem>,
    pub has_more: bool,
    #[serde(skip)]
    pub next_link: Option<String>,
}

pub(super) fn trusted_graph(url: &str) -> anyhow::Result<Url> {
    let u = Url::parse(url).map_err(|_| anyhow::anyhow!("untrusted_graph_target"))?;
    anyhow::ensure!(
        u.scheme() == "https"
            && u.host_str() == Some("graph.microsoft.com")
            && u.port_or_known_default() == Some(443)
            && u.username().is_empty()
            && u.password().is_none()
            && u.fragment().is_none()
            && u.path().starts_with("/v1.0/"),
        "untrusted_graph_target"
    );
    Ok(u)
}

pub(super) fn endpoint(segments: &[&str]) -> anyhow::Result<String> {
    anyhow::ensure!(
        segments
            .iter()
            .all(|s| !s.is_empty() && *s != "." && *s != ".." && s.len() < 1024),
        "invalid_item_id"
    );
    let mut u = Url::parse("https://graph.microsoft.com/v1.0/")?;
    u.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .extend(segments);
    Ok(u.into())
}

impl GraphBackend {
    pub(super) fn browse_json(
        &self,
        url: &str,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<serde_json::Value> {
        trusted_graph(url)?;
        #[cfg(test)]
        let test_url = self
            .test_endpoint
            .as_ref()
            .map(|base| format!("{base}{}", &url["https://graph.microsoft.com".len()..]));
        #[cfg(test)]
        let url = test_url.as_deref().unwrap_or(url);
        let started = Instant::now();
        let mut refreshed = false;
        let mut attempt = 0;
        loop {
            check()?;
            let cooldown = self
                .browse_retry_until
                .lock()
                .unwrap()
                .map(|until| until.saturating_duration_since(Instant::now()))
                .unwrap_or_default();
            anyhow::ensure!(
                cooldown <= Duration::from_secs(90).saturating_sub(started.elapsed()),
                "rate_limited_retry_later"
            );
            super::http::checked_delay(cooldown, check)?;
            anyhow::ensure!(
                started.elapsed() < Duration::from_secs(90),
                "browse_timeout"
            );
            let token = self.access_token().map_err(|e| self.browse_auth_error(e))?;
            check()?;
            let response = self
                .browse_client
                .get(url)
                .bearer_auth(&token)
                .timeout(Duration::from_secs(20))
                .send();
            check()?;
            let response = match response {
                Ok(r) => r,
                Err(e) => {
                    attempt += 1;
                    anyhow::ensure!(
                        attempt < 3,
                        if e.is_timeout() {
                            "browse_timeout"
                        } else {
                            "network_unavailable"
                        }
                    );
                    super::http::checked_delay(Duration::from_millis(250 << attempt), check)?;
                    continue;
                }
            };
            let status = response.status().as_u16();
            if status == 401 && !refreshed {
                self.refresh_rejected_token(&token)
                    .map_err(|e| self.browse_auth_error(e))?;
                refreshed = true;
                continue;
            }
            if status == 429 || status >= 500 || status == 408 {
                attempt += 1;
                let delay = response
                    .headers()
                    .get("Retry-After")
                    .and_then(|v| v.to_str().ok())
                    .and_then(retry_after)
                    .unwrap_or_else(|| Duration::from_millis(250 << attempt));
                if status == 429 {
                    *self.browse_retry_until.lock().unwrap() = Some(
                        Instant::now()
                            .checked_add(delay)
                            .unwrap_or_else(|| Instant::now() + Duration::from_secs(3153600000)),
                    );
                }
                anyhow::ensure!(
                    attempt < 3
                        && delay <= Duration::from_secs(90).saturating_sub(started.elapsed()),
                    if status == 429 {
                        "rate_limited_retry_later"
                    } else {
                        "service_unavailable"
                    }
                );
                super::http::checked_delay(delay, check)?;
                continue;
            }
            anyhow::ensure!(
                (200..300).contains(&status),
                match status {
                    401 => "reauthentication_required",
                    403 => "permission_denied",
                    404 => "item_not_found",
                    410 => "delta_expired",
                    300..=399 => "redirect_rejected",
                    _ => "graph_request_failed",
                }
            );
            // Limit metadata even if a server disregards $top; never include response text in errors.
            use std::io::Read;
            let mut bytes = Vec::new();
            response
                .take(256 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| anyhow::anyhow!("network_unavailable"))?;
            anyhow::ensure!(bytes.len() <= 256 * 1024, "metadata_page_too_large");
            return serde_json::from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("invalid_graph_metadata"));
        }
    }

    fn browse_auth_error(&self, error: anyhow::Error) -> anyhow::Error {
        if let Some(http) = error.downcast_ref::<super::http::HttpFailure>() {
            if http.status == 429 {
                *self.browse_retry_until.lock().unwrap() = Some(
                    Instant::now()
                        .checked_add(http.retry_after)
                        .unwrap_or_else(|| Instant::now() + Duration::from_secs(3153600000)),
                );
                return anyhow::anyhow!("rate_limited_retry_later");
            }
            if http.status >= 500 {
                return anyhow::anyhow!("service_unavailable");
            }
        }
        if let Some(network) = error.downcast_ref::<reqwest::Error>() {
            return anyhow::anyhow!(if network.is_timeout() {
                "browse_timeout"
            } else {
                "network_unavailable"
            });
        }
        anyhow::anyhow!("reauthentication_required")
    }

    pub fn browse_directory(
        &self,
        drive: Option<&str>,
        item: Option<&str>,
        next: Option<&str>,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<DirectoryPage> {
        let current = self.browse_json(
            "https://graph.microsoft.com/v1.0/me/drive?$select=id,owner",
            check,
        )?;
        let drive_id = required(&current, "id")?;
        anyhow::ensure!(
            drive.is_none_or(|d| d == drive_id),
            "account_drive_mismatch"
        );
        let account_id = current
            .pointer("/owner/user/id")
            .and_then(|v| v.as_str())
            .unwrap_or(&drive_id)
            .to_string();
        let folder_url = match item {
            Some(id) => endpoint(&["drives", &drive_id, "items", id])?,
            None => endpoint(&["drives", &drive_id, "root"])?,
        };
        let folder = self.browse_json(
            &format!("{folder_url}?$select=id,folder,remoteItem,package"),
            check,
        )?;
        anyhow::ensure!(
            folder.get("remoteItem").is_none() && folder.get("package").is_none(),
            "unsupported_shared_or_package_item"
        );
        anyhow::ensure!(folder.get("folder").is_some(), "not_a_folder");
        let item_id = required(&folder, "id")?;
        let base = endpoint(&["drives", &drive_id, "items", &item_id, "children"])?;
        let url = if let Some(next) = next {
            let u = trusted_graph(next)?;
            anyhow::ensure!(
                u.path() == Url::parse(&base)?.path(),
                "invalid_directory_continuation"
            );
            next.to_string()
        } else {
            format!(
                "{base}?$top=100&$select=id,name,size,lastModifiedDateTime,folder,file,remoteItem,package"
            )
        };
        let body = self.browse_json(&url, check)?;
        let next_link = body
            .get("@odata.nextLink")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        if let Some(next) = &next_link {
            let u = trusted_graph(next)?;
            anyhow::ensure!(
                u.path() == Url::parse(&base)?.path() && next != &url,
                "invalid_directory_continuation"
            );
        }
        let items = body
            .get("value")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("invalid_graph_metadata"))?
            .iter()
            .map(|v| {
                let kind = if v.get("remoteItem").is_some() || v.get("package").is_some() {
                    "unsupported"
                } else if v.get("folder").is_some() {
                    "folder"
                } else if v.get("file").is_some() {
                    "file"
                } else {
                    "unsupported"
                };
                Ok(CloudItem {
                    id: required(v, "id")?,
                    name: required(v, "name")?,
                    kind: kind.into(),
                    size: v.get("size").and_then(|s| s.as_u64()),
                    modified: v
                        .get("lastModifiedDateTime")
                        .and_then(|s| s.as_str())
                        .map(str::to_owned),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        check()?;
        Ok(DirectoryPage {
            account_id,
            drive_id,
            item_id,
            items,
            has_more: next_link.is_some(),
            next_link,
        })
    }
}

fn retry_after(value: &str) -> Option<Duration> {
    super::http::parse_retry_after_seconds(value)
}

fn required(v: &serde_json::Value, key: &str) -> anyhow::Result<String> {
    v.get(key)
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("invalid_graph_metadata"))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::sync::Arc;
    use tiny_http::{Header, Response, Server};
    use twodrive_core::{AppPaths, TokenData, TokenStore, now_unix};

    pub(crate) fn backend(server: &Server) -> GraphBackend {
        let root = std::env::temp_dir().join(format!(
            "twodrive-browse-{}-{}",
            std::process::id(),
            super::super::auth::random_string(16)
        ));
        let paths = AppPaths {
            config_dir: root.clone(),
            config_path: root.join("config.toml"),
            data_dir: root.clone(),
            cache_dir: root.join("cache"),
            db_path: root.join("db"),
            mount_dir: root.join("mount"),
            token_path: root.join("tokens.json"),
        };
        TokenStore::new(&paths.token_path)
            .save(&TokenData {
                access_token: "synthetic-access".into(),
                refresh_token: Some("synthetic-refresh".into()),
                expires_at_unix: now_unix() + 3600,
            })
            .unwrap();
        let mut backend = GraphBackend::from_paths(&paths).unwrap();
        backend.test_endpoint = Some(format!("http://{}", server.server_addr()));
        backend
    }

    #[test]
    fn pagination_unicode_empty_and_unsupported_are_metadata_only() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let backend = backend(&server);
        let worker = std::thread::spawn(move || {
            let expected = [
                "/v1.0/me/drive",
                "/v1.0/drives/drive/root",
                "/v1.0/drives/drive/items/root/children",
                "/v1.0/me/drive",
                "/v1.0/drives/drive/items/root",
                "/v1.0/drives/drive/items/root/children",
            ];
            for (i, path) in expected.iter().enumerate() {
                let r = server
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap();
                assert_eq!(r.method().as_str(), "GET");
                assert_eq!(r.url().split('?').next().unwrap(), *path);
                let body = match i % 3 {
                    0 => serde_json::json!({"id":"drive","owner":{"user":{"id":"account"}}}),
                    1 => serde_json::json!({"id":"root","folder":{}}),
                    _ if i == 2 => serde_json::json!({"value":[
                        {"id":"one","name":"中文 # + % 文件.pdf","file":{},"size":42,"lastModifiedDateTime":"2026-09-16T10:00:00Z"},
                        {"id":"two","name":"共享","folder":{},"remoteItem":{}},
                        {"id":"three","name":"目录","folder":{},"size":-2}],
                        "@odata.nextLink":"https://graph.microsoft.com/v1.0/drives/drive/items/root/children?$skiptoken=synthetic"}),
                    _ => serde_json::json!({"value":[]}),
                };
                r.respond(Response::from_string(body.to_string())).unwrap();
            }
        });
        let first = backend
            .browse_directory(None, None, None, &mut || Ok(()))
            .unwrap();
        assert_eq!(first.account_id, "account");
        assert_eq!(first.items[0].name, "中文 # + % 文件.pdf");
        assert_eq!(first.items[0].size, Some(42));
        assert_eq!(first.items[1].kind, "unsupported");
        assert_eq!(first.items[2].size, None);
        assert!(first.has_more);
        assert!(!serde_json::to_string(&first).unwrap().contains("skiptoken"));
        let last = backend
            .browse_directory(
                Some("drive"),
                Some("root"),
                first.next_link.as_deref(),
                &mut || Ok(()),
            )
            .unwrap();
        assert!(last.items.is_empty());
        assert!(!last.has_more);
        worker.join().unwrap();
    }

    #[test]
    fn rejects_untrusted_targets_and_path_injection() {
        for url in [
            "http://graph.microsoft.com/v1.0/me",
            "https://graph.microsoft.com.evil/v1.0/me",
            "https://evil/v1.0/me",
            "https://user@graph.microsoft.com/v1.0/me",
            "https://graph.microsoft.com:444/v1.0/me",
            "https://graph.microsoft.com/beta/me",
            "https://graph.microsoft.com/v1.0/me#fragment",
        ] {
            assert!(trusted_graph(url).is_err());
        }
        assert!(endpoint(&["drives", ".."]).is_err());
        assert!(
            endpoint(&["items", "a/b?x#"])
                .unwrap()
                .contains("a%2Fb%3Fx%23")
        );
    }

    #[test]
    fn permission_and_redirect_errors_are_sanitized() {
        for (status, error) in [
            (403, "permission_denied"),
            (302, "redirect_rejected"),
            (404, "item_not_found"),
        ] {
            let server = Server::http("127.0.0.1:0").unwrap();
            let backend = backend(&server);
            let worker = std::thread::spawn(move || {
                let r = server
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap();
                r.respond(
                    Response::from_string("SENSITIVE RESPONSE")
                        .with_status_code(status)
                        .with_header(
                            Header::from_bytes("Location", "https://evil.example/private").unwrap(),
                        ),
                )
                .unwrap();
                assert!(
                    server
                        .recv_timeout(Duration::from_millis(100))
                        .unwrap()
                        .is_none()
                );
            });
            assert_eq!(
                backend
                    .browse_json("https://graph.microsoft.com/v1.0/me/drive", &mut || Ok(()))
                    .unwrap_err()
                    .to_string(),
                error
            );
            worker.join().unwrap();
        }
    }

    #[test]
    fn throttling_honors_retry_after_and_is_bounded() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let backend = backend(&server);
        let worker = std::thread::spawn(move || {
            let first = server.recv().unwrap();
            let start = Instant::now();
            first
                .respond(
                    Response::empty(429)
                        .with_header(Header::from_bytes("Retry-After", "1").unwrap()),
                )
                .unwrap();
            let second = server.recv().unwrap();
            assert!(start.elapsed() >= Duration::from_secs(1));
            second
                .respond(
                    Response::empty(429)
                        .with_header(Header::from_bytes("Retry-After", "120").unwrap()),
                )
                .unwrap();
        });
        assert_eq!(
            backend
                .browse_json("https://graph.microsoft.com/v1.0/me/drive", &mut || Ok(()))
                .unwrap_err()
                .to_string(),
            "rate_limited_retry_later"
        );
        worker.join().unwrap();
    }

    #[test]
    fn rejected_token_refresh_is_single_flight_and_persisted() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let backend = Arc::new(backend(&server));
        let worker = std::thread::spawn(move || {
            let first = server.recv().unwrap();
            assert_eq!(first.url(), "/v1.0/me/drive");
            first.respond(Response::empty(401)).unwrap();
            let refresh = server.recv().unwrap();
            assert_eq!(refresh.url(), "/token");
            assert_eq!(refresh.method().as_str(), "POST");
            refresh
                .respond(Response::from_string(
                    r#"{"access_token":"synthetic-new","expires_in":3600}"#,
                ))
                .unwrap();
            let second = server.recv().unwrap();
            assert!(
                second
                    .headers()
                    .iter()
                    .any(|h| h.field.equiv("Authorization")
                        && h.value.as_str() == "Bearer synthetic-new")
            );
            second.respond(Response::from_string("{}")).unwrap();
        });
        backend
            .browse_json("https://graph.microsoft.com/v1.0/me/drive", &mut || Ok(()))
            .unwrap();
        worker.join().unwrap();
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let b = backend.clone();
                std::thread::spawn(move || b.refresh_rejected_token("synthetic-access").unwrap())
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let saved = backend.token_store.load().unwrap().unwrap();
        assert_eq!(saved.access_token, "synthetic-new");
        assert_eq!(saved.refresh_token.as_deref(), Some("synthetic-refresh"));
        backend.forget_credentials().unwrap();
        assert!(backend.token_store.load().unwrap().is_none());
        assert!(backend.access_token().is_err());
    }

    #[test]
    fn cancellation_stops_retry_without_a_second_request() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let backend = backend(&server);
        let start = Instant::now();
        let worker = std::thread::spawn(move || {
            server
                .recv()
                .unwrap()
                .respond(
                    Response::empty(429)
                        .with_header(Header::from_bytes("Retry-After", "30").unwrap()),
                )
                .unwrap();
        });
        let result = backend.browse_json("https://graph.microsoft.com/v1.0/me/drive", &mut || {
            anyhow::ensure!(start.elapsed() < Duration::from_millis(150), "cancelled");
            Ok(())
        });
        assert_eq!(result.unwrap_err().to_string(), "cancelled");
        assert!(start.elapsed() < Duration::from_secs(2));
        worker.join().unwrap();
    }

    #[test]
    fn expired_token_refresh_is_coordinated_across_concurrent_queries() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let backend = Arc::new(backend(&server));
        backend.token.lock().unwrap().expires_at_unix = 0;
        let worker = std::thread::spawn(move || {
            let refresh = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            assert_eq!(refresh.url(), "/token");
            std::thread::sleep(Duration::from_millis(50));
            refresh.respond(Response::from_string(r#"{"access_token":"synthetic-rotated","refresh_token":"rotated-refresh","expires_in":3600}"#)).unwrap();
            assert!(
                server
                    .recv_timeout(Duration::from_millis(150))
                    .unwrap()
                    .is_none()
            );
        });
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let b = backend.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    assert_eq!(b.access_token().unwrap(), "synthetic-rotated");
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        worker.join().unwrap();
        assert_eq!(
            backend
                .token_store
                .load()
                .unwrap()
                .unwrap()
                .refresh_token
                .as_deref(),
            Some("rotated-refresh")
        );
    }

    #[test]
    fn a_second_401_requires_login_instead_of_refresh_loop() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let backend = backend(&server);
        let worker = std::thread::spawn(move || {
            server
                .recv()
                .unwrap()
                .respond(Response::empty(401))
                .unwrap();
            let refresh = server.recv().unwrap();
            assert_eq!(refresh.url(), "/token");
            refresh
                .respond(Response::from_string(
                    r#"{"access_token":"synthetic-denied","expires_in":3600}"#,
                ))
                .unwrap();
            server
                .recv()
                .unwrap()
                .respond(Response::empty(401))
                .unwrap();
            assert!(
                server
                    .recv_timeout(Duration::from_millis(150))
                    .unwrap()
                    .is_none()
            );
        });
        assert_eq!(
            backend
                .browse_json("https://graph.microsoft.com/v1.0/me/drive", &mut || Ok(()))
                .unwrap_err()
                .to_string(),
            "reauthentication_required"
        );
        worker.join().unwrap();
    }

    #[test]
    fn refresh_throttling_retains_full_cooldown_across_queries() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let backend = backend(&server);
        backend.token.lock().unwrap().expires_at_unix = 0;
        let worker = std::thread::spawn(move || {
            let refresh = server.recv().unwrap();
            assert_eq!(refresh.url(), "/token");
            refresh
                .respond(
                    Response::from_string("sensitive error body")
                        .with_status_code(429)
                        .with_header(Header::from_bytes("Retry-After", "120").unwrap()),
                )
                .unwrap();
            assert!(
                server
                    .recv_timeout(Duration::from_millis(200))
                    .unwrap()
                    .is_none()
            );
        });
        for _ in 0..2 {
            assert_eq!(
                backend
                    .browse_json("https://graph.microsoft.com/v1.0/me/drive", &mut || Ok(()))
                    .unwrap_err()
                    .to_string(),
                "rate_limited_retry_later"
            );
        }
        assert!(backend.token_store.load().unwrap().is_some());
        worker.join().unwrap();
        assert_eq!(
            retry_after("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(Duration::ZERO)
        );
    }
}
