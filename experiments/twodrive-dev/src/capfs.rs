//! Capability-scoped filesystem with atomic full-file PUTs.
use bytes::{Buf, Bytes};
use cap_std::fs::{Dir, Metadata, OpenOptions as CapOptions};
use dav_server::{davpath::DavPath, fs::*};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

const TEMP_PREFIX: &str = ".twodrive-upload-";
#[derive(Clone)]
pub struct CapFs(Arc<Dir>);

fn error(error: std::io::Error) -> FsError {
    match error.kind() {
        std::io::ErrorKind::NotFound => FsError::NotFound,
        std::io::ErrorKind::AlreadyExists => FsError::Exists,
        std::io::ErrorKind::PermissionDenied => FsError::Forbidden,
        _ => FsError::GeneralFailure,
    }
}
fn path(path: &DavPath) -> FsResult<PathBuf> {
    let path = path.as_rel_ospath();
    if path
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
        || path.components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .starts_with(TEMP_PREFIX)
        })
    {
        return Err(FsError::Forbidden);
    }
    Ok(if path.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        path.to_owned()
    })
}
fn blocking<'a, T: Send + 'a>(action: impl FnOnce() -> FsResult<T> + Send + 'a) -> FsFuture<'a, T> {
    Box::pin(async move { tokio::task::block_in_place(action) })
}

impl CapFs {
    pub fn new(root: &Path) -> anyhow::Result<Self> {
        Ok(Self(Arc::new(Dir::open_ambient_dir(
            root,
            cap_std::ambient_authority(),
        )?)))
    }
}

#[derive(Debug, Clone)]
struct Meta(Metadata);
impl DavMetaData for Meta {
    fn len(&self) -> u64 {
        self.0.len()
    }
    fn modified(&self) -> FsResult<SystemTime> {
        self.0.modified().map(|time| time.into_std()).map_err(error)
    }
    fn is_dir(&self) -> bool {
        self.0.is_dir()
    }
    fn is_symlink(&self) -> bool {
        self.0.is_symlink()
    }
    fn etag(&self) -> Option<String> {
        let time = self
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        #[cfg(unix)]
        let identity = {
            use cap_std::fs::MetadataExt;
            self.0.ino()
        };
        #[cfg(not(unix))]
        let identity = 0;
        Some(format!("{:x}-{time:x}-{identity:x}", self.len()))
    }
}
struct Entry {
    name: Vec<u8>,
    meta: Meta,
}
impl DavDirEntry for Entry {
    fn name(&self) -> Vec<u8> {
        self.name.clone()
    }
    fn metadata(&self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        Box::pin(std::future::ready(Ok(Box::new(self.meta.clone()) as _)))
    }
}

struct AtomicFile {
    file: cap_std::fs::File,
    dir: Arc<Dir>,
    pending: Option<(PathBuf, PathBuf)>,
    size: Option<u64>,
    written: u64,
}
impl std::fmt::Debug for AtomicFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AtomicFile")
    }
}
impl Drop for AtomicFile {
    fn drop(&mut self) {
        if let Some((temp, _)) = &self.pending {
            let _ = self.dir.remove_file(temp);
        }
    }
}
impl DavFile for AtomicFile {
    fn metadata(&mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        blocking(|| Ok(Box::new(Meta(self.file.metadata().map_err(error)?)) as _))
    }
    fn write_buf(&mut self, mut buffer: Box<dyn Buf + Send>) -> FsFuture<'_, ()> {
        blocking(move || {
            while buffer.has_remaining() {
                let chunk = buffer.chunk();
                self.file.write_all(chunk).map_err(error)?;
                let count = chunk.len();
                self.written += count as u64;
                buffer.advance(count);
            }
            Ok(())
        })
    }
    fn write_bytes(&mut self, bytes: Bytes) -> FsFuture<'_, ()> {
        blocking(move || {
            self.file.write_all(&bytes).map_err(error)?;
            self.written += bytes.len() as u64;
            Ok(())
        })
    }
    fn read_bytes(&mut self, count: usize) -> FsFuture<'_, Bytes> {
        blocking(move || {
            let mut bytes = vec![0; count.min(1024 * 1024)];
            let count = self.file.read(&mut bytes).map_err(error)?;
            bytes.truncate(count);
            Ok(bytes.into())
        })
    }
    fn seek(&mut self, position: SeekFrom) -> FsFuture<'_, u64> {
        blocking(move || self.file.seek(position).map_err(error))
    }
    fn flush(&mut self) -> FsFuture<'_, ()> {
        blocking(|| {
            if self.size.is_some_and(|size| size != self.written) {
                return Err(FsError::GeneralFailure);
            }
            self.file.sync_all().map_err(error)?;
            if let Some((temp, destination)) = &self.pending {
                self.dir
                    .rename(temp, &self.dir, destination)
                    .map_err(error)?;
                // cap-std's directory capability can be an O_PATH handle on Linux,
                // which cannot be fsynced. Open a readable handle to the *parent*
                // so nested renames are persisted without escaping the capability.
                #[cfg(unix)]
                self.dir
                    .open_with(
                        destination
                            .parent()
                            .filter(|parent| !parent.as_os_str().is_empty())
                            .unwrap_or(Path::new(".")),
                        CapOptions::new().read(true),
                    )
                    .map_err(error)?
                    .sync_all()
                    .map_err(error)?;
                self.pending = None;
            }
            Ok(())
        })
    }
}

