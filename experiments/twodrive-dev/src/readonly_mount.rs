//! A private loopback read-only filesystem bridge over the existing Provider
//! contract. The native filesystem component never receives cloud credentials.
use crate::{
    model::{CloudBackend, MetadataEntry},
    provider::{Backend, valid_path},
    state::random_secret,
};
use anyhow::{Context, ensure};
use bytes::{Buf, Bytes};
use dav_server::{DavHandler, body::Body, davpath::DavPath, fs::*};
use http::{Request, Response, StatusCode};
use http_body_util::BodyExt;
use hyper::{body::Incoming, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    convert::Infallible,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    net::SocketAddr,
    path::Path,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use tokio::{net::TcpListener, sync::Semaphore};

#[path = "mount_metadata.rs"]
mod metadata;

struct Cached {
    meta: MetadataEntry,
    file: tempfile::NamedTempFile,
}
struct Download {
    meta: MetadataEntry,
    lock: Weak<Mutex<()>>,
}
struct QuotaSample {
    checked: Instant,
    value: Option<(u64, u64)>,
}
struct Inner {
    backend: Backend,
    metadata: Option<Arc<metadata::Cache>>,
    writer: Option<Arc<crate::mount_write::Writer>>,
    cache: Mutex<VecDeque<Arc<Cached>>>,
    downloads: Mutex<Vec<Download>>,
    quota: Mutex<Option<QuotaSample>>,
    directory: tempfile::TempDir,
    stop: Arc<AtomicBool>,
}
#[derive(Clone)]
struct RemoteFs(Arc<Inner>, bool, bool); // The HEAD view may open metadata, never content.
#[derive(Clone, Debug)]
struct Meta(MetadataEntry);
impl DavMetaData for Meta {
    fn len(&self) -> u64 {
        self.0.size
    }
    fn modified(&self) -> FsResult<SystemTime> {
        UNIX_EPOCH
            .checked_add(Duration::from_secs(self.0.modified_unix.max(0) as u64))
            .ok_or(FsError::GeneralFailure)
    }
    fn created(&self) -> FsResult<SystemTime> {
        self.0
            .created_unix
            .filter(|value| *value > 0)
            .and_then(|value| UNIX_EPOCH.checked_add(Duration::from_secs(value as u64)))
            .ok_or(FsError::NotImplemented)
    }
    fn is_dir(&self) -> bool {
        self.0.is_dir
    }
    fn etag(&self) -> Option<String> {
        Some(format!(
            "{:x}",
            Sha256::digest(format!(
                "{}\0{}\0{}",
                self.0.remote_id, self.0.etag, self.0.size
            ))
        ))
    }
}
struct Entry(Meta);
impl DavDirEntry for Entry {
    fn name(&self) -> Vec<u8> {
        self.0.0.name.as_bytes().to_vec()
    }
    fn metadata(&self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        Box::pin(std::future::ready(Ok(Box::new(self.0.clone()) as _)))
    }
}
fn blocking<'a, T: Send + 'a>(action: impl FnOnce() -> FsResult<T> + Send + 'a) -> FsFuture<'a, T> {
    Box::pin(async move { tokio::task::block_in_place(action) })
}
fn resource(dav: &DavPath) -> FsResult<String> {
    let value = std::str::from_utf8(dav.as_bytes()).map_err(|_| FsError::Forbidden)?;
    let value = if value == "/" {
        value
    } else {
        value.trim_end_matches('/')
    };
    valid_path(value).map_err(|_| FsError::Forbidden)?;
    Ok(value.into())
}
fn same(a: &MetadataEntry, b: &MetadataEntry) -> bool {
    a.remote_id == b.remote_id
        && a.path == b.path
        && a.etag == b.etag
        && a.size == b.size
        && a.is_dir == b.is_dir
}
impl RemoteFs {
    fn check(&self) -> anyhow::Result<()> {
        ensure!(!self.0.stop.load(Ordering::Relaxed), "mount stopped");
        self.0.backend.check_read_access()
    }
    fn entry(&self, path: &str) -> FsResult<MetadataEntry> {
        self.check().map_err(|_| FsError::GeneralFailure)?;
        let load = || self.0.backend.get_metadata_by_path(path);
        let entry = if self.2
            && let Some(cache) = &self.0.metadata
        {
            cache.metadata(path, || self.check(), load)
        } else {
            load()
        }
        .map_err(|_| FsError::GeneralFailure)?
        .ok_or(FsError::NotFound)?;
        if let Some(writer) = &self.0.writer {
            writer
                .observe(&entry)
                .map_err(|_| FsError::GeneralFailure)?;
        }
        Ok(entry)
    }
    fn directory(&self, path: &str) -> anyhow::Result<Vec<MetadataEntry>> {
        let load = || {
            let entries = self
                .0
                .backend
                .list_directory_checked(path, &mut || self.check())?;
            ensure!(entries.len() <= 100000, "directory response exceeds budget");
            for entry in &entries {
                valid_path(&entry.path)?;
                ensure!(
                    entry.parent_path == path
                        && entry.path != path
                        && !entry.name.is_empty()
                        && !entry.name.contains('/')
                        && entry.path == format!("{}/{}", path.trim_end_matches('/'), entry.name),
                    "invalid directory response"
                );
            }
            Ok(entries)
        };
        if let Some(cache) = &self.0.metadata {
            cache.list(path, || self.check(), load)
        } else {
            load()
        }
    }
    fn cached(&self, entry: &MetadataEntry) -> anyhow::Result<Option<Arc<Cached>>> {
        let mut cache = self
            .0
            .cache
            .lock()
            .map_err(|_| anyhow::anyhow!("cache unavailable"))?;
        if let Some(i) = cache.iter().position(|item| same(&item.meta, entry)) {
            let item = cache.remove(i).unwrap();
            cache.push_front(item.clone());
            return Ok(Some(item));
        }
        Ok(None)
    }
    fn materialize(&self, entry: MetadataEntry) -> anyhow::Result<Arc<Cached>> {
        self.check()?;
        if let Some(item) = self.cached(&entry)? {
            return Ok(item);
        }
        // Only readers of the same object/version wait together. Never hold
        // the global cache lock during network I/O for an unrelated file.
        let lock = {
            let mut downloads = self
                .0
                .downloads
                .lock()
                .map_err(|_| anyhow::anyhow!("download coordination unavailable"))?;
            downloads.retain(|item| item.lock.strong_count() != 0);
            if let Some(lock) = downloads
                .iter()
                .find(|item| same(&item.meta, &entry))
                .and_then(|item| item.lock.upgrade())
            {
                lock
            } else {
                let lock = Arc::new(Mutex::new(()));
                downloads.push(Download {
                    meta: entry.clone(),
                    lock: Arc::downgrade(&lock),
                });
                lock
            }
        };
        let _download = lock
            .lock()
            .map_err(|_| anyhow::anyhow!("download unavailable"))?;
        self.check()?;
        if let Some(item) = self.cached(&entry)? {
            return Ok(item);
        }
        // The complete temporary file is published only after the provider's
        // opaque object/version and byte count are verified. No Vec-sized file.
        let mut temp = tempfile::NamedTempFile::new_in(self.0.directory.path())?;
        struct Bounded<'a> {
            file: &'a mut File,
            remaining: u64,
        }
        impl Write for Bounded<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if bytes.len() as u64 > self.remaining {
                    return Err(std::io::Error::other("unexpected download size"));
                }
                let done = self.file.write(bytes)?;
                self.remaining -= done as u64;
                Ok(done)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.file.flush()
            }
        }
        let done = self.0.backend.download_to(
            &entry.remote_id,
            &mut Bounded {
                file: temp.as_file_mut(),
                remaining: entry.size,
            },
            &mut |_| self.check(),
        )?;
        ensure!(
            done == entry.size && temp.as_file().metadata()?.len() == entry.size,
            "incomplete mount download"
        );
        let after = self
            .0
            .backend
            .get_metadata_by_path(&entry.path)?
            .context("file removed during download")?;
        ensure!(same(&entry, &after), "file changed during download");
        self.check()?;
        let item = Arc::new(Cached {
            meta: entry,
            file: temp,
        });
        let mut cache = self
            .0
            .cache
            .lock()
            .map_err(|_| anyhow::anyhow!("cache unavailable"))?;
        cache.push_front(item.clone());
        // Retain at most eight versions / 2 GiB, except one larger currently
        // requested file. Active readers keep their own Arc and file handle.
        while cache.len() > 1
            && (cache.len() > 8
                || cache.iter().map(|v| v.meta.size as u128).sum::<u128>() > 2 * 1024 * 1024 * 1024)
        {
            cache.pop_back();
        }
        Ok(item)
    }
}
struct RemoteFile {
    cached: Arc<Cached>,
    file: File,
}
#[derive(Debug)]
struct MetadataFile {
    meta: MetadataEntry,
    position: u64,
}
impl DavFile for MetadataFile {
    fn metadata(&mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        Box::pin(std::future::ready(Ok(
            Box::new(Meta(self.meta.clone())) as _
        )))
    }
    fn read_bytes(&mut self, _: usize) -> FsFuture<'_, Bytes> {
        Box::pin(std::future::ready(Err(FsError::Forbidden)))
    }
    fn seek(&mut self, at: SeekFrom) -> FsFuture<'_, u64> {
        let position = match at {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(n) => self.position.checked_add_signed(n),
            SeekFrom::End(n) => self.meta.size.checked_add_signed(n),
        };
        Box::pin(std::future::ready(match position {
            Some(n) => {
                self.position = n;
                Ok(n)
            }
            None => Err(FsError::GeneralFailure),
        }))
    }
    fn write_buf(&mut self, _: Box<dyn Buf + Send>) -> FsFuture<'_, ()> {
        Box::pin(std::future::ready(Err(FsError::Forbidden)))
    }
    fn write_bytes(&mut self, _: Bytes) -> FsFuture<'_, ()> {
        Box::pin(std::future::ready(Err(FsError::Forbidden)))
    }
    fn flush(&mut self) -> FsFuture<'_, ()> {
        Box::pin(std::future::ready(Ok(())))
    }
}
impl std::fmt::Debug for RemoteFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReadOnlyMountFile")
    }
}
impl DavFile for RemoteFile {
    fn metadata(&mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        Box::pin(std::future::ready(Ok(
            Box::new(Meta(self.cached.meta.clone())) as _,
        )))
    }
    fn read_bytes(&mut self, count: usize) -> FsFuture<'_, Bytes> {
        blocking(move || {
            let mut buffer = vec![0; count.min(65536)];
            let done = self
                .file
                .read(&mut buffer)
                .map_err(|_| FsError::GeneralFailure)?;
            buffer.truncate(done);
            Ok(buffer.into())
        })
    }
    fn seek(&mut self, at: SeekFrom) -> FsFuture<'_, u64> {
        blocking(move || self.file.seek(at).map_err(|_| FsError::GeneralFailure))
    }
    fn write_buf(&mut self, _: Box<dyn Buf + Send>) -> FsFuture<'_, ()> {
        Box::pin(std::future::ready(Err(FsError::Forbidden)))
    }
    fn write_bytes(&mut self, _: Bytes) -> FsFuture<'_, ()> {
        Box::pin(std::future::ready(Err(FsError::Forbidden)))
    }
    fn flush(&mut self) -> FsFuture<'_, ()> {
        Box::pin(std::future::ready(Ok(())))
    }
}
impl DavFileSystem for RemoteFs {
    fn get_quota(&self) -> FsFuture<'_, (u64, Option<u64>)> {
        blocking(|| {
            let mut cached = self.0.quota.lock().map_err(|_| FsError::GeneralFailure)?;
            let valid = cached.as_ref().is_some_and(|sample| {
                sample.checked.elapsed()
                    < Duration::from_secs(if sample.value.is_some() { 300 } else { 30 })
            });
            if !valid {
                let usage = self
                    .0
                    .backend
                    .storage_usage(&mut || self.check())
                    .ok()
                    .flatten();
                let value = usage.and_then(|value| value.used.zip(value.total));
                *cached = Some(QuotaSample {
                    checked: Instant::now(),
                    value,
                });
            }
            cached
                .as_ref()
                .and_then(|sample| sample.value)
                .map(|(used, total)| (used, Some(total)))
                .ok_or(FsError::NotImplemented)
        })
    }
    fn open<'a>(
        &'a self,
        dav: &'a DavPath,
        options: OpenOptions,
    ) -> FsFuture<'a, Box<dyn DavFile>> {
        blocking(move || {
            if options.write
                || options.append
                || options.truncate
                || options.create
                || options.create_new
            {
                return Err(FsError::Forbidden);
            }
            let entry = self.entry(&resource(dav)?)?;
            if entry.is_dir {
                return Err(FsError::Forbidden);
            }
            if self.1 {
                // dav-server opens files for HEAD as well as GET. Reuse its
                // conditional/range handling without fetching any body bytes.
                return Ok(Box::new(MetadataFile {
                    meta: entry,
                    position: 0,
                }) as _);
            }
            let cached = self
                .materialize(entry)
                .map_err(|_| FsError::GeneralFailure)?;
            // reopen, rather than try_clone: concurrent HTTP ranges need
            // independent file cursors, including on Windows.
            let file = cached.file.reopen().map_err(|_| FsError::GeneralFailure)?;
            Ok(Box::new(RemoteFile { cached, file }) as _)
        })
    }
    fn read_dir<'a>(
        &'a self,
        dav: &'a DavPath,
        _: ReadDirMeta,
    ) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
        blocking(move || {
            let path = resource(dav)?;
            let entries = self.directory(&path).map_err(|_| FsError::GeneralFailure)?;
            if entries.len() > 100000 {
                return Err(FsError::GeneralFailure);
            }
            let mut result: Vec<FsResult<Box<dyn DavDirEntry>>> = Vec::new();
            for entry in &entries {
                if valid_path(&entry.path).is_err()
                    || entry.parent_path != path
                    || entry.path == path
                    || entry.name.is_empty()
                    || entry.name.contains('/')
                    || entry.path != format!("{}/{}", path.trim_end_matches('/'), entry.name)
                {
                    return Err(FsError::GeneralFailure);
                }
            }
            if let Some(writer) = &self.0.writer {
                writer
                    .observe_many(&entries)
                    .map_err(|_| FsError::GeneralFailure)?;
            }
            for entry in entries {
                result.push(Ok(Box::new(Entry(Meta(entry)))));
            }
            Ok(Box::pin(futures_util::stream::iter(result)) as _)
        })
    }
    fn metadata<'a>(&'a self, dav: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
        blocking(move || Ok(Box::new(Meta(self.entry(&resource(dav)?)?)) as _))
    }
}

