use crate::{CloudBackend, DeltaResult};
use http::{retry_optional_request, retry_request, retry_request_checked};
use model::{GraphDeltaResponse, GraphDriveItem};
use paths::{
    encode_graph_path, graph_parent_lookup_url, split_cloud_parent_name, validate_graph_file_path,
};
use reqwest::blocking::Client;
use reqwest::header::{IF_MATCH, RANGE};
use std::fs::{self};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use twodrive_core::{AppPaths, Config, MetadataEntry, TokenData, TokenStore, normalize_cloud_path};
use upload::{
    GraphUploadSession, PersistedUploadSession, UploadSessionStore, query_upload_offset,
    shared_upload_session_store, simple_upload_url, upload_request, upload_session_create_url,
    upload_session_file, upload_session_request_body, uses_upload_session,
};
mod auth;
mod http;
mod model;
mod paths;
mod upload;

#[derive(Debug)]
pub struct GraphBackend {
    config: Config,
    token_store: TokenStore,
    token: Mutex<TokenData>,
    client: Client,
    upload_sessions_path: PathBuf,
    upload_sessions: Arc<Mutex<UploadSessionStore>>,
}

impl GraphBackend {
    pub fn from_paths(paths: &AppPaths) -> anyhow::Result<Self> {
        let config = Config::load_or_create(paths)?;
        config.validate_graph_login()?;
        let token_store = TokenStore::new(paths.token_path.clone());
        let token = token_store
            .load()?
            .ok_or_else(|| anyhow::anyhow!("not logged in; run twodrive login first"))?;
        let upload_sessions_path = paths.data_dir.join("upload-sessions.json");
        let upload_sessions = shared_upload_session_store(&upload_sessions_path)?;

        Ok(Self {
            config,
            token_store,
            token: Mutex::new(token),
            client: Client::builder()
                .user_agent("twodrive/0.1")
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::limited(10))
                .build()?,
            upload_sessions_path,
            upload_sessions,
        })
    }

    fn matching_upload_session(
        &self,
        key: &str,
        source_size: u64,
        source_modified_unix: i64,
        remote_id: Option<&str>,
        if_match: Option<&str>,
    ) -> anyhow::Result<Option<PersistedUploadSession>> {
        let sessions = self
            .upload_sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("upload session store lock is poisoned"))?;
        Ok(sessions
            .sessions
            .get(key)
            .filter(|session| {
                session.source_size == source_size
                    && session.source_modified_unix == source_modified_unix
                    && session.remote_id.as_deref() == remote_id
                    && session.if_match.as_deref() == if_match
            })
            .cloned())
    }

    fn save_upload_session(
        &self,
        key: String,
        session: PersistedUploadSession,
    ) -> anyhow::Result<()> {
        let mut sessions = self
            .upload_sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("upload session store lock is poisoned"))?;
        sessions.sessions.insert(key, session);
        sessions.save(&self.upload_sessions_path)
    }

    fn remove_upload_session(&self, key: &str) -> anyhow::Result<()> {
        let mut sessions = self
            .upload_sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("upload session store lock is poisoned"))?;
        if sessions.sessions.remove(key).is_some() {
            sessions.save(&self.upload_sessions_path)?;
        }
        Ok(())
    }

    fn get_with_retry(&self, url: &str) -> anyhow::Result<reqwest::blocking::Response> {
        let access_token = self.access_token()?;
        retry_request(|| self.client.get(url).bearer_auth(&access_token))
    }

    fn get_range_with_retry(
        &self,
        url: &str,
        start: u64,
        end: u64,
        check: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<reqwest::blocking::Response> {
        check()?;
        let access_token = self.access_token()?;
        let range = format!("bytes={start}-{end}");
        retry_request_checked(
            || {
                self.client
                    .get(url)
                    .bearer_auth(&access_token)
                    .header(RANGE, range.clone())
            },
            check,
        )
    }
}

