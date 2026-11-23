use crate::hydration::hydrate_version;
use fuser::ReplyEmpty;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use twodrive_backend::CloudBackend;
use twodrive_core::{Database, FileRecord};

#[derive(Clone, Copy)]
pub(crate) enum Action {
    Pin,
    Unpin,
    Release,
}

type Command = (FileRecord, Action, ReplyEmpty);

pub(crate) struct CacheControls {
    commands: Option<SyncSender<Command>>,
    stopping: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}

impl CacheControls {
    pub(crate) fn new<B: CloudBackend>(db: Database, cache: PathBuf, backend: Arc<B>) -> Self {
        let (commands, rx) = mpsc::sync_channel::<Command>(32);
        let (wake, notifications) = mpsc::sync_channel(1);
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = stopping.clone();
        let reader = db.clone();
        let hydration = thread::spawn(move || {
            loop {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                if let Ok(ids) = reader.pending_pin_downloads() {
                    for id in ids {
                        if stop.load(Ordering::Acquire) {
                            return;
                        }
                        let result = (|| -> anyhow::Result<()> {
                            let generation = reader.download_generation(&id)?;
                            if let Some(record) = reader.get_by_remote_id(&id)? {
                                hydrate_version(
                                    &reader,
                                    &cache,
                                    backend.as_ref(),
                                    &record,
                                    generation,
                                    true,
                                )?;
                            }
                            Ok(())
                        })();
                        if result.is_err() {
                            eprintln!("twodrive: pending pin deferred");
                        }
                    }
                }
                if matches!(
                    notifications.recv_timeout(Duration::from_secs(30)),
                    Err(mpsc::RecvTimeoutError::Disconnected)
                ) {
                    break;
                }
            }
        });
        let policy = thread::spawn(move || {
            for (record, action, reply) in rx {
                let result = match action {
                    Action::Release => db.release_selected(record).map(|_| ()),
                    Action::Unpin => db.set_explicit_pin(&record.metadata.remote_id, false),
                    Action::Pin => {
                        db.set_explicit_pin(&record.metadata.remote_id, true)
                            .map(|()| {
                                let _ = wake.try_send(());
                            })
                    }
                };
                if result.is_ok() {
                    reply.ok();
                } else {
                    reply.error(libc::EIO);
                }
            }
        });
        Self {
            commands: Some(commands),
            stopping,
            workers: vec![policy, hydration],
        }
    }

    pub(crate) fn submit(&self, record: FileRecord, action: Action, reply: ReplyEmpty) {
        if let Err(error) = self
            .commands
            .as_ref()
            .unwrap()
            .try_send((record, action, reply))
        {
            match error {
                mpsc::TrySendError::Full((_, _, reply)) => reply.error(libc::EAGAIN),
                mpsc::TrySendError::Disconnected((_, _, reply)) => reply.error(libc::EIO),
            }
        }
    }
}

impl Drop for CacheControls {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        self.commands.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