pub struct Bridge {
    pub address: SocketAddr,
    pub token: String,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Bridge {
    pub fn start(backend: Backend, cache_parent: &Path) -> anyhow::Result<Self> {
        Self::start_inner(backend, cache_parent, None)
    }
    pub fn start_writable(
        backend: Backend,
        cache_parent: &Path,
        spool: &Path,
    ) -> anyhow::Result<Self> {
        let writer = crate::mount_write::Writer::new(backend.clone(), spool)?;
        Self::start_inner(backend, cache_parent, Some(writer))
    }
    fn start_inner(
        backend: Backend,
        cache_parent: &Path,
        writer: Option<Arc<crate::mount_write::Writer>>,
    ) -> anyhow::Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("readonly-mount-")
            .tempdir_in(cache_parent)?;
        let stop = Arc::new(AtomicBool::new(false));
        let metadata = (!backend.browse_cache_ttl().is_zero()).then(|| {
            Arc::new(metadata::Cache::new(
                backend.browse_cache_ttl().min(Duration::from_secs(2)),
            ))
        });
        let fs = RemoteFs(
            Arc::new(Inner {
                backend,
                metadata: metadata.clone(),
                writer: writer.clone(),
                cache: Mutex::default(),
                downloads: Mutex::default(),
                quota: Mutex::default(),
                directory,
                stop: stop.clone(),
            }),
            false,
            true,
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let token = random_secret();
        let auth = format!("Bearer {token}");
        let thread_stop = stop.clone();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let thread = thread::spawn(move || {
            runtime.block_on(async move {
                let Ok(listener) = TcpListener::from_std(listener) else {
                    return;
                };
                let head_handler = DavHandler::builder()
                    .filesystem(Box::new(RemoteFs(fs.0.clone(), true, true)))
                    .build_handler();
                let content_handler = DavHandler::builder()
                    .filesystem(Box::new(RemoteFs(fs.0.clone(), false, false)))
                    .build_handler();
                let handler = DavHandler::builder()
                    .filesystem(Box::new(fs))
                    .build_handler();
                let slots = Arc::new(Semaphore::new(16));
                while !thread_stop.load(Ordering::Relaxed) {
                    let accepted =
                        tokio::time::timeout(Duration::from_millis(50), listener.accept()).await;
                    let Ok(Ok((socket, _))) = accepted else {
                        continue;
                    };
                    let Ok(permit) = slots.clone().try_acquire_owned() else {
                        continue;
                    };
                    let handler = handler.clone();
                    let head_handler = head_handler.clone();
                    let content_handler = content_handler.clone();
                    let auth = auth.clone();
                    let writer = writer.clone();
                    let metadata = metadata.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        let service = service_fn(move |request: Request<Incoming>| {
                            let handler = if request.method() == http::Method::HEAD {
                                head_handler.clone()
                            } else if request.method() == http::Method::GET {
                                content_handler.clone()
                            } else {
                                handler.clone()
                            };
                            let auth = auth.clone();
                            let writer = writer.clone();
                            let metadata = metadata.clone();
                            async move {
                                let actual = request
                                    .headers()
                                    .get("Authorization")
                                    .map(|v| v.as_bytes())
                                    .unwrap_or_default();
                                let denied = if !bool::from(actual.ct_eq(auth.as_bytes())) {
                                    Some(StatusCode::UNAUTHORIZED)
                                } else if !matches!(
                                    request.method().as_str(),
                                    "GET" | "HEAD" | "OPTIONS" | "PROPFIND"
                                ) && !(writer.is_some()
                                    && matches!(
                                        request.method().as_str(),
                                        "PUT" | "MKCOL" | "DELETE" | "MOVE"
                                    ))
                                    || request.method().as_str() == "PROPFIND"
                                        && !matches!(
                                            request
                                                .headers()
                                                .get("Depth")
                                                .and_then(|v| v.to_str().ok()),
                                            Some("0" | "1")
                                        )
                                {
                                    Some(StatusCode::FORBIDDEN)
                                } else if request.uri().query().is_some() {
                                    Some(StatusCode::BAD_REQUEST)
                                } else {
                                    None
                                };
                                let response = if let Some(status) = denied {
                                    Response::builder()
                                        .status(status)
                                        .body(Body::empty())
                                        .unwrap()
                                } else {
                                    if matches!(
                                        request.method().as_str(),
                                        "PUT" | "MKCOL" | "DELETE" | "MOVE"
                                    ) {
                                        if let Some(cache) = &metadata {
                                            cache.invalidate();
                                        }
                                        let response = crate::mount_write::handle(
                                            writer.unwrap(),
                                            request,
                                            address,
                                        )
                                        .await;
                                        if let Some(cache) = &metadata {
                                            cache.invalidate();
                                        }
                                        return Ok::<_, Infallible>(response);
                                    }
                                    match tokio::time::timeout(
                                        Duration::from_secs(330),
                                        handler.handle(request.map(|body| {
                                            http_body_util::Limited::new(body, 65536).map_err(
                                                |_| std::io::Error::other("invalid request body"),
                                            )
                                        })),
                                    )
                                    .await
                                    {
                                        Ok(response) => response,
                                        Err(_) => Response::builder()
                                            .status(504)
                                            .body(Body::empty())
                                            .unwrap(),
                                    }
                                };
                                Ok::<_, Infallible>(response)
                            }
                        });
                        let _ = hyper::server::conn::http1::Builder::new()
                            .max_headers(32)
                            .timer(TokioTimer::new())
                            .header_read_timeout(Duration::from_secs(10))
                            .serve_connection(TokioIo::new(socket), service)
                            .await;
                    });
                }
            });
            runtime.shutdown_timeout(Duration::from_secs(1));
        });
        Ok(Self {
            address,
            token,
            stop,
            thread: Some(thread),
        })
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
#[path = "readonly_mount_tests.rs"]
mod tests;
