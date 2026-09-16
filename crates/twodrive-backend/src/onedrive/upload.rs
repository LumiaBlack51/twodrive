use super::auth::random_string;
use super::http::retry_after_delay;
use super::model::GraphDriveItem;
use super::paths::{encode_graph_path, percent_encode_path_segment};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{CONTENT_RANGE, IF_MATCH};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread::sleep;
use std::time::Duration;
use twodrive_core::MetadataEntry;

pub(super) const SIMPLE_UPLOAD_MAX: u64 = 10 * 1024 * 1024;
pub(super) const UPLOAD_FRAGMENT_SIZE: usize = 10 * 1024 * 1024;
const _: () = assert!(UPLOAD_FRAGMENT_SIZE.is_multiple_of(320 * 1024));
const _: () = assert!(UPLOAD_FRAGMENT_SIZE < 60 * 1024 * 1024);
#[derive(Debug, Deserialize)]
pub(super) struct GraphUploadSession {
    #[serde(rename = "uploadUrl")]
    pub(super) upload_url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct UploadSessionStore {
    pub(super) sessions: HashMap<String, PersistedUploadSession>,
}

pub(super) fn shared_upload_session_store(
    path: &Path,
) -> anyhow::Result<Arc<Mutex<UploadSessionStore>>> {
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
    pub(super) fn load(path: &Path) -> anyhow::Result<Self> {
        match fs::read_to_string(path) {
            Ok(data) => Ok(serde_json::from_str(&data)?),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err.into()),
        }
    }

    pub(super) fn save(&self, path: &Path) -> anyhow::Result<()> {
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
            if let Some(parent) = path.parent() {
                fs::File::open(parent)?.sync_all()?;
            }
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
pub(super) struct PersistedUploadSession {
    pub(super) upload_url: String,
    pub(super) source_size: u64,
    pub(super) source_modified_unix: i64,
    pub(super) remote_id: Option<String>,
    pub(super) if_match: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GraphUploadStatus {
    #[serde(rename = "nextExpectedRanges", default)]
    pub(super) next_expected_ranges: Vec<String>,
}

pub(super) fn upload_request(
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

pub(super) fn simple_upload_url(path: &str, remote_id: Option<&str>) -> String {
    match remote_id {
        Some(id) => format!(
            "https://graph.microsoft.com/v1.0/me/drive/items/{}/content",
            encode_graph_path(id)
        ),
        None => format!(
            "https://graph.microsoft.com/v1.0/me/drive/root:/{}:/content",
            encode_graph_path(path)
        ),
    }
}

pub(super) fn uses_upload_session(size: u64) -> bool {
    size > SIMPLE_UPLOAD_MAX
}

pub(super) fn upload_session_request_body() -> serde_json::Value {
    serde_json::json!({
        "item": {
            "@microsoft.graph.conflictBehavior": "replace"
        }
    })
}

pub(super) fn upload_session_create_url(
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

pub(super) fn upload_session_file(
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

pub(super) fn query_upload_offset(
    client: &Client,
    upload_url: &str,
) -> anyhow::Result<Option<u64>> {
    let response = client
        .get(upload_url)
        .send()
        .map_err(reqwest::Error::without_url)?;
    // Only an expired/missing session permits starting over. Offline and server errors
    // must preserve the durable session and its already uploaded fragments.
    if matches!(response.status().as_u16(), 404 | 410) {
        return Ok(None);
    }
    if !response.status().is_success() {
        anyhow::bail!(
            "Graph upload status request failed with HTTP {}",
            response.status()
        );
    }
    let status: GraphUploadStatus = response.json().map_err(reqwest::Error::without_url)?;
    Ok(first_expected_offset(&status.next_expected_ranges))
}

pub(super) fn first_expected_offset(ranges: &[String]) -> Option<u64> {
    ranges
        .iter()
        .filter_map(|range| range.split('-').next())
        .find_map(|start| start.trim().parse::<u64>().ok())
}
