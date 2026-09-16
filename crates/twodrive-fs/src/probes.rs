use fuser::Request;
use std::ffi::OsStr;
use std::fs::{self};
use std::path::{Path, PathBuf};
use twodrive_core::FileRecord;

use crate::cache_io::has_existing_cache;

pub(crate) fn should_defer_hydration(req: &Request<'_>, record: &FileRecord) -> bool {
    !has_existing_cache(record) && is_thumbnail_or_indexer_request(req)
}

pub(crate) fn is_gio_metadata_probe(req: &Request<'_>, flags: i32) -> bool {
    if flags & libc::O_NOATIME == 0 {
        return false;
    }
    let executable = fs::read_link(format!("/proc/{}/exe", req.pid())).ok();
    executable
        .as_deref()
        .and_then(Path::file_name)
        .is_some_and(|name| is_gio_probe_open(flags, name))
}

pub(crate) fn is_gio_probe_open(flags: i32, executable: &OsStr) -> bool {
    // GLib content-type sniffing uses O_NOATIME and falls back to its filename
    // guess on ENODATA. Its real input streams/copies use ordinary O_RDONLY.
    // Do not block arbitrary tools which legitimately read with O_NOATIME.
    flags & libc::O_ACCMODE == libc::O_RDONLY
        && flags & libc::O_NOATIME != 0
        && matches!(executable.to_str(), Some("gio" | "nautilus"))
}

pub(crate) fn is_thumbnail_or_indexer_request(req: &Request<'_>) -> bool {
    let process = request_process_text(req).to_lowercase();
    if process.is_empty() {
        return false;
    }

    const EXACT_HINTS: &[&str] = &[
        "thumbnail",
        "thumbnailer",
        "tracker-extract",
        "tracker-miner",
        "tracker3",
        "localsearch",
        "evince-thumbnailer",
        "ffmpegthumbnailer",
        "totem-video-thumbnailer",
        "gdk-pixbuf-thumbnailer",
        "gnome-epub-thumbnailer",
    ];
    if EXACT_HINTS.iter().any(|hint| process.contains(hint)) {
        return true;
    }

    (process.contains("soffice") || process.contains("libreoffice"))
        && (process.contains("--headless")
            || process.contains("--convert-to")
            || process.contains("thumbnail"))
}

pub(crate) fn request_process_text(req: &Request<'_>) -> String {
    let pid = req.pid();
    let proc_dir = PathBuf::from(format!("/proc/{pid}"));
    let executable = fs::read_link(proc_dir.join("exe")).unwrap_or_default();
    let comm = fs::read_to_string(proc_dir.join("comm")).unwrap_or_default();
    let cmdline = fs::read(proc_dir.join("cmdline"))
        .map(|bytes| String::from_utf8_lossy(&bytes).replace('\0', " "))
        .unwrap_or_default();
    format!("{} {comm} {cmdline}", executable.display())
}

// Do not log command-line arguments: they may contain credentials or document content.
pub(crate) fn request_process_identity(req: &Request<'_>) -> String {
    let pid = req.pid();
    let proc_dir = PathBuf::from(format!("/proc/{pid}"));
    let executable = fs::read_link(proc_dir.join("exe")).unwrap_or_default();
    let comm = fs::read_to_string(proc_dir.join("comm")).unwrap_or_default();
    format!("pid={pid} exe={executable:?} comm={:?}", comm.trim())
}
