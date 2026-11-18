//! A cache snapshot keeps the working file as its base. Before a mounted
//! writer overwrites a block, it saves the original once. The short directory
//! lock coordinates other processes (including a concurrent CLI recovery).
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const BLOCK: u64 = 1024 * 1024;
const DIRECTORY: &str = ".twodrive-upload-snapshots";

pub(crate) fn upload_cached<B: twodrive_backend::CloudBackend>(
    backend: &B,
    path: &str,
    cache: &Path,
    progress: &mut dyn FnMut(u64, u64) -> anyhow::Result<()>,
) -> anyhow::Result<twodrive_core::MetadataEntry> {
    let gate = CacheGate::acquire(cache)?;
    let mut snapshot = gate
        .capture_named(cache, path)?
        .ok_or_else(|| io::Error::other("another transfer owns this snapshot"))?;
    drop(gate);
    backend.upload_snapshot_with_version(path, &mut snapshot, None, None, progress)
}
pub(crate) struct CacheGate {
    root: PathBuf,
    _lock: File,
}

impl CacheGate {
    fn lock_path(cache: &Path) -> io::Result<PathBuf> {
        let parent = cache
            .parent()
            .ok_or_else(|| io::Error::other("cache has no parent"))?;
        let name = cache
            .file_name()
            .ok_or_else(|| io::Error::other("missing cache name"))?;
        let stripe = crate::conflicts::stable_path_fingerprint(&name.to_string_lossy()) % 64;
        Ok(parent.join(DIRECTORY).join(format!("lock-{stripe}")))
    }
    pub(crate) fn acquire(cache: &Path) -> io::Result<Self> {
        let path = Self::lock_path(cache)?;
        let root = path.parent().unwrap().to_owned();
        let lock = match private_options().open(&path) {
            Ok(lock) => lock,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(root.join("entries"))?;
                private_options().create(true).truncate(false).open(path)?
            }
            Err(e) => return Err(e),
        };
        lock.lock()?;
        Ok(Self { root, _lock: lock })
    }
    #[cfg(unix)]
    pub(crate) fn pair(first: &Path, second: &Path) -> io::Result<(Self, Option<Self>)> {
        let first_key = Self::lock_path(first)?;
        let second_key = Self::lock_path(second)?;
        if first_key == second_key {
            return Ok((Self::acquire(first)?, None));
        }
        let (a, b) = if first_key < second_key {
            (first, second)
        } else {
            (second, first)
        };
        Ok((Self::acquire(a)?, Some(Self::acquire(b)?)))
    }

    fn entry(&self, path: &Path) -> io::Result<PathBuf> {
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("missing cache name"))?;
        Ok(self.root.join("entries").join(name))
    }

    /// Called before both overwrites and truncation, under the same gate as the mutation.
    pub(crate) fn preserve(&self, path: &Path, start: u64, end: u64) -> io::Result<()> {
        let entry = self.entry(path)?;
        let mut header = match File::open(entry.join("size")) {
            Ok(header) => header,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let mut bytes = [0; 8];
        header.read_exact(&mut bytes)?;
        let size = u64::from_le_bytes(bytes);
        let end = size.min(end);
        if start >= end {
            return Ok(());
        }
        let mut base = File::open(path)?;
        let mut buf = Vec::new();
        for index in start / BLOCK..end.div_ceil(BLOCK) {
            let block = entry.join(index.to_string());
            if block.try_exists()? {
                continue;
            }
            let offset = index * BLOCK;
            let len = (size - offset).min(BLOCK) as usize;
            buf.resize(len, 0);
            base.seek(SeekFrom::Start(offset))?;
            base.read_exact(&mut buf[..len])?;
            // A failed or interrupted write must not leave a partial block marked saved.
            let tmp = entry.join("saving");
            let mut saved = private_options().create(true).truncate(true).open(&tmp)?;
            saved.write_all(&buf[..len])?;
            saved.sync_data()?;
            drop(saved);
            fs::rename(tmp, block)?;
        }
        #[cfg(unix)]
        if !buf.is_empty() {
            File::open(entry)?.sync_all()?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn capture(&self, path: &Path) -> io::Result<Option<CacheSnapshot>> {
        self.capture_named(path, "")
    }
    pub(crate) fn capture_named(
        &self,
        path: &Path,
        _name: &str,
    ) -> io::Result<Option<CacheSnapshot>> {
        let entry = self.entry(path)?;
        fs::create_dir_all(&entry)?;
        let owner = private_options()
            .create(true)
            .truncate(false)
            .open(entry.join("owner"))?;
        match owner.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(err)) => return Err(err),
        }
        // A dead process cannot finish its session. Rebuild from the authoritative
        // working cache; the transport checks the entire source identity before resume.
        for item in fs::read_dir(&entry)? {
            let item = item?;
            if item.file_name() != "owner" {
                fs::remove_file(item.path())?;
            }
        }
        let mut base = File::open(path)?;
        let size = base.metadata()?.len();
        #[cfg(unix)]
        let office = {
            use crate::office::{Office, extension};
            let mut active = if extension(_name) {
                Office::enroll(path, path, true)
            } else {
                Office::open(path)
            }
            .map_err(io::Error::other)?;
            if let Some(o) = &mut active {
                o.freeze().map_err(io::Error::other)?;
            }
            active
        };
        #[cfg(unix)]
        let use_office = office.is_some();
        #[cfg(not(unix))]
        let use_office = false;
        #[cfg(unix)]
        let size = match &office {
            Some(o) => o.size(1).map_err(io::Error::other)?,
            None => size,
        };
        let memory = if use_office {
            None
        } else if size <= 10 * 1024 * 1024 {
            let mut bytes = vec![0; size as usize];
            base.read_exact(&mut bytes)?;
            Some(bytes)
        } else {
            let mut header = private_options()
                .create_new(true)
                .open(entry.join("size"))?;
            header.write_all(&size.to_le_bytes())?;
            header.sync_all()?;
            None
        };
        Ok(Some(CacheSnapshot {
            path: path.into(),
            entry,
            base,
            #[cfg(unix)]
            office,
            memory,
            owner: Some(owner),
            size,
            pos: 0,
        }))
    }
}

