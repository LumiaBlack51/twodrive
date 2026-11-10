//! SMB2/3 read-only adapter. Each request owns a fresh protocol process/session.
//! IDs are bounded, connection-local observations; no persistent content cache,
//! cross-reconnect versions, mutation, recursive inventory or mount support.
use crate::{
    model::{CloudBackend, DeltaResult, MetadataEntry},
    provider::{Capabilities, Progress, Provider, valid_path},
    state::random_secret,
};
use anyhow::{Context, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct Settings {
    pub server: String,
    pub port: u16,
    pub share: String,
    pub root: String,
    pub username: String,
    pub domain: String,
    pub password: String,
    pub encrypt: bool,
}
impl Settings {
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            !self.server.is_empty() && self.server.len() <= 253 && self.port != 0,
            "SMB server/port invalid"
        );
        ensure!(
            self.server.parse::<std::net::IpAddr>().is_ok()
                || self
                    .server
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b)),
            "SMB server invalid"
        );
        safe_component(&self.share)?;
        safe_path(&self.root)?;
        ensure!(
            !self.username.is_empty() && !self.password.is_empty(),
            "SMB requires explicit username/password"
        );
        for value in [&self.username, &self.domain] {
            ensure!(
                value.len() <= 256 && !value.chars().any(|c| c.is_control() || "\\/".contains(c)),
                "SMB account/domain invalid"
            );
        }
        ensure!(self.password.len() <= 4096, "SMB credential exceeds budget");
        Ok(())
    }
}
pub fn safe_component(name: &str) -> anyhow::Result<()> {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    ensure!(
        !name.is_empty()
            && !matches!(name, "." | "..")
            && name.encode_utf16().count() <= 255
            && !name.ends_with([' ', '.'])
            && !reserved
            && !name
                .chars()
                .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c)),
        "SMB unsafe name"
    );
    Ok(())
}
pub fn safe_path(path: &str) -> anyhow::Result<()> {
    valid_path(path)?;
    ensure!(path.len() <= 4096, "SMB path exceeds budget");
    if path != "/" {
        for name in path[1..].split('/') {
            safe_component(name)?;
        }
    }
    Ok(())
}

