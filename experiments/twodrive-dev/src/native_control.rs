//! Cooperative cancellation at object/transaction boundaries. A cancelled
//! inventory is an error, never a partial namespace or proof of absence.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Default)]
pub struct NativeControl(Arc<Inner>);
#[derive(Default)]
struct Inner {
    stopping: AtomicBool,
    progress: Mutex<Progress>,
}
#[derive(Clone, Copy, Default, serde::Serialize)]
pub struct Progress {
    pub checkpoint: &'static str,
    pub checkpoints: u64,
}
#[derive(Debug, thiserror::Error)]
#[error("native runtime stopping at a recoverable boundary")]
pub struct NativeStopping;

impl NativeControl {
    pub fn request_stop(&self) {
        self.0.stopping.store(true, Ordering::SeqCst);
    }
    pub fn stopping(&self) -> bool {
        self.0.stopping.load(Ordering::SeqCst)
    }
    pub fn progress(&self) -> Progress {
        *self.0.progress.lock().unwrap()
    }
    pub fn checkpoint(&self, checkpoint: &'static str) -> anyhow::Result<()> {
        if self.stopping() {
            return Err(NativeStopping.into());
        }
        let mut progress = self.0.progress.lock().unwrap();
        progress.checkpoint = checkpoint;
        progress.checkpoints = progress.checkpoints.saturating_add(1);
        Ok(())
    }
}