impl CloudBackend for GraphBackend {
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        Ok(self.list_delta(None)?.entries)
    }

    fn get_metadata(&self, remote_id: &str) -> anyhow::Result<Option<MetadataEntry>> {
        let url = format!("https://graph.microsoft.com/v1.0/me/drive/items/{remote_id}");
        let Some(response) = retry_optional_request(|| {
            let access_token = self.access_token()?;
            Ok(self.client.get(&url).bearer_auth(access_token))
        })?
        else {
            return Ok(None);
        };
        let item: GraphDriveItem = response.json()?;
        Ok(item.into_metadata())
    }

    fn get_metadata_by_path(&self, path: &str) -> anyhow::Result<Option<MetadataEntry>> {
        let path = normalize_cloud_path(path);
        let url = format!(
            "https://graph.microsoft.com/v1.0/me/drive/root:/{}",
            encode_graph_path(&path)
        );
        let Some(response) = retry_optional_request(|| {
            let access_token = self.access_token()?;
            Ok(self.client.get(&url).bearer_auth(access_token))
        })?
        else {
            return Ok(None);
        };
        let item: GraphDriveItem = response.json()?;
        Ok(item.into_metadata_at_path(&path))
    }

    fn list_delta(&self, delta_link: Option<&str>) -> anyhow::Result<DeltaResult> {
        let mut url = delta_link
            .map(str::to_string)
            .unwrap_or_else(|| "https://graph.microsoft.com/v1.0/me/drive/root/delta".to_string());
        let mut result = DeltaResult::default();
        let started = Instant::now();
        let mut page = 0_u64;

        loop {
            page += 1;
            let page_started = Instant::now();
            eprintln!("twodrive: requesting OneDrive delta metadata page={page}");
            let body: GraphDeltaResponse = self.get_with_retry(&url)?.json()?;
            let item_count = body.value.len();
            for item in body.value {
                if item.deleted.is_some() {
                    result.deleted_remote_ids.push(item.id);
                    continue;
                }

                let Some(entry) = item.into_metadata() else {
                    continue;
                };
                result.entries.push(entry);
            }
            eprintln!(
                "twodrive: received OneDrive delta page={page} items={item_count} elapsed_ms={}",
                page_started.elapsed().as_millis()
            );

            if let Some(next) = body.next_link {
                url = next;
                continue;
            }
            result.delta_link = body.delta_link;
            break;
        }
        eprintln!(
            "twodrive: completed OneDrive delta pages={page} entries={} deleted={} elapsed_ms={}",
            result.entries.len(),
            result.deleted_remote_ids.len(),
            started.elapsed().as_millis()
        );

        Ok(result)
    }

    fn download(&self, remote_id: &str) -> anyhow::Result<Vec<u8>> {
        let url = format!("https://graph.microsoft.com/v1.0/me/drive/items/{remote_id}/content");
        let bytes = self.get_with_retry(&url)?.bytes()?;
        Ok(bytes.to_vec())
    }

    fn download_to(
        &self,
        remote_id: &str,
        writer: &mut dyn Write,
        on_progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        let url = format!("https://graph.microsoft.com/v1.0/me/drive/items/{remote_id}/content");
        let mut response = self.get_with_retry(&url)?;
        let mut buffer = [0_u8; 128 * 1024];
        let mut bytes_done = 0_u64;

        loop {
            let read = response.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            writer.write_all(&buffer[..read])?;
            bytes_done += read as u64;
            on_progress(bytes_done)?;
        }

        Ok(bytes_done)
    }

    fn download_sized_to(
        &self,
        remote_id: &str,
        size: u64,
        writer: &mut dyn Write,
        on_progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        if size == 0 {
            return Ok(0);
        }

        const RANGE_CHUNK: u64 = 4 * 1024 * 1024;
        let url = format!("https://graph.microsoft.com/v1.0/me/drive/items/{remote_id}/content");
        let mut buffer = [0_u8; 128 * 1024];
        let mut bytes_done = 0_u64;

        while bytes_done < size {
            let start = bytes_done;
            let end = (start + RANGE_CHUNK - 1).min(size - 1);
            let mut response =
                self.get_range_with_retry(&url, start, end, &mut || on_progress(bytes_done))?;

            loop {
                on_progress(bytes_done)?;
                let read = response.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                writer.write_all(&buffer[..read])?;
                bytes_done += read as u64;
                on_progress(bytes_done)?;
                if bytes_done >= size {
                    break;
                }
            }
        }

        Ok(bytes_done)
    }

    fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        validate_graph_file_path(path)?;
        let access_token = self.access_token()?;
        let url = format!(
            "https://graph.microsoft.com/v1.0/me/drive/root:/{}:/content",
            encode_graph_path(path)
        );
        let item: GraphDriveItem =
            retry_request(|| upload_request(&self.client, &url, &access_token, None, &content))?
                .json()?;
        item.into_metadata_at_path(path)
            .ok_or_else(|| anyhow::anyhow!("Graph upload response did not include file metadata"))
    }

    fn upload_file_with_version(
        &self,
        path: &str,
        source_path: &Path,
        remote_id: Option<&str>,
        if_match: Option<&str>,
        on_progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        validate_graph_file_path(path)?;
        let source_metadata = fs::metadata(source_path)?;
        let size = source_metadata.len();
        if !uses_upload_session(size) {
            let access_token = self.access_token()?;
            let url = simple_upload_url(path, remote_id);
            let content = fs::read(source_path)?;
            let item: GraphDriveItem = retry_request(|| {
                upload_request(&self.client, &url, &access_token, if_match, &content)
            })?
            .json()?;
            let uploaded = item.into_metadata_at_path(path).ok_or_else(|| {
                anyhow::anyhow!("Graph upload response did not include file metadata")
            })?;
            on_progress(size, size)?;
            return Ok(uploaded);
        }

        let source_modified_unix = source_metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|value| value.as_secs() as i64)
            .unwrap_or_default();
        let session_key = normalize_cloud_path(path);
        let persisted = self.matching_upload_session(
            &session_key,
            size,
            source_modified_unix,
            remote_id,
            if_match,
        )?;
        let resumed = match persisted {
            Some(session) => query_upload_offset(&self.client, &session.upload_url)?
                .filter(|offset| *offset < size)
                .map(|offset| (session.upload_url, offset)),
            None => None,
        };
        let (upload_url, initial_offset) = if let Some(resumed) = resumed {
            resumed
        } else {
            let _ = self.remove_upload_session(&session_key);
            let access_token = self.access_token()?;
            let (parent_path, name) = split_cloud_parent_name(path)?;
            let parent_id = if remote_id.is_none() {
                let parent_url = graph_parent_lookup_url(&parent_path);
                let parent: GraphDriveItem = self.get_with_retry(&parent_url)?.json()?;
                Some(parent.id)
            } else {
                None
            };
            let create_url = upload_session_create_url(remote_id, parent_id.as_deref(), &name)?;
            let body = upload_session_request_body();
            let session: GraphUploadSession = retry_request(|| {
                let builder = self
                    .client
                    .post(&create_url)
                    .bearer_auth(&access_token)
                    .json(&body);
                if let Some(etag) = if_match.filter(|etag| !etag.trim().is_empty()) {
                    builder.header(IF_MATCH, etag.to_string())
                } else {
                    builder
                }
            })?
            .json()?;
            self.save_upload_session(
                session_key.clone(),
                PersistedUploadSession {
                    upload_url: session.upload_url.clone(),
                    source_size: size,
                    source_modified_unix,
                    remote_id: remote_id.map(str::to_string),
                    if_match: if_match.map(str::to_string),
                },
            )?;
            (session.upload_url, 0)
        };

        let result = upload_session_file(
            &self.client,
            &upload_url,
            path,
            source_path,
            size,
            initial_offset,
            on_progress,
        );
        if result.is_ok() {
            let _ = self.remove_upload_session(&session_key);
        }
        result
    }

    fn upload_with_etag(
        &self,
        path: &str,
        content: Vec<u8>,
        if_match: Option<&str>,
    ) -> anyhow::Result<MetadataEntry> {
        validate_graph_file_path(path)?;
        let access_token = self.access_token()?;
        let url = format!(
            "https://graph.microsoft.com/v1.0/me/drive/root:/{}:/content",
            encode_graph_path(path)
        );
        let item: GraphDriveItem = retry_request(|| {
            upload_request(&self.client, &url, &access_token, if_match, &content)
        })?
        .json()?;
        item.into_metadata_at_path(path)
            .ok_or_else(|| anyhow::anyhow!("Graph upload response did not include file metadata"))
    }

    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        let access_token = self.access_token()?;
        let (parent_path, name) = split_cloud_parent_name(path)?;
        let url = if parent_path == "/" {
            "https://graph.microsoft.com/v1.0/me/drive/root/children".to_string()
        } else {
            format!(
                "https://graph.microsoft.com/v1.0/me/drive/root:/{}:/children",
                encode_graph_path(&parent_path)
            )
        };
        let body = serde_json::json!({
            "name": name,
            "folder": {},
            "@microsoft.graph.conflictBehavior": "fail"
        });
        let item: GraphDriveItem = retry_request(|| {
            self.client
                .post(&url)
                .bearer_auth(&access_token)
                .json(&body)
        })?
        .json()?;
        item.into_metadata_at_path(path)
            .ok_or_else(|| anyhow::anyhow!("Graph create folder response did not include metadata"))
    }

    fn rename(&self, remote_id: &str, new_path: &str) -> anyhow::Result<MetadataEntry> {
        let access_token = self.access_token()?;
        let (parent_path, name) = split_cloud_parent_name(new_path)?;
        let parent_url = graph_parent_lookup_url(&parent_path);
        let parent: GraphDriveItem = self.get_with_retry(&parent_url)?.json()?;
        let body = serde_json::json!({
            "name": name,
            "parentReference": {
                "id": parent.id
            }
        });
        let url = format!("https://graph.microsoft.com/v1.0/me/drive/items/{remote_id}");
        let item: GraphDriveItem = retry_request(|| {
            self.client
                .patch(&url)
                .bearer_auth(&access_token)
                .json(&body)
        })?
        .json()?;
        item.into_metadata_at_path(new_path)
            .ok_or_else(|| anyhow::anyhow!("Graph rename response did not include metadata"))
    }

    fn delete(&self, remote_id: &str) -> anyhow::Result<()> {
        let url = format!("https://graph.microsoft.com/v1.0/me/drive/items/{remote_id}");
        retry_optional_request(|| {
            let access_token = self.access_token()?;
            Ok(self.client.delete(&url).bearer_auth(access_token))
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;

// Historical classification is deliberately unchanged, including wrapped errors.
pub(crate) fn is_conflict_error(err: &anyhow::Error) -> bool {
    let text = err.to_string();
    text.contains("HTTP 412") || text.contains("Precondition Failed")
}
