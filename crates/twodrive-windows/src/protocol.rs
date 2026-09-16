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
    Snapshot,
    SetPaused { paused: bool },
    Download { id: String },
    Release { id: String },
    MockImport { name: String, content: String },
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