/// A crash can leave block files after the last network acknowledgement. Only
/// retire recognized inactive snapshots with a surviving working cache. Keep
/// fragments whose base is missing available for recovery.
#[cfg(unix)]
pub(crate) fn prune_abandoned(cache_dir: &Path) -> io::Result<()> {
    let entries = cache_dir.join(DIRECTORY).join("entries");
    let candidates = match fs::read_dir(entries) {
        Ok(items) => items,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for candidate in candidates {
        let candidate = candidate?;
        let path = cache_dir.join(candidate.file_name());
        if !candidate.file_type()?.is_dir() || !path.is_file() {
            continue;
        }
        let _gate = CacheGate::acquire(&path)?;
        let entry = candidate.path();
        let owner = match private_options().open(entry.join("owner")) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        match owner.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => continue,
            Err(std::fs::TryLockError::Error(err)) => return Err(err),
        }
        for child in fs::read_dir(&entry)? {
            let child = child?;
            let name = child.file_name();
            let name = name.to_string_lossy();
            if !child.file_type()?.is_file()
                || !(matches!(&*name, "size" | "owner" | "saving") || name.parse::<u64>().is_ok())
            {
                return Err(io::Error::other("unrecognized snapshot recovery material"));
            }
        }
        if let Some(mut o) = crate::office::Office::open(&path).map_err(io::Error::other)? {
            o.thaw().map_err(io::Error::other)?;
        }
        drop(owner);
        fs::remove_dir_all(entry)?;
    }
    Ok(())
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

pub(crate) struct CacheSnapshot {
    path: PathBuf,
    entry: PathBuf,
    base: File,
    #[cfg(unix)]
    office: Option<crate::office::Office>,
    memory: Option<Vec<u8>>,
    owner: Option<File>,
    size: u64,
    pos: u64,
}

impl Read for CacheSnapshot {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.size || buf.is_empty() {
            return Ok(0);
        }
        if let Some(bytes) = &self.memory {
            let n = (self.size - self.pos).min(buf.len() as u64) as usize;
            let start = self.pos as usize;
            buf[..n].copy_from_slice(&bytes[start..start + n]);
            self.pos += n as u64;
            return Ok(n);
        }
        let _gate = CacheGate::acquire(&self.path)?;
        #[cfg(unix)]
        if let Some(office) = &self.office {
            let n = office.read(1, self.pos, buf).map_err(io::Error::other)?;
            self.pos += n as u64;
            return Ok(n);
        }
        let index = self.pos / BLOCK;
        let within = self.pos % BLOCK;
        let n = (self.size - self.pos)
            .min(BLOCK - within)
            .min(buf.len() as u64) as usize;
        match File::open(self.entry.join(index.to_string())) {
            Ok(mut old) => {
                old.seek(SeekFrom::Start(within))?;
                old.read_exact(&mut buf[..n])?;
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                self.base.seek(SeekFrom::Start(self.pos))?;
                self.base.read_exact(&mut buf[..n])?;
            }
            Err(err) => return Err(err),
        }
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for CacheSnapshot {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.pos = match pos {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(n) => self.size.checked_add_signed(n),
            SeekFrom::Current(n) => self.pos.checked_add_signed(n),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid snapshot offset"))?;
        Ok(self.pos)
    }
}

impl Drop for CacheSnapshot {
    fn drop(&mut self) {
        if let Ok(_gate) = CacheGate::acquire(&self.path) {
            #[cfg(unix)]
            if let Some(office) = &mut self.office
                && let Err(err) = office.thaw()
            {
                eprintln!("twodrive: frozen Office cleanup deferred: {err}");
            }
            drop(self.owner.take());
            if let Err(err) = fs::remove_dir_all(&self.entry) {
                eprintln!("twodrive: snapshot cleanup deferred: {err}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overwritten_and_truncated_blocks_remain_readable_out_of_order() {
        let root = std::env::temp_dir().join(format!(
            "twodrive-cow-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("working");
        let original: Vec<_> = (0..BLOCK * 11 + 17).map(|i| (i % 251) as u8).collect();
        fs::write(&path, &original).unwrap();
        let gate = CacheGate::acquire(&path).unwrap();
        let mut snapshot = gate.capture(&path).unwrap().unwrap();
        assert!(gate.capture(&path).unwrap().is_none());
        gate.preserve(&path, BLOCK + 1, BLOCK + 2).unwrap();
        let mut working = private_options().open(&path).unwrap();
        working.seek(SeekFrom::Start(BLOCK + 1)).unwrap();
        working.write_all(&[0xff]).unwrap();
        gate.preserve(&path, BLOCK + 2, u64::MAX).unwrap();
        working.set_len(BLOCK + 2).unwrap();
        drop(gate);
        snapshot.seek(SeekFrom::Start(BLOCK * 2)).unwrap();
        let mut tail = Vec::new();
        snapshot.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, original[(BLOCK * 2) as usize..]);
        snapshot.rewind().unwrap();
        let mut all = Vec::new();
        snapshot.read_to_end(&mut all).unwrap();
        assert_eq!(all, original);
        let entry = snapshot.entry.clone();
        drop(snapshot);
        assert!(!entry.exists());
        drop(working);
        fs::remove_dir_all(root).unwrap();
    }
}
