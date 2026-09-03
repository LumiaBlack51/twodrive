use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::{Rng, distributions::Alphanumeric};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{CONTENT_RANGE, IF_MATCH, RANGE, RETRY_AFTER};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
#[cfg(test)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread::sleep;
use std::time::{Duration, Instant};
use time::OffsetDateTime;
use tiny_http::{Response, Server};
use twodrive_core::{
    AppPaths, Config, MetadataEntry, TokenData, TokenStore, join_cloud_path, normalize_cloud_path,
    now_unix,
};
use url::Url;

const SIMPLE_UPLOAD_MAX: u64 = 10 * 1024 * 1024;
const UPLOAD_FRAGMENT_SIZE: usize = 10 * 1024 * 1024;
const _: () = assert!(UPLOAD_FRAGMENT_SIZE.is_multiple_of(320 * 1024));
const _: () = assert!(UPLOAD_FRAGMENT_SIZE < 60 * 1024 * 1024);

pub trait CloudBackend: Send + Sync + 'static {
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>>;
    fn get_metadata(&self, remote_id: &str) -> anyhow::Result<Option<MetadataEntry>> {
        Ok(self
            .list_all()?
            .into_iter()
            .find(|entry| entry.remote_id == remote_id))
    }
    fn list_delta(&self, _delta_link: Option<&str>) -> anyhow::Result<DeltaResult> {
        Ok(DeltaResult {
            entries: self.list_all()?,
            deleted_remote_ids: Vec::new(),
            delta_link: None,
        })
    }
    fn download(&self, remote_id: &str) -> anyhow::Result<Vec<u8>>;
    fn download_to(
        &self,
        remote_id: &str,
        writer: &mut dyn Write,
        on_progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        let content = self.download(remote_id)?;
        let bytes_done = content.len() as u64;
        writer.write_all(&content)?;
        on_progress(bytes_done)?;
        Ok(bytes_done)
    }
    fn download_sized_to(
        &self,
        remote_id: &str,
        _size: u64,
        writer: &mut dyn Write,
        on_progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        self.download_to(remote_id, writer, on_progress)
    }
    fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry>;
    fn upload_with_etag(
        &self,
        path: &str,
        content: Vec<u8>,
        if_match: Option<&str>,
    ) -> anyhow::Result<MetadataEntry> {
        let _ = if_match;
        self.upload(path, content)
    }
    fn upload_file_with_version(
        &self,
        path: &str,
        source_path: &Path,
        remote_id: Option<&str>,
        if_match: Option<&str>,
        on_progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        let _ = remote_id;
        let content = fs::read(source_path)?;
        let size = content.len() as u64;
        let uploaded = self.upload_with_etag(path, content, if_match)?;
        on_progress(size, size)?;
        Ok(uploaded)
    }
    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry>;
    fn rename(&self, remote_id: &str, new_path: &str) -> anyhow::Result<MetadataEntry>;
    fn delete(&self, remote_id: &str) -> anyhow::Result<()>;
}

#[derive(Debug, Clone, Default)]
pub struct DeltaResult {
    pub entries: Vec<MetadataEntry>,
    pub deleted_remote_ids: Vec<String>,
    pub delta_link: Option<String>,
}

#[derive(Debug)]
pub struct MockBackend {
    entries: Mutex<Vec<MetadataEntry>>,
    files: Mutex<HashMap<String, Vec<u8>>>,
}

impl MockBackend {
    pub fn new() -> Self {
        let now = 1_719_000_000;
        let samples = vec![
            (
                "file-readme",
                "/README-cloud.txt",
                b"Welcome to the mock OneDrive tree.\nThis file is downloaded only when opened through FUSE.\n".to_vec(),
            ),
            (
                "file-lecture-01",
                "/Courses/Econometrics/Lecture_01.txt",
                b"Econometrics Lecture 01\n\n1. What is an estimator?\n2. Bias and variance\n3. Ordinary least squares\n".to_vec(),
            ),
            (
                "file-syllabus",
                "/Courses/Econometrics/Syllabus.md",
                b"# Econometrics Syllabus\n\n- Week 1: Linear models\n- Week 2: Inference\n- Week 3: Panel data\n".to_vec(),
            ),
            (
                "file-notes",
                "/Documents/twodrive-notes.txt",
                b"twodrive read-only MVP notes:\n- metadata first\n- hydrate on open\n- cache second reads\n".to_vec(),
            ),
        ];

        let mut files = HashMap::new();
        let mut entries = vec![
            MetadataEntry::new_dir("dir-courses", "/Courses", now, "etag-dir-courses"),
            MetadataEntry::new_dir(
                "dir-econometrics",
                "/Courses/Econometrics",
                now,
                "etag-dir-econometrics",
            ),
            MetadataEntry::new_dir("dir-documents", "/Documents", now, "etag-dir-documents"),
        ];

        for (remote_id, path, content) in samples {
            entries.push(MetadataEntry::new_file(
                remote_id,
                path,
                content.len() as u64,
                now,
                format!("etag-{remote_id}"),
            ));
            files.insert(remote_id.to_string(), content);
        }

        Self {
            entries: Mutex::new(entries),
            files: Mutex::new(files),
        }
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CloudBackend for MockBackend {
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        Ok(self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("mock entries lock is poisoned"))?
            .clone())
    }

