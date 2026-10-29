//! A private loopback filesystem bridge over the existing Provider
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
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    convert::Infallible,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    net::SocketAddr,
    path::Path,
    sync::{
        Arc, Condvar, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use tokio::{net::TcpListener, sync::Semaphore};

type DirectoryEntries = Arc<BTreeMap<String, MetadataEntry>>;
type DownloadKey = (String, String, String, u64, bool);
type DownloadGates = HashMap<DownloadKey, Weak<Mutex<()>>>;
const DIRECTORY_FRESH: Duration = Duration::from_secs(30);
const DIRECTORY_MAX_AGE: Duration = Duration::from_secs(300);
const DIRECTORY_RETRY: Duration = Duration::from_secs(10);
const DIRECTORY_RESPONSE_LIMIT: usize = 16 * 1024 * 1024;
const DIRECTORY_CACHE_LIMIT: usize = 64;
const QUOTA_FRESH: Duration = Duration::from_secs(300);
const QUOTA_MAX_AGE: Duration = Duration::from_secs(1800);
const QUOTA_RETRY: Duration = Duration::from_secs(30);
tokio::task_local! {
    static DISPLAY_METADATA_ONLY: bool;
    static PREPARED_DIRECTORY: Option<(String, DirectoryEntries)>;
    static LIVE_READ_METADATA: RefCell<BTreeMap<String, MetadataEntry>>;
}

struct Cached {
    meta: MetadataEntry,
    file: tempfile::NamedTempFile,
}
struct QuotaSample {
    checked: Instant,
    value: (u64, u64),
}
#[derive(Default)]
struct QuotaCache {
    sample: Option<QuotaSample>,
    attempted: Option<Instant>,
    pending: bool,
}
struct DirectorySample {
    path: String,
    checked: Instant,
    attempted: Instant,
    entries: DirectoryEntries,
}
#[derive(Default)]
struct DirectoryCache {
    samples: VecDeque<DirectorySample>,
    pending: HashSet<String>,
    generation: u64,
}
struct Inner {
    backend: Backend,
    writer: Option<Arc<crate::mount_write::Writer>>,
    cache: Mutex<VecDeque<Arc<Cached>>>,
    downloads: Mutex<DownloadGates>,
    quota: Mutex<QuotaCache>,
    quota_refresh: mpsc::SyncSender<()>,
    directories: Mutex<DirectoryCache>,
    directory_ready: Condvar,
    refresh: mpsc::SyncSender<String>,
    directory: tempfile::TempDir,
    stop: Arc<AtomicBool>,
}
#[derive(Clone)]
struct RemoteFs(Arc<Inner>);
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
        Ok(())
    }
    fn quota_snapshot(&self) -> FsResult<(u64, Option<u64>)> {
        self.check().map_err(|_| FsError::GeneralFailure)?;
        let mut cache = self.0.quota.lock().map_err(|_| FsError::GeneralFailure)?;
        let fresh = cache
            .sample
            .as_ref()
            .is_some_and(|sample| sample.checked.elapsed() < QUOTA_FRESH);
        let retry = cache
            .attempted
            .is_none_or(|attempted| attempted.elapsed() >= QUOTA_RETRY);
        if !fresh && retry && !cache.pending {
            cache.pending = true;
            if self.0.quota_refresh.try_send(()).is_err() {
                cache.pending = false;
            }
        }
        // Statfs also runs for type/remote classification and can block a file
        // manager before it displays an already cached listing. Never perform
        // account/network IO here, including on the first query. Unknown stays
        // unknown; an old successful display value has a bounded grace period.
        cache
            .sample
            .as_ref()
            .filter(|sample| sample.checked.elapsed() < QUOTA_MAX_AGE)
            .map(|sample| (sample.value.0, Some(sample.value.1)))
            .ok_or(FsError::NotImplemented)
    }
    fn refresh_quota(&self) {
        let value = self
            .check()
            .and_then(|()| self.0.backend.storage_usage(&mut || self.check()))
            .ok()
            .flatten()
            .and_then(|usage| usage.used.zip(usage.total));
        if let Ok(mut cache) = self.0.quota.lock() {
            cache.pending = false;
            cache.attempted = Some(Instant::now());
            if let Some(value) = value.filter(|_| self.check().is_ok()) {
                cache.sample = Some(QuotaSample {
                    checked: Instant::now(),
                    value,
                });
            }
        }
    }
    fn entry(&self, path: &str) -> FsResult<MetadataEntry> {
        self.check().map_err(|_| FsError::GeneralFailure)?;
        let entry = self
            .0
            .backend
            .get_metadata_by_path(path)
            .map_err(|_| FsError::GeneralFailure)?
            .ok_or(FsError::NotFound)?;
        if let Some(writer) = &self.0.writer {
            writer
                .observe(&entry)
                .map_err(|_| FsError::GeneralFailure)?;
        }
        Ok(entry)
    }
    fn metadata_entry(&self, path: &str) -> FsResult<MetadataEntry> {
        self.check().map_err(|_| FsError::GeneralFailure)?;
        // A real GET must use live metadata before response headers, including
        // zero-byte files and conditional/range requests. Display caches are
        // only for HEAD/PROPFIND, never download validity or authentication.
        if !DISPLAY_METADATA_ONLY
            .try_with(|value| *value)
            .unwrap_or(false)
        {
            // dav-server can ask for metadata and then open the same path in
            // one GET. Reuse only that request's live lookup, never a display
            // snapshot or another request's validation.
            if let Ok(Some(entry)) =
                LIVE_READ_METADATA.try_with(|entries| entries.borrow().get(path).cloned())
            {
                return Ok(entry);
            }
            let entry = self.entry(path)?;
            let _ = LIVE_READ_METADATA.try_with(|entries| {
                entries.borrow_mut().insert(path.into(), entry.clone());
            });
            return Ok(entry);
        }
        let parent = path
            .rsplit_once('/')
            .map(|(p, _)| if p.is_empty() { "/" } else { p });
        if path != "/" {
            let mut directories = self
                .0
                .directories
                .lock()
                .map_err(|_| FsError::GeneralFailure)?;
            directories
                .samples
                .retain(|sample| sample.checked.elapsed() < DIRECTORY_MAX_AGE);
            if let Some(index) = directories
                .samples
                .iter()
                .position(|sample| Some(sample.path.as_str()) == parent)
            {
                // Only a complete, validated shallow listing can prove that
                // a child (including file-manager probes) does not exist.
                // Child-count/attribute probes actively use their parent too;
                // keep it hot instead of evicting it after visiting siblings.
                let sample = directories.samples.remove(index).unwrap();
                let entry = sample.entries.get(path).cloned().ok_or(FsError::NotFound);
                directories.samples.push_front(sample);
                return entry;
            }
        }
        self.entry(path)
    }
    fn validated_entries(path: &str, entries: Vec<MetadataEntry>) -> FsResult<DirectoryEntries> {
        if entries.len() > 100000 {
            return Err(FsError::GeneralFailure);
        }
        let mut result = BTreeMap::new();
        for entry in entries {
            if valid_path(&entry.path).is_err()
                || entry.parent_path != path
                || entry.path == path
                || entry.name.is_empty()
                || entry.name.contains('/')
                || entry.path != format!("{}/{}", path.trim_end_matches('/'), entry.name)
                || result.contains_key(&entry.path)
            {
                return Err(FsError::GeneralFailure);
            }
            result.insert(entry.path.clone(), entry);
        }
        Ok(Arc::new(result))
    }
    fn remember(cache: &mut DirectoryCache, path: String, entries: DirectoryEntries) {
        cache
            .samples
            .retain(|s| s.path != path && s.checked.elapsed() < DIRECTORY_MAX_AGE);
        cache.samples.push_front(DirectorySample {
            path,
            checked: Instant::now(),
            attempted: Instant::now(),
            entries,
        });
        while cache.samples.len() > 1
            && (cache.samples.len() > DIRECTORY_CACHE_LIMIT
                || cache.samples.iter().map(|s| s.entries.len()).sum::<usize>() > 100000)
        {
            cache.samples.pop_back();
        }
    }
    fn invalidate_display(&self) {
        if let Ok(mut cache) = self.0.directories.lock() {
            cache.generation = cache.generation.wrapping_add(1);
            cache.samples.clear();
            self.0.directory_ready.notify_all();
        }
    }
    fn fetch_directory(&self, path: &str) -> FsResult<DirectoryEntries> {
        let generation = self
            .0
            .directories
            .lock()
            .map_err(|_| FsError::GeneralFailure)?
            .generation;
        let mut result = self
            .check()
            .map_err(|_| FsError::GeneralFailure)
            .and_then(|_| {
                self.0
                    .backend
                    .list_directory_checked(path, &mut || self.check())
                    .map_err(|_| FsError::GeneralFailure)
            })
            .and_then(|entries| Self::validated_entries(path, entries));
        let mut cache = self
            .0
            .directories
            .lock()
            .map_err(|_| FsError::GeneralFailure)?;
        if cache.generation != generation || self.0.stop.load(Ordering::Relaxed) {
            // A mutation can finish while a background listing is in flight.
            // Never republish its pre-mutation snapshot, including on failure.
            result = Err(FsError::GeneralFailure);
        }
        if let Some(entries) = result.as_ref().ok().cloned() {
            let observed = self.0.writer.as_ref().map_or(Ok(()), |writer| {
                // First observed version remains conservative; every mutation
                // compares it to live metadata before changing the remote.
                writer.observe_many(&entries.values().cloned().collect::<Vec<_>>())
            });
            if observed.is_ok() {
                Self::remember(&mut cache, path.into(), entries);
            } else {
                result = Err(FsError::GeneralFailure);
            }
        }
        // A failed refresh never replaces the last complete snapshot with [];
        // a genuinely successful empty listing still replaces it normally.
        cache.pending.remove(path);
        self.0.directory_ready.notify_all();
        result
    }
    fn directory_snapshot(&self, path: &str) -> FsResult<DirectoryEntries> {
        loop {
            self.check().map_err(|_| FsError::GeneralFailure)?;
            let mut cache = self
                .0
                .directories
                .lock()
                .map_err(|_| FsError::GeneralFailure)?;
            cache
                .samples
                .retain(|s| s.checked.elapsed() < DIRECTORY_MAX_AGE);
            if let Some(index) = cache.samples.iter().position(|s| s.path == path) {
                let sample = &cache.samples[index];
                let entries = sample.entries.clone();
                let refresh = sample.checked.elapsed() >= DIRECTORY_FRESH
                    && sample.attempted.elapsed() >= DIRECTORY_RETRY
                    && cache.pending.is_empty();
                if refresh {
                    cache.samples[index].attempted = Instant::now();
                    cache.pending.insert(path.into());
                    if self.0.refresh.try_send(path.into()).is_err() {
                        cache.pending.remove(path);
                    }
                }
                // A successful display hit is a use, not merely an insertion
                // timestamp. Background child probes must not age it out.
                let sample = cache.samples.remove(index).unwrap();
                cache.samples.push_front(sample);
                return Ok(entries);
            }
            if cache.pending.contains(path) {
                let _ = self
                    .0
                    .directory_ready
                    .wait_timeout(cache, Duration::from_millis(50))
                    .map_err(|_| FsError::GeneralFailure)?;
                continue;
            }
            cache.pending.insert(path.into());
            drop(cache);
            return self.fetch_directory(path);
        }
    }
    fn materialize(&self, entry: MetadataEntry) -> anyhow::Result<Arc<Cached>> {
        // Coalesce one object/version, without holding the shared cache lock
        // across a remote download. A slow preview must not stall other files.
        let gate = {
            let mut downloads = self
                .0
                .downloads
                .lock()
                .map_err(|_| anyhow::anyhow!("download gates unavailable"))?;
            downloads.retain(|_, gate| gate.strong_count() > 0);
            let key = (
                entry.remote_id.clone(),
                entry.path.clone(),
                entry.etag.clone(),
                entry.size,
                entry.is_dir,
            );
            match downloads.get(&key).and_then(Weak::upgrade) {
                Some(gate) => gate,
                None => {
                    let gate = Arc::new(Mutex::new(()));
                    downloads.insert(key, Arc::downgrade(&gate));
                    gate
                }
            }
        };
        let _flight = gate
            .lock()
            .map_err(|_| anyhow::anyhow!("download unavailable"))?;
        let mut cache = self
            .0
            .cache
            .lock()
            .map_err(|_| anyhow::anyhow!("cache unavailable"))?;
        self.check()?;
        if let Some(i) = cache.iter().position(|item| same(&item.meta, &entry)) {
            let item = cache.remove(i).unwrap();
            cache.push_front(item.clone());
            return Ok(item);
        }
        drop(cache);
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
    meta: MetadataEntry,
    _cached: Option<Arc<Cached>>,
    file: Option<File>,
    position: u64,
}
impl std::fmt::Debug for RemoteFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReadOnlyMountFile")
    }
}
impl DavFile for RemoteFile {
    fn metadata(&mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        Box::pin(std::future::ready(Ok(
            Box::new(Meta(self.meta.clone())) as _
        )))
    }
    fn read_bytes(&mut self, count: usize) -> FsFuture<'_, Bytes> {
        blocking(move || {
            if count == 0 {
                return Ok(Bytes::new());
            }
            let mut buffer = vec![0; count.min(65536)];
            let done = self
                .file
                .as_mut()
                .ok_or(FsError::GeneralFailure)?
                .read(&mut buffer)
                .map_err(|_| FsError::GeneralFailure)?;
            self.position += done as u64;
            buffer.truncate(done);
            Ok(buffer.into())
        })
    }
    fn seek(&mut self, at: SeekFrom) -> FsFuture<'_, u64> {
        blocking(move || {
            let position = match at {
                SeekFrom::Start(value) => i128::from(value),
                SeekFrom::Current(value) => i128::from(self.position) + i128::from(value),
                SeekFrom::End(value) => i128::from(self.meta.size) + i128::from(value),
            };
            let position = u64::try_from(position).map_err(|_| FsError::GeneralFailure)?;
            if let Some(file) = &mut self.file {
                file.seek(SeekFrom::Start(position))
                    .map_err(|_| FsError::GeneralFailure)?;
            }
            self.position = position;
            Ok(position)
        })
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
        blocking(|| self.quota_snapshot())
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
            let entry = self.metadata_entry(&resource(dav)?)?;
            if entry.is_dir {
                return Err(FsError::Forbidden);
            }
            let metadata_only = DISPLAY_METADATA_ONLY
                .try_with(|value| *value)
                .unwrap_or(false);
            let (cached, file) = if metadata_only {
                (None, None)
            } else {
                // Validate a real GET before publishing response headers.
                // Reopen keeps concurrent HTTP range cursors independent.
                let cached = self
                    .materialize(entry.clone())
                    .map_err(|_| FsError::GeneralFailure)?;
                let file = cached.file.reopen().map_err(|_| FsError::GeneralFailure)?;
                (Some(cached), Some(file))
            };
            Ok(Box::new(RemoteFile {
                meta: entry,
                _cached: cached,
                file,
                position: 0,
            }) as _)
        })
    }
    fn read_dir<'a>(
        &'a self,
        dav: &'a DavPath,
        _: ReadDirMeta,
    ) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
        blocking(move || {
            let path = resource(dav)?;
            // The HTTP preflight pins one validated listing for this request,
            // including while dav-server lazily renders its response body.
            let prepared = PREPARED_DIRECTORY
                .try_with(|prepared| {
                    prepared
                        .as_ref()
                        .filter(|(p, _)| p == &path)
                        .map(|(_, entries)| entries.clone())
                })
                .ok()
                .flatten();
            let entries = match prepared {
                Some(entries) => entries,
                None => self.directory_snapshot(&path)?,
            };
            let result: Vec<FsResult<Box<dyn DavDirEntry>>> = entries
                .values()
                .cloned()
                .map(|entry| Ok(Box::new(Entry(Meta(entry))) as _))
                .collect();
            Ok(Box::pin(futures_util::stream::iter(result)) as _)
        })
    }
    fn metadata<'a>(&'a self, dav: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
        blocking(move || Ok(Box::new(Meta(self.metadata_entry(&resource(dav)?)?)) as _))
    }
}