struct Process {
    child: Child,
    receiver: Option<mpsc::Receiver<anyhow::Result<Value>>>,
    reader: Option<thread::JoinHandle<()>>,
}
impl Drop for Process {
    fn drop(&mut self) {
        self.receiver.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

pub struct SmbBackend {
    settings: Settings,
    observations: Mutex<HashMap<String, Value>>,
    server_guid: Mutex<Option<String>>,
    cancel: Mutex<Arc<AtomicBool>>,
}
impl SmbBackend {
    pub fn new(settings: Settings) -> anyhow::Result<Self> {
        settings.validate()?;
        Ok(Self {
            settings,
            observations: Mutex::default(),
            server_guid: Mutex::default(),
            cancel: Mutex::new(Arc::new(AtomicBool::new(false))),
        })
    }
    pub fn probe(&self, cancel: &AtomicBool) -> anyhow::Result<()> {
        let rows = self.request(
            "stat",
            "/",
            None,
            &mut |_| {
                ensure!(!cancel.load(Ordering::Relaxed), "SMB operation cancelled");
                Ok(())
            },
            &mut std::io::sink(),
        )?;
        ensure!(
            rows.len() == 1 && rows[0]["directory"] == true,
            "SMB root directory unavailable"
        );
        Ok(())
    }
    fn check_cancel(&self) -> anyhow::Result<()> {
        ensure!(
            !self.cancel.lock().unwrap().load(Ordering::Relaxed),
            "SMB operation cancelled"
        );
        Ok(())
    }
    fn request(
        &self,
        op: &str,
        path: &str,
        expected: Option<&Value>,
        progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
        writer: &mut dyn Write,
    ) -> anyhow::Result<Vec<Value>> {
        safe_path(path)?;
        self.check_cancel()?;
        progress(0)?;
        let s = &self.settings;
        let req = json!({"op":op,"path":path,"server":s.server,"port":s.port,"share":s.share,
            "root":s.root,"username":s.username,"domain":s.domain,"password":s.password,
            "encrypt":s.encrypt,"server_guid":*self.server_guid.lock().unwrap(),"expected":expected});
        let python = std::env::var_os("TWODRIVE_SMB_PYTHON").unwrap_or_else(|| {
            if cfg!(windows) {
                "python".into()
            } else {
                "python3".into()
            }
        });
        let mut cmd = Command::new(python);
        cmd.args([
            "-I",
            "-u",
            "-c",
            include_str!("../components/smb_readonly.py"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            let parent = std::process::id();
            unsafe {
                cmd.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::getppid() as u32 != parent {
                        return Err(std::io::Error::other("SMB parent exited"));
                    }
                    Ok(())
                });
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let child = cmd.spawn().context(
            "SMB needs Python with components/requirements-smb.txt; set TWODRIVE_SMB_PYTHON",
        )?;
        let mut process = Process {
            child,
            receiver: None,
            reader: None,
        };
        let mut input = process
            .child
            .stdin
            .take()
            .context("SMB component input unavailable")?;
        serde_json::to_writer(&mut input, &req)?;
        input.write_all(b"\n")?;
        drop(input);
        let output = process
            .child
            .stdout
            .take()
            .context("SMB component output unavailable")?;
        let (tx, rx) = mpsc::sync_channel(2);
        process.receiver = Some(rx);
        process.reader = Some(thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                let mut line = Vec::new();
                let result = reader.by_ref().take(131073).read_until(b'\n', &mut line);
                match result {
                    Ok(0) => break,
                    Ok(n) if n <= 131072 && line.last() == Some(&b'\n') => {
                        let value = serde_json::from_slice(&line)
                            .map_err(|_| anyhow::anyhow!("SMB invalid component frame"));
                        if tx.send(value).is_err() {
                            break;
                        }
                    }
                    _ => {
                        let _ = tx.send(Err(anyhow::anyhow!(
                            "SMB component frame budget/read failure"
                        )));
                        break;
                    }
                }
            }
        }));
        let mut rows = Vec::new();
        let (mut hello, mut ended, mut verified) = (false, false, false);
        let mut done = 0_u64;
        let mut digest = Sha256::new();
        let mut last = Instant::now();
        loop {
            self.check_cancel()?;
            progress(done)?;
            ensure!(
                last.elapsed() < Duration::from_secs(30),
                "SMB component timed out"
            );
            let frame = match process
                .receiver
                .as_ref()
                .unwrap()
                .recv_timeout(Duration::from_millis(50))
            {
                Ok(value) => {
                    last = Instant::now();
                    value?
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            ensure!(!ended, "SMB data after completion");
            match frame["type"].as_str() {
                Some("hello") if !hello => {
                    let guid = frame["server_guid"]
                        .as_str()
                        .context("SMB missing server identity")?;
                    ensure!(
                        frame["dialect"]
                            .as_u64()
                            .is_some_and(|v| (0x202..=0x311).contains(&v)),
                        "SMB unsupported dialect"
                    );
                    ensure!(
                        !s.encrypt || frame["encrypted"] == true,
                        "SMB encryption required"
                    );
                    let mut known = self.server_guid.lock().unwrap();
                    ensure!(
                        known.as_ref().is_none_or(|old| old == guid),
                        "SMB server changed; reconnect explicitly"
                    );
                    *known = Some(guid.into());
                    hello = true;
                }
                Some("entry") if hello && op != "download" => {
                    ensure!(rows.len() < 10000, "SMB directory exceeds budget");
                    rows.push(frame);
                }
                Some("data") if hello && op == "download" && !verified => {
                    let data = STANDARD
                        .decode(frame["data"].as_str().context("SMB missing content")?)
                        .map_err(|_| anyhow::anyhow!("SMB invalid content frame"))?;
                    ensure!(
                        !data.is_empty() && data.len() <= 65536,
                        "SMB chunk exceeds budget"
                    );
                    done = done
                        .checked_add(data.len() as u64)
                        .context("SMB byte overflow")?;
                    ensure!(
                        Some(done) <= expected.and_then(|v| v["size"].as_u64()),
                        "SMB excess content"
                    );
                    digest.update(&data);
                    writer.write_all(&data)?;
                }
                Some("verified") if hello && op == "download" && !verified => {
                    let hash = format!("{:x}", digest.clone().finalize());
                    ensure!(
                        frame["size"].as_u64() == Some(done)
                            && expected.and_then(|v| v["size"].as_u64()) == Some(done)
                            && frame["sha256"].as_str() == Some(hash.as_str()),
                        "SMB incomplete download"
                    );
                    verified = true;
                }
                Some("end") if hello => {
                    ended = true;
                }
                Some("error") => {
                    // Only local allowlisted codes and numeric NTSTATUS escape the component.
                    let code = frame["code"].as_str().unwrap_or_default();
                    let public = match code {
                        "component_missing" | "component_version" => {
                            "component missing/version mismatch (requires smbprotocol 1.17.0)"
                        }
                        "link_rejected" | "dfs_rejected" => {
                            "links/reparse points/DFS are unsupported"
                        }
                        "reparse_safety_unavailable" => {
                            "server lacks required reparse/no-follow safety; unsupported server"
                        }
                        "unsafe_name" | "unsafe_path" => "unsafe path/name rejected",
                        "object_changed" | "server_changed" => {
                            "object/server changed; refresh or reconnect"
                        }
                        "directory_budget" => "directory exceeds safety budget",
                        _ => "request failed",
                    };
                    let status = frame["status"].as_u64().unwrap_or(0);
                    bail!("SMB {public} (NTSTATUS 0x{status:08x})");
                }
                _ => bail!("SMB invalid component sequence"),
            }
        }
        ensure!(
            hello && ended && (op != "download" || verified),
            "SMB disconnected/incomplete response"
        );
        // EOF need not imply process exit; poll with the same cancellation/deadline.
        let status = loop {
            self.check_cancel()?;
            progress(done)?;
            if let Some(status) = process.child.try_wait()? {
                break status;
            }
            ensure!(
                last.elapsed() < Duration::from_secs(30),
                "SMB component exit timed out"
            );
            thread::sleep(Duration::from_millis(20));
        };
        ensure!(status.success(), "SMB component failed");
        Ok(rows)
    }
    fn entries(&self, op: &str, path: &str) -> anyhow::Result<Vec<MetadataEntry>> {
        let rows = self.request(op, path, None, &mut |_| Ok(()), &mut std::io::sink())?;
        let mut result = Vec::new();
        let mut ids = self.observations.lock().unwrap();
        if ids.len() + rows.len() > 20000 {
            ids.clear();
        }
        for row in rows {
            let name = row["path"].as_str().context("SMB missing entry path")?;
            safe_path(name)?;
            if op == "stat" {
                ensure!(name == path, "SMB unrelated metadata");
            } else {
                let parent = name.rsplit_once('/').unwrap().0;
                ensure!(
                    if parent.is_empty() { "/" } else { parent } == path,
                    "SMB unrelated directory child"
                );
            }
            let id = random_secret();
            let size = row["size"].as_u64().context("SMB missing size")?;
            let modified = row["modified"].as_i64().context("SMB missing time")?;
            let entry = if row["directory"] == true {
                MetadataEntry::new_dir(&id, name, modified, "")
            } else {
                MetadataEntry::new_file(&id, name, size, modified, "")
            };
            ids.insert(id, row);
            result.push(entry);
        }
        Ok(result)
    }
}
impl Provider for SmbBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn supports_mount(&self) -> bool {
        false
    }
    fn set_operation_cancel(&self, cancel: Arc<AtomicBool>) {
        *self.cancel.lock().unwrap() = cancel;
    }
    fn list_directory(&self, path: &str) -> anyhow::Result<Vec<MetadataEntry>> {
        self.entries("list", path)
    }
    fn upload_controlled(
        &self,
        _: &str,
        _: &Path,
        _: Option<&str>,
        _: Option<&str>,
        _: Progress,
    ) -> anyhow::Result<MetadataEntry> {
        bail!("SMB is read-only")
    }
}
impl CloudBackend for SmbBackend {
    fn is_conflict_error(&self, _: &anyhow::Error) -> bool {
        false
    }
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        bail!("SMB recursive inventory/mount unavailable")
    }
    fn list_delta(&self, _: Option<&str>) -> anyhow::Result<DeltaResult> {
        bail!("SMB delta unavailable")
    }
    fn get_metadata(&self, id: &str) -> anyhow::Result<Option<MetadataEntry>> {
        let observed = self
            .observations
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .context("SMB observation expired; refresh")?;
        let entry =
            self.get_metadata_by_path(observed["path"].as_str().context("SMB missing path")?)?;
        if let Some(entry) = &entry {
            let ids = self.observations.lock().unwrap();
            let current = ids
                .get(&entry.remote_id)
                .context("SMB observation expired")?;
            ensure!(
                current["key"] == observed["key"],
                "SMB observed object replaced; refresh"
            );
        }
        Ok(entry)
    }
    fn get_metadata_by_path(&self, path: &str) -> anyhow::Result<Option<MetadataEntry>> {
        let rows = self.entries("stat", path)?;
        ensure!(rows.len() <= 1, "SMB ambiguous metadata");
        Ok(rows.into_iter().next())
    }
    fn download(&self, _: &str) -> anyhow::Result<Vec<u8>> {
        bail!("SMB requires streaming download_to")
    }
    fn download_to(
        &self,
        id: &str,
        writer: &mut dyn Write,
        progress: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        let expected = self
            .observations
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .context("SMB observation expired; refresh")?;
        self.request(
            "download",
            expected["path"].as_str().context("SMB missing path")?,
            Some(&expected),
            progress,
            writer,
        )?;
        expected["size"].as_u64().context("SMB missing size")
    }
    fn upload(&self, _: &str, _: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        bail!("SMB is read-only")
    }
    fn upload_with_etag(
        &self,
        _: &str,
        _: Vec<u8>,
        _: Option<&str>,
    ) -> anyhow::Result<MetadataEntry> {
        bail!("SMB is read-only")
    }
    fn upload_file_with_version(
        &self,
        _: &str,
        _: &Path,
        _: Option<&str>,
        _: Option<&str>,
        _: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<MetadataEntry> {
        bail!("SMB is read-only")
    }
    fn create_folder(&self, _: &str) -> anyhow::Result<MetadataEntry> {
        bail!("SMB is read-only")
    }
    fn rename(&self, _: &str, _: &str) -> anyhow::Result<MetadataEntry> {
        bail!("SMB is read-only")
    }
    fn delete(&self, _: &str) -> anyhow::Result<()> {
        bail!("SMB is read-only")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings() -> Settings {
        Settings {
            server: "127.0.0.1".into(),
            port: 445,
            share: "synthetic".into(),
            root: "/".into(),
            username: "test".into(),
            domain: "".into(),
            password: "synthetic".into(),
            encrypt: true,
        }
    }
    #[test]
    fn rejects_mutations_before_source_io_or_network() {
        let b = SmbBackend::new(settings()).unwrap();
        let caps = b.capabilities();
        assert!(!caps.upload && !caps.mkdir && !caps.rename && !caps.delete && !caps.backup());
        assert!(!b.supports_mount());
        assert!(b.upload("/x", vec![]).is_err());
        assert!(b.upload_with_etag("/x", vec![], Some("v")).is_err());
        assert!(
            b.upload_file_with_version(
                "/x",
                Path::new("/missing"),
                None,
                None,
                &mut |_, _| panic!("write progress called")
            )
            .is_err()
        );
        assert!(
            b.upload_controlled(
                "/x",
                Path::new("/missing"),
                None,
                None,
                Arc::new(|_, _| panic!("write progress called"))
            )
            .is_err()
        );
        assert!(b.create_folder("/x").is_err());
        assert!(b.rename("opaque", "/y").is_err());
        assert!(b.delete("opaque").is_err());
        assert!(b.list_all().is_err());
        assert!(b.list_delta(None).is_err());
        assert!(b.download("old-connection-id").is_err());
        assert!(
            b.download_to("old-connection-id", &mut vec![], &mut |_| Ok(()))
                .is_err()
        );
    }
    #[test]
    fn observation_ids_cannot_cross_connection_instances() {
        let first = SmbBackend::new(settings()).unwrap();
        first.observations.lock().unwrap().insert(
            "opaque-id".into(),
            json!({"path":"/x","key":"7","size":1,"change":"1"}),
        );
        let second = SmbBackend::new(settings()).unwrap();
        let mut output = Vec::new();
        let error = second
            .download_to("opaque-id", &mut output, &mut |_| Ok(()))
            .unwrap_err();
        assert!(error.to_string().contains("expired"));
        assert!(output.is_empty());
        assert!(second.get_metadata("opaque-id").is_err());
    }
    #[test]
    fn paths_and_endpoint_reject_escapes_and_preserve_unicode() {
        for path in [
            "relative",
            "/../x",
            "/a/../b",
            "/a//b",
            "/a\\b",
            "/x:stream",
            "/CON.txt",
            "/LPT1",
            "/x.",
            "/x ",
            "/nul",
            "/x\0",
            "/a/*",
            "/a/\n",
        ] {
            assert!(safe_path(path).is_err(), "accepted dangerous path");
        }
        for path in ["/", "/你好 space.txt", "/Mixed Case/é", "/COM10"] {
            safe_path(path).unwrap();
        }
        for host in ["host/share", "user@host", "host:445", "host\n", "-bad/host"] {
            let mut s = settings();
            s.server = host.into();
            assert!(s.validate().is_err());
        }
        let mut s = settings();
        s.password.clear();
        assert!(s.validate().is_err());
    }
}