    fn download(&self, remote_id: &str) -> anyhow::Result<Vec<u8>> {
        self.files
            .lock()
            .map_err(|_| anyhow::anyhow!("mock files lock is poisoned"))?
            .get(remote_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("mock backend has no file with remote id {remote_id}"))
    }

    fn upload(&self, path: &str, content: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        self.upload_with_etag(path, content, None)
    }

    fn upload_with_etag(
        &self,
        path: &str,
        content: Vec<u8>,
        if_match: Option<&str>,
    ) -> anyhow::Result<MetadataEntry> {
        let path = normalize_cloud_path(path);
        let remote_id = format!("mock-upload-{}", sanitize_remote_component(&path));
        if let Some(if_match) = if_match {
            let entries = self
                .entries
                .lock()
                .map_err(|_| anyhow::anyhow!("mock entries lock is poisoned"))?;
            if let Some(existing) = entries.iter().find(|entry| entry.path == path)
                && existing.etag != if_match
            {
                anyhow::bail!("Graph request failed with HTTP 412 Precondition Failed");
            }
        }
        let entry = MetadataEntry::new_file(
            remote_id.clone(),
            path.clone(),
            content.len() as u64,
            now_unix(),
            format!("etag-{remote_id}"),
        );

        self.files
            .lock()
            .map_err(|_| anyhow::anyhow!("mock files lock is poisoned"))?
            .insert(remote_id, content);
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("mock entries lock is poisoned"))?;
        entries.retain(|existing| existing.path != path);
        entries.push(entry.clone());
        Ok(entry)
    }

    fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
        let path = normalize_cloud_path(path);
        let remote_id = format!("mock-folder-{}", sanitize_remote_component(&path));
        let entry = MetadataEntry::new_dir(
            remote_id,
            path.clone(),
            now_unix(),
            format!("etag-{}", sanitize_remote_component(&path)),
        );
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("mock entries lock is poisoned"))?;
        entries.retain(|existing| existing.path != path);
        entries.push(entry.clone());
        Ok(entry)
    }

    fn rename(&self, remote_id: &str, new_path: &str) -> anyhow::Result<MetadataEntry> {
        let new_path = normalize_cloud_path(new_path);
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("mock entries lock is poisoned"))?;
        let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry.remote_id == remote_id)
        else {
            anyhow::bail!("mock backend has no item with remote id {remote_id}");
        };
        let updated = if entry.is_dir {
            MetadataEntry::new_dir(
                entry.remote_id.clone(),
                new_path,
                now_unix(),
                format!("etag-{}", entry.remote_id),
            )
        } else {
            MetadataEntry::new_file(
                entry.remote_id.clone(),
                new_path,
                entry.size,
                now_unix(),
                format!("etag-{}", entry.remote_id),
            )
        };
        *entry = updated.clone();
        Ok(updated)
    }

    fn delete(&self, remote_id: &str) -> anyhow::Result<()> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("mock entries lock is poisoned"))?;
        let before = entries.len();
        entries.retain(|entry| entry.remote_id != remote_id);
        if entries.len() == before {
            anyhow::bail!("mock backend has no item with remote id {remote_id}");
        }
        self.files
            .lock()
            .map_err(|_| anyhow::anyhow!("mock files lock is poisoned"))?
            .remove(remote_id);
        Ok(())
    }
}

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

    pub fn login(paths: &AppPaths) -> anyhow::Result<()> {
        let config = Config::load_or_create(paths)?;
        config.validate_graph_login()?;
        let token_store = TokenStore::new(paths.token_path.clone());
        let client = Client::builder()
            .user_agent("twodrive/0.1")
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()?;

        let verifier = random_string(64);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = random_string(32);
        let redirect = Url::parse(&config.graph.redirect_uri)?;
        let host = redirect.host_str().unwrap_or("127.0.0.1");
        let port = redirect
            .port_or_known_default()
            .ok_or_else(|| anyhow::anyhow!("redirect_uri must include a port"))?;
        let server = Server::http(format!("{host}:{port}"))
            .map_err(|err| anyhow::anyhow!("failed to listen for OAuth callback: {err}"))?;

        let mut auth_url = Url::parse(&format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/authorize",
            config.graph.tenant
        ))?;
        auth_url
            .query_pairs_mut()
            .append_pair("client_id", &config.graph.client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &config.graph.redirect_uri)
            .append_pair("response_mode", "query")
            .append_pair("scope", &config.graph.scopes.join(" "))
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state);

        println!("Open this URL to sign in:\n{auth_url}\n");
        let _ = Command::new("xdg-open").arg(auth_url.as_str()).spawn();
        println!(
            "Waiting for Microsoft OAuth callback on {} ...",
            config.graph.redirect_uri
        );

        let request = server.recv()?;
        let callback_url = Url::parse(&format!("http://{host}:{port}{}", request.url()))?;
        let params = callback_url
            .query_pairs()
            .into_owned()
            .collect::<HashMap<_, _>>();

        let response = if params.get("state") == Some(&state) && params.contains_key("code") {
            Response::from_string("twodrive login complete. You can close this tab.")
        } else {
            Response::from_string("twodrive login failed. Return to the terminal.")
                .with_status_code(400)
        };
        let _ = request.respond(response);

        if let Some(error) = params.get("error") {
            anyhow::bail!(
                "OAuth authorization failed: {}",
                params
                    .get("error_description")
                    .map(String::as_str)
                    .unwrap_or(error)
            );
        }

        if params.get("state") != Some(&state) {
            anyhow::bail!("OAuth state mismatch");
        }
        let code = params
            .get("code")
            .ok_or_else(|| anyhow::anyhow!("OAuth callback did not include code"))?;
        let token = exchange_code(&client, &config, code, &verifier)?;
        token_store.save(&token)?;
        println!("twodrive login succeeded");
        Ok(())
    }

    fn access_token(&self) -> anyhow::Result<String> {
        let mut token = self
            .token
            .lock()
            .map_err(|_| anyhow::anyhow!("token lock is poisoned"))?;
        if token.expires_at_unix > now_unix() + 60 {
            return Ok(token.access_token.clone());
        }

        let refresh_token_value = token.refresh_token.clone().ok_or_else(|| {
            anyhow::anyhow!("access token expired and no refresh token is stored")
        })?;
        let refreshed = refresh_token(&self.client, &self.config, &refresh_token_value)?;
        self.token_store.save(&refreshed)?;
        *token = refreshed;
        Ok(token.access_token.clone())
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
    ) -> anyhow::Result<reqwest::blocking::Response> {
        let access_token = self.access_token()?;
        let range = format!("bytes={start}-{end}");
        retry_request(|| {
            self.client
                .get(url)
                .bearer_auth(&access_token)
                .header(RANGE, range.clone())
        })
    }
}

