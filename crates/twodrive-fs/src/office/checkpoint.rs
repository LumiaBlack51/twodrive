//! A bounded hint queue. SQLite's normal autocheckpoint remains the fallback
//! when the worker, disk or another process is busy. Hints never own save data.
use rusqlite::Connection;
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};

pub(super) const HINT_WRITES: u32 = 192;

struct Map {
    db: Connection,
    directory: File,
}
type Handle = Mutex<Option<Map>>;

fn queue() -> Option<&'static mpsc::SyncSender<Weak<Handle>>> {
    static QUEUE: OnceLock<Option<mpsc::SyncSender<Weak<Handle>>>> = OnceLock::new();
    QUEUE
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel::<Weak<Handle>>(1);
            std::thread::Builder::new()
                .name("office-checkpoint".into())
                .spawn(move || {
                    for hint in receiver {
                        let Some(handle) = hint.upgrade() else {
                            continue;
                        };
                        let Ok(slot) = handle.lock() else { continue };
                        if let Some(map) = slot.as_ref()
                            && map.directory.try_lock().is_ok()
                        {
                            // PASSIVE does not wait for a writer or a reader. A foreground
                            // sync takes the same directory lock before its FULL checkpoint.
                            let _ = map.db.execute_batch("PRAGMA wal_checkpoint(PASSIVE)");
                            let _ = map.directory.unlock();
                        }
                    }
                })
                .ok()
                .map(|_| sender)
        })
        .as_ref()
}

#[derive(Default)]
pub(super) struct Checkpoints {
    handle: Option<Arc<Handle>>,
    writes: u32,
}

impl Checkpoints {
    pub fn note(&mut self, db: &Connection) {
        // One hint per batch. Each typical reference updates two small pages.
        self.writes += 1;
        if self.writes < HINT_WRITES {
            return;
        }
        self.writes = 0;
        let Some(sender) = queue() else { return };
        if self.handle.is_none() {
            let Some(path) = db.path().map(Path::new) else {
                return;
            };
            let Some(parent) = path.parent() else { return };
            let Ok(directory) = File::open(parent) else {
                return;
            };
            let Ok(connection) = super::conn(path) else {
                return;
            };
            // Once the worker is available, allow headroom for overlap. Keep a
            // synchronous fallback even if the worker queue is busy, and leave
            // the larger budget for changed media (or a disabled pragma) intact.
            if let (Ok(page), Ok(pages)) = (
                db.pragma_query_value(None, "page_size", |r| r.get::<_, u32>(0)),
                db.pragma_query_value(None, "wal_autocheckpoint", |r| r.get::<_, u32>(0)),
            ) && pages.saturating_mul(page) == super::CHECKPOINT_BYTES
            {
                let _ = db.pragma_update(None, "wal_autocheckpoint", (1024 * 1024) / page);
            }
            self.handle = Some(Arc::new(Mutex::new(Some(Map {
                db: connection,
                directory,
            }))));
        }
        if let Some(handle) = &self.handle {
            let _ = sender.try_send(Arc::downgrade(handle));
        }
    }
}
impl Drop for Checkpoints {
    fn drop(&mut self) {
        // Retire the connection before the carrier's open-handle protection ends.
        if let Some(handle) = self.handle.take() {
            handle.lock().unwrap_or_else(|e| e.into_inner()).take();
        }
    }
}
