use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
pub const MAX_FRAME: usize = 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub id: String,
    pub command: Command,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    RefreshIndex,
    CancelRefresh,
    IndexPage {
        offset: usize,
    },
    DownloadCloud {
        id: String,
    },
    CancelDownload {
        id: String,
    },
    OpenCached {
        id: String,
        reveal: bool,
    },
    Snapshot,
    Login,
    CancelLogin,
    Logout,
    Browse {
        query_id: String,
        drive_id: Option<String>,
        item_id: Option<String>,
    },
    BrowseNext {
        query_id: String,
    },
    CancelBrowse {
        query_id: String,
    },
    SetPaused {
        paused: bool,
    },
    Download {
        id: String,
    },
    Release {
        id: String,
    },
    MockImport {
        name: String,
        content: String,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    pub version: u32,
    pub id: String,
    pub ok: bool,
    pub error: Option<String>,
    pub snapshot: Snapshot,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub state: String,
    pub size: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transfer {
    pub id: String,
    pub name: String,
    pub direction: String,
    pub done: u64,
    pub total: u64,
    pub outcome: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub cloud: Option<CloudView>,
    #[serde(default)]
    pub directory: Option<DirectoryView>,
    pub auth_status: String,
    pub auth_error: Option<String>,
    pub engine_version: String,
    pub engine_pid: u32,
    pub revision: u64,
    pub mode: String,
    pub status: String,
    pub paused: bool,
    pub queued: usize,
    pub active: Option<Transfer>,
    pub recent: Vec<Transfer>,
    pub files: Vec<Item>,
    pub file_count: usize,
    pub capabilities: Vec<String>,
    pub unsupported: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudView {
    pub file_count: usize,
    pub cached_bytes: u64,
    pub status: String,
    pub error: Option<String>,
    pub offset: usize,
    pub count: usize,
    pub items: Vec<CloudFile>,
    pub tasks: Vec<twodrive_core::DownloadTask>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudFile {
    #[serde(flatten)]
    pub item: twodrive_core::IndexedItem,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryView {
    pub page_number: usize,
    pub query_id: String,
    pub status: String,
    pub error: Option<String>,
    pub page: Option<twodrive_backend::onedrive::browse::DirectoryPage>,
    pub fetched_at: Option<i64>,
}
