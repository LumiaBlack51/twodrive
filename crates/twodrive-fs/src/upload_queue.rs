use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use twodrive_backend::CloudBackend;
use twodrive_core::{Database, FileState};

use crate::activity::ActivityGuard;
use crate::hydration::hydrate_record;
use crate::locks::item_sync_lock;
use crate::recovery::{
    recover_dirty_record, recover_pending_delete_id, recover_pending_metadata_record,
};

#[derive(Clone)]
pub(crate) enum UploadCommand {
    Upload(String),
    Delete(String),
    Shutdown,
}

#[derive(Clone)]
pub(crate) struct UploadQueue {
    pub(crate) sender: Sender<UploadCommand>,
    pub(crate) pending: Arc<Mutex<HashMap<String, bool>>>,
}

impl UploadQueue {
    pub(crate) fn send(&self, command: UploadCommand) -> anyhow::Result<()> {
        let key = match &command {
            UploadCommand::Upload(id) => Some(format!("upload:{id}")),
            UploadCommand::Delete(id) => Some(format!("delete:{id}")),
            UploadCommand::Shutdown => None,
        };
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| anyhow::anyhow!("upload queue poisoned"))?;
        if let Some(key) = &key {
            if let Some(rerun) = pending.get_mut(key) {
                *rerun = true;
                return Ok(());
            }
            pending.insert(key.clone(), false);
        }
        if self.sender.send(command).is_err() {
            if let Some(key) = key {
                pending.remove(&key);
            }
            anyhow::bail!("upload queue closed");
        }
        Ok(())
    }
}

pub(crate) struct UploadRun {
    pub(crate) queue: UploadQueue,
    pub(crate) command: UploadCommand,
    pub(crate) key: String,
}

impl UploadRun {
    pub(crate) fn start(queue: &UploadQueue, command: &UploadCommand) -> Option<Self> {
        let key = match command {
            UploadCommand::Upload(id) => format!("upload:{id}"),
            UploadCommand::Delete(id) => format!("delete:{id}"),
            UploadCommand::Shutdown => return None,
        };
        if let Ok(mut pending) = queue.pending.lock() {
            pending.insert(key.clone(), false);
        }
        Some(Self {
            queue: queue.clone(),
            command: command.clone(),
            key,
        })
    }
}

impl Drop for UploadRun {
    fn drop(&mut self) {
        let rerun = self
            .queue
            .pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(&self.key))
            .unwrap_or(false);
        if rerun {
            let _ = self.queue.send(self.command.clone());
        }
    }
}

pub(crate) struct UploadPool<B: CloudBackend> {
    pub(crate) sender: UploadQueue,
    pub(crate) workers: Vec<JoinHandle<()>>,
    pub(crate) _backend: std::marker::PhantomData<B>,
}

impl<B: CloudBackend> UploadPool<B> {
    pub(crate) fn new(
        db: Database,
        cache_dir: PathBuf,
        backend: Arc<B>,
        concurrency: usize,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        let receiver = Arc::new(Mutex::new(receiver));
        let sender = UploadQueue {
            sender,
            pending: Arc::new(Mutex::new(HashMap::new())),
        };
        let mut workers = Vec::new();
        for _ in 0..concurrency.max(1) {
            let db = db.clone();
            let cache_dir = cache_dir.clone();
            let backend = Arc::clone(&backend);
            let receiver = Arc::clone(&receiver);
            let queue = sender.clone();
            workers.push(thread::spawn(move || {
                run_upload_worker(db, cache_dir, backend, receiver, queue)
            }));
        }
        Self {
            sender,
            workers,
            _backend: std::marker::PhantomData,
        }
    }

    pub(crate) fn enqueue(&self, remote_id: String) -> anyhow::Result<()> {
        self.sender
            .send(UploadCommand::Upload(remote_id))
            .map_err(|_| anyhow::anyhow!("upload worker queue is closed"))
    }

    pub(crate) fn enqueue_delete(&self, cloud_remote_id: String) -> anyhow::Result<()> {
        self.sender
            .send(UploadCommand::Delete(cloud_remote_id))
            .map_err(|_| anyhow::anyhow!("sync worker queue is closed"))
    }
}

impl<B: CloudBackend> Drop for UploadPool<B> {
    fn drop(&mut self) {
        for _ in &self.workers {
            let _ = self.sender.send(UploadCommand::Shutdown);
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

pub(crate) fn run_upload_worker<B: CloudBackend>(
    db: Database,
    cache_dir: PathBuf,
    backend: Arc<B>,
    receiver: Arc<Mutex<Receiver<UploadCommand>>>,
    queue: UploadQueue,
) {
    loop {
        let command = match receiver.lock() {
            Ok(receiver) => receiver.recv(),
            Err(_) => return,
        };
        let Ok(command) = command else {
            return;
        };
        let _running = UploadRun::start(&queue, &command);
        let remote_id = match command {
            UploadCommand::Upload(remote_id) => remote_id,
            UploadCommand::Delete(cloud_remote_id) => {
                if let Err(err) = recover_pending_delete_id(&db, backend.as_ref(), &cloud_remote_id)
                {
                    eprintln!("twodrive: queued delete failed for {cloud_remote_id}: {err:#}");
                }
                continue;
            }
            UploadCommand::Shutdown => return,
        };
        let item_lock = item_sync_lock(&remote_id);
        let Ok(_item_guard) = item_lock.lock() else {
            continue;
        };
        if let Ok(Some(operation)) = db.pending_metadata_operation(&remote_id)
            && let Err(err) = recover_pending_metadata_record(&db, backend.as_ref(), &operation)
        {
            eprintln!(
                "twodrive: queued metadata operation failed for {}: {err:#}",
                operation.path
            );
            continue;
        }
        if let Ok(Some(record)) = db.get_by_remote_id(&remote_id)
            && !record.metadata.is_dir
            && record.effective_pinned()
            && !record.cache_path.as_deref().is_some_and(Path::exists)
            && let Err(err) = hydrate_record(&db, &cache_dir, backend.as_ref(), &record)
        {
            eprintln!("twodrive: queued pinned hydration failed for {remote_id}: {err:#}");
        }
        let record = match db.get_by_remote_id(&remote_id) {
            Ok(Some(record))
                if matches!(
                    record.state,
                    FileState::Dirty | FileState::Uploading | FileState::Conflict
                ) =>
            {
                record
            }
            Ok(_) => continue,
            Err(err) => {
                eprintln!("twodrive: queued upload lookup failed for {remote_id}: {err:#}");
                continue;
            }
        };
        let mut activity = ActivityGuard::start(
            &cache_dir,
            "upload",
            &record.metadata.path,
            &record.metadata.name,
            Some(record.metadata.size),
        );
        if let Err(err) = recover_dirty_record(&db, backend.as_ref(), record, &mut |done, total| {
            activity.set_progress(done, Some(total));
            Ok(())
        }) {
            eprintln!("twodrive: queued upload failed for {remote_id}: {err:#}");
        }
        activity.finish();
    }
}