impl CloudBackend for GraphBackend {
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        Ok(self.list_delta(None)?.entries)
    }

    fn get_metadata(&self, remote_id: &str) -> anyhow::Result<Option<MetadataEntry>> {
        let url = format!("https://graph.microsoft.com/v1.0/me/drive/items/{remote_id}");
        let item: GraphDriveItem = self.get_with_retry(&url)?.json()?;
        Ok(item.into_metadata())
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
            let mut response = self.get_range_with_retry(&url, start, end)?;

            loop {
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
        let source_metadata = fs::metadata(source_path)?;
        let size = source_metadata.len();
        if !uses_upload_session(size) {
            let uploaded = self.upload_with_etag(path, fs::read(source_path)?, if_match)?;
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
        let resumed = persisted.and_then(|session| {
            query_upload_offset(&self.client, &session.upload_url)
                .ok()
                .flatten()
                .filter(|offset| *offset < size)
                .map(|offset| (session.upload_url, offset))
        });
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
        let access_token = self.access_token()?;
        let url = format!("https://graph.microsoft.com/v1.0/me/drive/items/{remote_id}");
        retry_request(|| self.client.delete(&url).bearer_auth(&access_token))?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct GraphDeltaResponse {
    value: Vec<GraphDriveItem>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
    #[serde(rename = "@odata.deltaLink")]
    delta_link: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphUploadSession {
    #[serde(rename = "uploadUrl")]
    upload_url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct UploadSessionStore {
    sessions: HashMap<String, PersistedUploadSession>,
}

fn shared_upload_session_store(path: &Path) -> anyhow::Result<Arc<Mutex<UploadSessionStore>>> {
    static STORES: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<UploadSessionStore>>>>> =
        OnceLock::new();
    let stores = STORES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut stores = stores
        .lock()
        .map_err(|_| anyhow::anyhow!("upload session registry lock is poisoned"))?;
    if let Some(store) = stores.get(path).and_then(Weak::upgrade) {
        return Ok(store);
    }
    let store = Arc::new(Mutex::new(UploadSessionStore::load(path)?));
    stores.insert(path.to_path_buf(), Arc::downgrade(&store));
    Ok(store)
}

impl UploadSessionStore {
    fn load(path: &Path) -> anyhow::Result<Self> {
        match fs::read_to_string(path) {
            Ok(data) => Ok(serde_json::from_str(&data)?),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err.into()),
        }
    }

    fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp_path = path.with_extension(format!(
            "json.tmp-{}-{}",
            std::process::id(),
            random_string(8)
        ));
        let result = (|| -> anyhow::Result<()> {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&tmp_path)?;
            file.write_all(&serde_json::to_vec_pretty(self)?)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp_path, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp_path);
        }
        result?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedUploadSession {
    upload_url: String,
    source_size: u64,
    source_modified_unix: i64,
    remote_id: Option<String>,
    if_match: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphUploadStatus {
    #[serde(rename = "nextExpectedRanges", default)]
    next_expected_ranges: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct GraphDriveItem {
    id: String,
    name: Option<String>,
    size: Option<u64>,
    #[serde(rename = "lastModifiedDateTime")]
    last_modified: Option<String>,
    #[serde(rename = "eTag")]
    etag: Option<String>,
    folder: Option<serde_json::Value>,
    deleted: Option<serde_json::Value>,
    #[serde(rename = "parentReference")]
    parent_reference: Option<GraphParentReference>,
}

#[derive(Debug, Deserialize)]
struct GraphParentReference {
    path: Option<String>,
}

impl GraphDriveItem {
    fn into_metadata(self) -> Option<MetadataEntry> {
        let name = self.name?;
        if name.is_empty() {
            return None;
        }
        let parent_path = self
            .parent_reference
            .and_then(|parent| parent.path)
            .and_then(|path| path.strip_prefix("/drive/root:").map(str::to_string))
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| "/".to_string());
        let path = join_cloud_path(&parent_path, &name);
        let modified_unix = self
            .last_modified
            .as_deref()
            .and_then(|value| {
                OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
            })
            .map(|value| value.unix_timestamp())
            .unwrap_or_else(now_unix);
        let etag = self.etag.unwrap_or_default();

        if self.folder.is_some() {
            Some(MetadataEntry::new_dir(self.id, path, modified_unix, etag))
        } else {
            Some(MetadataEntry::new_file(
                self.id,
                path,
                self.size.unwrap_or(0),
                modified_unix,
                etag,
            ))
        }
    }

    fn into_metadata_at_path(self, path: &str) -> Option<MetadataEntry> {
        let path = normalize_cloud_path(path);
        let modified_unix = self
            .last_modified
            .as_deref()
            .and_then(|value| {
                OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
            })
            .map(|value| value.unix_timestamp())
            .unwrap_or_else(now_unix);
        let etag = self.etag.unwrap_or_default();

        if self.folder.is_some() {
            Some(MetadataEntry::new_dir(self.id, path, modified_unix, etag))
        } else {
            Some(MetadataEntry::new_file(
                self.id,
                path,
                self.size.unwrap_or(0),
                modified_unix,
                etag,
            ))
        }
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

#[derive(Debug, Serialize)]
struct CodeTokenRequest<'a> {
    client_id: &'a str,
    scope: String,
    code: &'a str,
    redirect_uri: &'a str,
    grant_type: &'a str,
    code_verifier: &'a str,
}

#[derive(Debug, Serialize)]
struct RefreshTokenRequest<'a> {
    client_id: &'a str,
    scope: String,
    refresh_token: &'a str,
    grant_type: &'a str,
}

fn exchange_code(
    client: &Client,
    config: &Config,
    code: &str,
    verifier: &str,
) -> anyhow::Result<TokenData> {
    let request = CodeTokenRequest {
        client_id: &config.graph.client_id,
        scope: config.graph.scopes.join(" "),
        code,
        redirect_uri: &config.graph.redirect_uri,
        grant_type: "authorization_code",
        code_verifier: verifier,
    };
    token_request(
        client.post(token_url(config)).form(&request),
        "OAuth token exchange",
    )
}

fn refresh_token(
    client: &Client,
    config: &Config,
    refresh_token: &str,
) -> anyhow::Result<TokenData> {
    let request = RefreshTokenRequest {
        client_id: &config.graph.client_id,
        scope: config.graph.scopes.join(" "),
        refresh_token,
        grant_type: "refresh_token",
    };
    token_request(
        client.post(token_url(config)).form(&request),
        "OAuth refresh",
    )
}

fn token_request(builder: RequestBuilder, label: &str) -> anyhow::Result<TokenData> {
    let response = retry_request(|| builder.try_clone().expect("request can be cloned"))?;
    let status = response.status();
    let text = response.text()?;
    if !status.is_success() {
        anyhow::bail!("{label} failed with HTTP {status}: {text}");
    }
    let token: TokenResponse = serde_json::from_str(&text)?;
    Ok(TokenData {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at_unix: now_unix() + token.expires_in.unwrap_or(3600),
    })
}

fn upload_request(
    client: &Client,
    url: &str,
    access_token: &str,
    if_match: Option<&str>,
    content: &[u8],
) -> RequestBuilder {
    let builder = client
        .put(url)
        .bearer_auth(access_token)
        .header("Content-Type", "application/octet-stream")
        .body(content.to_vec());
    if let Some(etag) = if_match.filter(|etag| !etag.trim().is_empty()) {
        builder.header(IF_MATCH, etag.to_string())
    } else {
        builder
    }
}

fn uses_upload_session(size: u64) -> bool {
    size > SIMPLE_UPLOAD_MAX
}

fn upload_session_request_body() -> serde_json::Value {
    serde_json::json!({
        "item": {
            "@microsoft.graph.conflictBehavior": "replace"
        }
    })
}

fn upload_session_create_url(
    remote_id: Option<&str>,
    parent_id: Option<&str>,
    name: &str,
) -> anyhow::Result<String> {
    if let Some(remote_id) = remote_id.filter(|remote_id| !remote_id.trim().is_empty()) {
        return Ok(format!(
            "https://graph.microsoft.com/v1.0/me/drive/items/{remote_id}/createUploadSession"
        ));
    }
    let parent_id = parent_id
        .filter(|parent_id| !parent_id.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("new upload session requires a parent item id"))?;
    if name.is_empty() {
        anyhow::bail!("new upload session requires a file name");
    }
    Ok(format!(
        "https://graph.microsoft.com/v1.0/me/drive/items/{parent_id}:/{}:/createUploadSession",
        percent_encode_path_segment(name)
    ))
}

fn upload_session_file(
    client: &Client,
    upload_url: &str,
    cloud_path: &str,
    source_path: &Path,
    total_size: u64,
    initial_offset: u64,
    on_progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
) -> anyhow::Result<MetadataEntry> {
    let mut file = fs::File::open(source_path)?;
    let mut offset = initial_offset.min(total_size);
    if offset > 0 {
        on_progress(offset, total_size)?;
    }
    let mut stalled_responses = 0_u8;

    while offset < total_size {
        file.seek(SeekFrom::Start(offset))?;
        let chunk_len = (total_size - offset).min(UPLOAD_FRAGMENT_SIZE as u64) as usize;
        let mut chunk = vec![0_u8; chunk_len];
        file.read_exact(&mut chunk)?;
        let end = offset + chunk_len as u64 - 1;
        let content_range = format!("bytes {offset}-{end}/{total_size}");
        let mut next_offset = None;
        let mut last_error = None;

        for attempt in 1..=3_u64 {
            match client
                .put(upload_url)
                .header(CONTENT_RANGE, content_range.clone())
                .header("Content-Type", "application/octet-stream")
                .body(chunk.clone())
                .send()
            {
                Ok(response)
                    if response.status() == reqwest::StatusCode::OK
                        || response.status() == reqwest::StatusCode::CREATED =>
                {
                    let item: GraphDriveItem = response.json()?;
                    on_progress(total_size, total_size)?;
                    return item.into_metadata_at_path(cloud_path).ok_or_else(|| {
                        anyhow::anyhow!(
                            "Graph upload session response did not include file metadata"
                        )
                    });
                }
                Ok(response) if response.status() == reqwest::StatusCode::ACCEPTED => {
                    let status: GraphUploadStatus = response.json()?;
                    next_offset = first_expected_offset(&status.next_expected_ranges);
                    break;
                }
                Ok(response) if response.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE => {
                    next_offset = query_upload_offset(client, upload_url)?;
                    break;
                }
                Ok(response)
                    if response.status().as_u16() == 429 || response.status().is_server_error() =>
                {
                    let status = response.status();
                    let delay = retry_after_delay(response.headers(), attempt);
                    let body = response.text().unwrap_or_default();
                    last_error = Some(anyhow::anyhow!("HTTP {status}: {body}"));
                    if let Ok(Some(server_offset)) = query_upload_offset(client, upload_url)
                        && server_offset > offset
                    {
                        next_offset = Some(server_offset);
                        break;
                    }
                    sleep(delay);
                }
                Ok(response) => {
                    let status = response.status();
                    let body = response.text().unwrap_or_default();
                    anyhow::bail!("Graph upload fragment failed with HTTP {status}: {body}");
                }
                Err(err) => {
                    last_error = Some(err.into());
                    if let Ok(Some(server_offset)) = query_upload_offset(client, upload_url)
                        && server_offset > offset
                    {
                        next_offset = Some(server_offset);
                        break;
                    }
                    sleep(Duration::from_millis(250 * attempt));
                }
            }
        }

        let next_offset = next_offset.ok_or_else(|| {
            last_error.unwrap_or_else(|| anyhow::anyhow!("Graph upload fragment failed"))
        })?;
        if next_offset <= offset {
            stalled_responses += 1;
            if stalled_responses >= 3 {
                anyhow::bail!("Graph upload session did not advance after three attempts");
            }
        } else {
            stalled_responses = 0;
            offset = next_offset.min(total_size);
            on_progress(offset, total_size)?;
        }
    }

    anyhow::bail!("Graph upload session ended without final file metadata")
}

fn query_upload_offset(client: &Client, upload_url: &str) -> anyhow::Result<Option<u64>> {
    let response = client.get(upload_url).send()?;
    if !response.status().is_success() {
        anyhow::bail!(
            "Graph upload status request failed with HTTP {}",
            response.status()
        );
    }
    let status: GraphUploadStatus = response.json()?;
    Ok(first_expected_offset(&status.next_expected_ranges))
}

fn first_expected_offset(ranges: &[String]) -> Option<u64> {
    ranges
        .iter()
        .filter_map(|range| range.split('-').next())
        .find_map(|start| start.trim().parse::<u64>().ok())
}

fn retry_request<F>(mut build: F) -> anyhow::Result<reqwest::blocking::Response>
where
    F: FnMut() -> RequestBuilder,
{
    let mut last_error = None;
    for attempt in 1..=3 {
        match build().send() {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response)
                if response.status().as_u16() == 429 || response.status().is_server_error() =>
            {
                let status = response.status();
                let delay = retry_after_delay(response.headers(), attempt);
                let body = response.text().unwrap_or_default();
                last_error = Some(anyhow::anyhow!("HTTP {status}: {body}"));
                eprintln!("twodrive: Graph request attempt {attempt} failed; retrying");
                sleep(delay);
                continue;
            }
            Ok(response) => {
                let status = response.status();
                let body = response.text().unwrap_or_default();
                anyhow::bail!("Graph request failed with HTTP {status}: {body}");
            }
            Err(err) => last_error = Some(err.into()),
        }

        sleep(Duration::from_millis(250 * attempt));
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Graph request failed")))
}

fn retry_after_delay(headers: &reqwest::header::HeaderMap, attempt: u64) -> Duration {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after_seconds)
        .unwrap_or_else(|| Duration::from_millis(250 * attempt))
}

fn parse_retry_after_seconds(value: &str) -> Option<Duration> {
    let seconds = value.trim().parse::<u64>().ok()?;
    Some(Duration::from_secs(seconds.clamp(1, 30)))
}

fn token_url(config: &Config) -> String {
    format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
        config.graph.tenant
    )
}

fn encode_graph_path(path: &str) -> String {
    normalize_cloud_path(path)
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(percent_encode_path_segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn split_cloud_parent_name(path: &str) -> anyhow::Result<(String, String)> {
    let path = normalize_cloud_path(path);
    if path == "/" {
        anyhow::bail!("root path does not have a parent/name");
    }
    let (parent, name) = match path.rfind('/') {
        Some(0) => ("/".to_string(), path[1..].to_string()),
        Some(index) => (path[..index].to_string(), path[index + 1..].to_string()),
        None => ("/".to_string(), path),
    };
    if name.is_empty() {
        anyhow::bail!("path does not include a name");
    }
    Ok((parent, name))
}

fn graph_parent_lookup_url(parent_path: &str) -> String {
    let parent_path = normalize_cloud_path(parent_path);
    if parent_path == "/" {
        "https://graph.microsoft.com/v1.0/me/drive/root".to_string()
    } else {
        format!(
            "https://graph.microsoft.com/v1.0/me/drive/root:/{}",
            encode_graph_path(&parent_path)
        )
    }
}

fn percent_encode_path_segment(segment: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::new();
    for byte in segment.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(*byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

fn sanitize_remote_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn random_string(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use tiny_http::{Header, StatusCode};

    #[test]
    fn graph_move_resolves_the_parent_item_instead_of_sending_a_path() {
        assert_eq!(
            graph_parent_lookup_url("/"),
            "https://graph.microsoft.com/v1.0/me/drive/root"
        );
        assert_eq!(
            graph_parent_lookup_url("/Course Work/Week #1"),
            "https://graph.microsoft.com/v1.0/me/drive/root:/Course%20Work/Week%20%231"
        );
    }

    #[test]
    fn graph_paths_encode_each_segment_without_encoding_separators() {
        assert_eq!(
            encode_graph_path("/资料/notes & tasks.txt"),
            "%E8%B5%84%E6%96%99/notes%20%26%20tasks.txt"
        );
    }

    #[test]
    fn mock_upload_with_matching_etag_replaces_existing_file() {
        let backend = MockBackend::new();
        let uploaded = backend
            .upload_with_etag(
                "/README-cloud.txt",
                b"new content".to_vec(),
                Some("etag-file-readme"),
            )
            .unwrap();

        assert_eq!(uploaded.path, "/README-cloud.txt");
        assert_eq!(
            backend.download(&uploaded.remote_id).unwrap(),
            b"new content".to_vec()
        );
    }

    #[test]
    fn mock_upload_with_stale_etag_fails_precondition() {
        let backend = MockBackend::new();
        let error = backend
            .upload_with_etag(
                "/README-cloud.txt",
                b"stale content".to_vec(),
                Some("etag-from-an-old-generation"),
            )
            .unwrap_err()
            .to_string();

        assert!(error.contains("HTTP 412"));
    }

    #[test]
    fn retry_after_seconds_are_honored_with_a_small_cap() {
        assert_eq!(parse_retry_after_seconds("2"), Some(Duration::from_secs(2)));
        assert_eq!(
            parse_retry_after_seconds("120"),
            Some(Duration::from_secs(30))
        );
        assert_eq!(parse_retry_after_seconds("not-a-number"), None);
    }

    #[test]
    fn large_files_use_graph_upload_sessions_with_valid_fragment_sizes() {
        assert!(!uses_upload_session(10 * 1024 * 1024));
        assert!(uses_upload_session(10 * 1024 * 1024 + 1));
    }

    #[test]
    fn new_upload_session_uses_parent_item_id_and_encoded_file_name() {
        assert_eq!(
            upload_session_create_url(None, Some("parent-id"), "JFLAP notes #1.jar").unwrap(),
            "https://graph.microsoft.com/v1.0/me/drive/items/parent-id:/JFLAP%20notes%20%231.jar:/createUploadSession"
        );
        assert!(upload_session_create_url(None, None, "large.bin").is_err());
    }

    #[test]
    fn existing_upload_session_uses_remote_item_id() {
        assert_eq!(
            upload_session_create_url(Some("remote-id"), None, "ignored.bin").unwrap(),
            "https://graph.microsoft.com/v1.0/me/drive/items/remote-id/createUploadSession"
        );
    }

    #[test]
    fn upload_session_body_uses_graph_compatible_minimal_properties() {
        assert_eq!(
            upload_session_request_body(),
            serde_json::json!({
                "item": {
                    "@microsoft.graph.conflictBehavior": "replace"
                }
            })
        );
    }

    #[test]
    fn upload_session_sends_sequential_ranges_and_finishes_with_metadata() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let upload_url = format!("http://{}/upload", server.server_addr());
        let (range_tx, range_rx) = mpsc::channel();
        let server_thread = thread::spawn(move || {
            for index in 0..2 {
                let mut request = server.recv().unwrap();
                let range = request
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("Content-Range"))
                    .map(|header| header.value.as_str().to_string())
                    .unwrap();
                std::io::copy(request.as_reader(), &mut std::io::sink()).unwrap();
                range_tx.send(range).unwrap();

                let response = if index == 0 {
                    Response::from_string(format!(
                        r#"{{"nextExpectedRanges":["{}-"]}}"#,
                        UPLOAD_FRAGMENT_SIZE
                    ))
                    .with_status_code(StatusCode(202))
                } else {
                    Response::from_string(format!(
                        r#"{{"id":"large-id","name":"large.bin","size":{},"eTag":"large-etag"}}"#,
                        UPLOAD_FRAGMENT_SIZE + 3
                    ))
                    .with_status_code(StatusCode(201))
                }
                .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                request.respond(response).unwrap();
            }
        });

        let source_path = std::env::temp_dir().join(format!(
            "twodrive-upload-session-{}-{}.bin",
            std::process::id(),
            now_unix()
        ));
        let source = fs::File::create(&source_path).unwrap();
        source.set_len(UPLOAD_FRAGMENT_SIZE as u64 + 3).unwrap();
        drop(source);
        let mut progress = Vec::new();
        let uploaded = upload_session_file(
            &Client::new(),
            &upload_url,
            "/large.bin",
            &source_path,
            UPLOAD_FRAGMENT_SIZE as u64 + 3,
            0,
            &mut |done, total| {
                progress.push((done, total));
                Ok(())
            },
        )
        .unwrap();

        server_thread.join().unwrap();
        fs::remove_file(source_path).unwrap();
        assert_eq!(
            range_rx.into_iter().collect::<Vec<_>>(),
            vec![
                format!(
                    "bytes 0-{}/{}",
                    UPLOAD_FRAGMENT_SIZE - 1,
                    UPLOAD_FRAGMENT_SIZE + 3
                ),
                format!(
                    "bytes {}-{}/{}",
                    UPLOAD_FRAGMENT_SIZE,
                    UPLOAD_FRAGMENT_SIZE + 2,
                    UPLOAD_FRAGMENT_SIZE + 3
                ),
            ]
        );
        assert_eq!(uploaded.remote_id, "large-id");
        assert_eq!(uploaded.path, "/large.bin");
        assert_eq!(
            progress.last(),
            Some(&(
                UPLOAD_FRAGMENT_SIZE as u64 + 3,
                UPLOAD_FRAGMENT_SIZE as u64 + 3
            ))
        );
    }

    #[test]
    fn upload_session_resumes_from_a_persisted_offset() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let upload_url = format!("http://{}/upload", server.server_addr());
        let (range_tx, range_rx) = mpsc::channel();
        let server_thread = thread::spawn(move || {
            let mut request = server.recv().unwrap();
            let range = request
                .headers()
                .iter()
                .find(|header| header.field.equiv("Content-Range"))
                .map(|header| header.value.as_str().to_string())
                .unwrap();
            std::io::copy(request.as_reader(), &mut std::io::sink()).unwrap();
            range_tx.send(range).unwrap();
            request
                .respond(
                    Response::from_string(format!(
                        r#"{{"id":"resumed-id","name":"large.bin","size":{},"eTag":"etag"}}"#,
                        UPLOAD_FRAGMENT_SIZE + 3
                    ))
                    .with_status_code(StatusCode(201))
                    .with_header(Header::from_bytes("Content-Type", "application/json").unwrap()),
                )
                .unwrap();
        });

        let source_path = std::env::temp_dir().join(format!(
            "twodrive-upload-resume-{}-{}.bin",
            std::process::id(),
            now_unix()
        ));
        let source = fs::File::create(&source_path).unwrap();
        source.set_len(UPLOAD_FRAGMENT_SIZE as u64 + 3).unwrap();
        drop(source);
        let uploaded = upload_session_file(
            &Client::new(),
            &upload_url,
            "/large.bin",
            &source_path,
            UPLOAD_FRAGMENT_SIZE as u64 + 3,
            UPLOAD_FRAGMENT_SIZE as u64,
            &mut |_, _| Ok(()),
        )
        .unwrap();

        server_thread.join().unwrap();
        fs::remove_file(source_path).unwrap();
        assert_eq!(
            range_rx.recv().unwrap(),
            format!(
                "bytes {}-{}/{}",
                UPLOAD_FRAGMENT_SIZE,
                UPLOAD_FRAGMENT_SIZE + 2,
                UPLOAD_FRAGMENT_SIZE + 3
            )
        );
        assert_eq!(uploaded.remote_id, "resumed-id");
    }

    #[test]
    fn upload_session_store_round_trips_with_private_permissions() {
        let root = std::env::temp_dir().join(format!(
            "twodrive-upload-store-{}-{}",
            std::process::id(),
            now_unix()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("upload-sessions.json");
        let mut store = UploadSessionStore::default();
        store.sessions.insert(
            "/large.bin".to_string(),
            PersistedUploadSession {
                upload_url: "https://upload.example/session".to_string(),
                source_size: 42,
                source_modified_unix: 7,
                remote_id: Some("remote-id".to_string()),
                if_match: Some("etag".to_string()),
            },
        );
        store.save(&path).unwrap();

        let loaded = UploadSessionStore::load(&path).unwrap();
        let session = loaded.sessions.get("/large.bin").unwrap();
        assert_eq!(session.source_size, 42);
        assert_eq!(session.remote_id.as_deref(), Some("remote-id"));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn upload_session_store_is_shared_for_the_same_data_path() {
        let root = std::env::temp_dir().join(format!(
            "twodrive-shared-upload-store-{}-{}",
            std::process::id(),
            now_unix()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("upload-sessions.json");

        let first = shared_upload_session_store(&path).unwrap();
        let second = shared_upload_session_store(&path).unwrap();

        assert!(Arc::ptr_eq(&first, &second));
        drop(first);
        drop(second);
        fs::remove_dir_all(root).unwrap();
    }
}