fn empty_response(status: StatusCode) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(Body::empty())
        .unwrap()
}
async fn handle_read(
    fs: RemoteFs,
    handler: DavHandler,
    request: Request<Incoming>,
) -> Response<Body> {
    let propfind = request.method().as_str() == "PROPFIND";
    let metadata_only = matches!(request.method().as_str(), "HEAD" | "PROPFIND" | "OPTIONS");
    let response = DISPLAY_METADATA_ONLY.scope(metadata_only, async move {
        let prepared = if propfind && request.headers().get("Depth").is_some_and(|v| v == "1") {
            let path = match DavPath::from_uri(request.uri())
                .map_err(|_| FsError::Forbidden)
                .and_then(|p| resource(&p))
            {
                Ok(path) => path,
                Err(_) => return empty_response(StatusCode::BAD_REQUEST),
            };
            let meta = match tokio::task::block_in_place(|| fs.metadata_entry(&path)) {
                Ok(meta) => meta,
                Err(FsError::NotFound) => return empty_response(StatusCode::NOT_FOUND),
                Err(_) => return empty_response(StatusCode::SERVICE_UNAVAILABLE),
            };
            if meta.is_dir {
                match tokio::task::block_in_place(|| fs.directory_snapshot(&path)) {
                    Ok(entries) => Some((path, entries)),
                    Err(_) => return empty_response(StatusCode::SERVICE_UNAVAILABLE),
                }
            } else {
                None
            }
        } else {
            None
        };
        PREPARED_DIRECTORY
            .scope(prepared, async move {
                let response = handler
                    .handle(request.map(|body| {
                        http_body_util::Limited::new(body, 65536)
                            .map_err(|_| std::io::Error::other("invalid request body"))
                    }))
                    .await;
                if !propfind {
                    return response;
                }
                // dav-server renders PROPFIND lazily, outside handle(). Render under
                // the same display context before committing success, with a bound.
                let (parts, body) = response.into_parts();
                match http_body_util::Limited::new(body, DIRECTORY_RESPONSE_LIMIT)
                    .collect()
                    .await
                {
                    Ok(body) => Response::from_parts(parts, Body::from(body.to_bytes())),
                    Err(_) => empty_response(StatusCode::BAD_GATEWAY),
                }
            })
            .await
    });
    LIVE_READ_METADATA.scope(RefCell::default(), response).await
}

