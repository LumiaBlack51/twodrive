//! Shallow complete metadata refreshes; cached reads never wait for metadata IO.
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};
use twodrive_backend::CloudBackend;
use twodrive_core::Database;
pub(crate) struct DirectoryRefresh<B: CloudBackend> {
    db: Database,
    backend: Arc<B>,
    refreshed: Mutex<HashMap<String, Instant>>,
    gates: Mutex<HashMap<String, Weak<Mutex<()>>>>,
    pending: Mutex<HashSet<String>>,
}
impl<B: CloudBackend> DirectoryRefresh<B> {
    pub(crate) fn new(db: Database, backend: Arc<B>) -> Self {
        Self {
            db,
            backend,
            refreshed: Mutex::default(),
            gates: Mutex::default(),
            pending: Mutex::default(),
        }
    }
    pub(crate) fn fresh(&self, path: &str) -> bool {
        self.refreshed
            .lock()
            .unwrap()
            .get(path)
            .is_some_and(|at| at.elapsed() < Duration::from_secs(15))
    }
    pub(crate) fn schedule(self: &Arc<Self>, pool: &crate::read_pool::ReadPool, path: String) {
        if !self.pending.lock().unwrap().insert(path.clone()) {
            return;
        }
        let owner = self.clone();
        pool.spawn(move || {
            let _ = owner.refresh(&path);
            owner.pending.lock().unwrap().remove(&path);
        });
    }
    pub(crate) fn refresh(&self, path: &str) -> anyhow::Result<()> {
        let gate = {
            let mut gates = self.gates.lock().unwrap();
            gates.retain(|_, gate| gate.strong_count() > 0);
            let gate = gates.get(path).and_then(Weak::upgrade).unwrap_or_default();
            gates.insert(path.into(), Arc::downgrade(&gate));
            gate
        };
        let _guard = gate.lock().unwrap();
        if self.fresh(path) {
            return Ok(());
        }
        let entries = self.backend.directory_metadata(path)?;
        self.db.reconcile_directory_metadata(path, &entries)?;
        let mut refreshed = self.refreshed.lock().unwrap();
        refreshed.retain(|_, at| at.elapsed() < Duration::from_secs(60));
        if refreshed.len() >= 4096 {
            refreshed.clear();
        }
        refreshed.insert(path.into(), Instant::now());
        Ok(())
    }
}