impl DavFileSystem for CapFs {
    fn open<'a>(
        &'a self,
        dav: &'a DavPath,
        options: OpenOptions,
    ) -> FsFuture<'a, Box<dyn DavFile>> {
        blocking(move || {
            let destination = path(dav)?;
            if options.write {
                if !options.truncate || options.append || destination == Path::new(".") {
                    return Err(FsError::NotImplemented);
                }
                let existing = match self.0.metadata(&destination) {
                    Ok(meta) => Some(meta),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => return Err(error(e)),
                };
                if existing.as_ref().is_some_and(Metadata::is_dir) {
                    return Err(FsError::Forbidden);
                }
                if options.create_new && existing.is_some() {
                    return Err(FsError::Exists);
                }
                if !options.create && existing.is_none() {
                    return Err(FsError::NotFound);
                }
                let temp = destination
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join(format!("{TEMP_PREFIX}{}", crate::state::random_secret()));
                let mut opts = CapOptions::new();
                opts.read(true).write(true).create_new(true);
                #[cfg(unix)]
                {
                    use cap_std::fs::OpenOptionsExt;
                    opts.mode(0o600);
                }
                let file = self.0.open_with(&temp, &opts).map_err(error)?;
                if let Some(existing) = existing
                    && let Err(e) = file.set_permissions(existing.permissions())
                {
                    let _ = self.0.remove_file(&temp);
                    return Err(error(e));
                }
                Ok(Box::new(AtomicFile {
                    file,
                    dir: self.0.clone(),
                    pending: Some((temp, destination)),
                    size: options.size,
                    written: 0,
                }) as _)
            } else {
                if !self.0.metadata(&destination).map_err(error)?.is_file() {
                    return Err(FsError::Forbidden);
                }
                let mut opts = CapOptions::new();
                opts.read(true);
                // Avoid blocking if a local writer substitutes a FIFO after stat.
                #[cfg(unix)]
                {
                    use cap_std::fs::OpenOptionsExt;
                    opts.custom_flags(libc::O_NONBLOCK);
                }
                let file = self.0.open_with(&destination, &opts).map_err(error)?;
                if !file.metadata().map_err(error)?.is_file() {
                    return Err(FsError::Forbidden);
                }
                Ok(Box::new(AtomicFile {
                    file,
                    dir: self.0.clone(),
                    pending: None,
                    size: None,
                    written: 0,
                }) as _)
            }
        })
    }
    fn read_dir<'a>(
        &'a self,
        dav: &'a DavPath,
        _meta: ReadDirMeta,
    ) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
        blocking(move || {
            let directory = path(dav)?;
            let mut entries: Vec<FsResult<Box<dyn DavDirEntry>>> = Vec::new();
            for entry in self.0.read_dir(&directory).map_err(error)? {
                let entry = entry.map_err(error)?;
                let name = entry.file_name();
                if name.to_string_lossy().starts_with(TEMP_PREFIX)
                    || entry.file_type().map_err(error)?.is_symlink()
                {
                    continue;
                }
                let meta = self.0.metadata(directory.join(&name)).map_err(error)?;
                if !meta.is_dir() && !meta.is_file() {
                    continue;
                }
                #[cfg(unix)]
                let name = {
                    use std::os::unix::ffi::OsStrExt;
                    name.as_bytes().to_vec()
                };
                #[cfg(not(unix))]
                let name = name.to_string_lossy().as_bytes().to_vec();
                entries.push(Ok(Box::new(Entry {
                    name,
                    meta: Meta(meta),
                })));
            }
            Ok(Box::pin(futures_util::stream::iter(entries)) as _)
        })
    }
    fn metadata<'a>(&'a self, dav: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
        blocking(move || Ok(Box::new(Meta(self.0.metadata(path(dav)?).map_err(error)?)) as _))
    }
    fn symlink_metadata<'a>(&'a self, dav: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
        blocking(move || {
            Ok(Box::new(Meta(self.0.symlink_metadata(path(dav)?).map_err(error)?)) as _)
        })
    }
    fn create_dir<'a>(&'a self, dav: &'a DavPath) -> FsFuture<'a, ()> {
        blocking(move || self.0.create_dir(path(dav)?).map_err(error))
    }
    fn remove_dir<'a>(&'a self, dav: &'a DavPath) -> FsFuture<'a, ()> {
        blocking(move || {
            let path = path(dav)?;
            if path == Path::new(".") {
                return Err(FsError::Forbidden);
            }
            self.0.remove_dir(path).map_err(error)
        })
    }
    fn remove_file<'a>(&'a self, dav: &'a DavPath) -> FsFuture<'a, ()> {
        blocking(move || self.0.remove_file(path(dav)?).map_err(error))
    }
    fn rename<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        blocking(move || {
            let from = path(from)?;
            let to = path(to)?;
            if from == Path::new(".") || to == Path::new(".") {
                return Err(FsError::Forbidden);
            }
            self.0.rename(from, &self.0, to).map_err(error)
        })
    }
    fn copy<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        blocking(move || {
            self.0
                .copy(path(from)?, &self.0, path(to)?)
                .map(|_| ())
                .map_err(error)
        })
    }
}