pub struct Bridge {
    pub address: SocketAddr,
    pub token: String,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    refresh_thread: Option<thread::JoinHandle<()>>,
    quota_thread: Option<thread::JoinHandle<()>>,
    #[cfg(test)]
    fs: RemoteFs,
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
        let mut directories = DirectoryCache::default();
        if let Some((checked, entries)) = backend
            .initial_directory_listing()
            .filter(|(checked, _)| checked.elapsed() < DIRECTORY_MAX_AGE)
        {
            let entries = RemoteFs::validated_entries("/", entries)
                .map_err(|_| anyhow::anyhow!("invalid initial directory listing"))?;
            if let Some(writer) = &writer {
                writer.observe_many(&entries.values().cloned().collect::<Vec<_>>())?;
            }
            RemoteFs::remember(&mut directories, "/".into(), entries);
            directories.samples.front_mut().unwrap().checked = checked;
            directories.samples.front_mut().unwrap().attempted = checked;
        }
        let (send, receive) = mpsc::sync_channel(1);
        let (quota_send, quota_receive) = mpsc::sync_channel(1);
        let fs = RemoteFs(Arc::new(Inner {
            backend,
            writer: writer.clone(),
            cache: Mutex::default(),
            downloads: Mutex::default(),
            quota: Mutex::default(),
            quota_refresh: quota_send,
            directories: Mutex::new(directories),
            directory_ready: Condvar::new(),
            refresh: send,
            directory,
            stop: stop.clone(),
        }));
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
        let refresh_fs = fs.clone();
        let refresh_thread = thread::spawn(move || {
            while !refresh_fs.0.stop.load(Ordering::Relaxed) {
                if let Ok(path) = receive.recv_timeout(Duration::from_millis(50)) {
                    let _ = refresh_fs.fetch_directory(&path);
                }
            }
        });
        let quota_fs = fs.clone();
        let quota_thread = thread::spawn(move || {
            while !quota_fs.0.stop.load(Ordering::Relaxed) {
                if quota_receive
                    .recv_timeout(Duration::from_millis(50))
                    .is_ok()
                {
                    quota_fs.refresh_quota();
                }
            }
        });
        // Warm asynchronously so the usual first native Statfs can report
        // capacity, while a slow/unavailable provider never delays mounting.
        let _ = fs.quota_snapshot();
        #[cfg(test)]
        let test_fs = fs.clone();
        let thread = thread::spawn(move || {
            runtime.block_on(async move {
                let Ok(listener) = TcpListener::from_std(listener) else {
                    return;
                };
                let handler = DavHandler::builder()
                    .filesystem(Box::new(fs.clone()))
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
                    let fs = fs.clone();
                    let auth = auth.clone();
                    let writer = writer.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        let service = service_fn(move |request: Request<Incoming>| {
                            let handler = handler.clone();
                            let fs = fs.clone();
                            let auth = auth.clone();
                            let writer = writer.clone();
                            async move {
                                let actual = request
                                    .headers()
                                    .get("Authorization")
                                    .map(|v| v.as_bytes())
                                    .unwrap_or_default();
                                let denied = if !bool::from(actual.ct_eq(auth.as_bytes())) {
                                    Some(StatusCode::UNAUTHORIZED)
                                } else if !(matches!(
                                    request.method().as_str(),
                                    "GET" | "HEAD" | "OPTIONS" | "PROPFIND"
                                ) || writer.is_some()
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
                                        let response = crate::mount_write::handle(
                                            writer.unwrap(),
                                            request,
                                            address,
                                        )
                                        .await;
                                        // Invalidate on errors too: an upstream operation may
                                        // have completed before its acknowledgement failed.
                                        fs.invalidate_display();
                                        return Ok::<_, Infallible>(response);
                                    }
                                    match tokio::time::timeout(
                                        Duration::from_secs(330),
                                        handle_read(fs, handler, request),
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
            refresh_thread: Some(refresh_thread),
            quota_thread: Some(quota_thread),
            #[cfg(test)]
            fs: test_fs,
        })
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.quota_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.refresh_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
#[path = "readonly_mount_tests.rs"]
mod tests;
