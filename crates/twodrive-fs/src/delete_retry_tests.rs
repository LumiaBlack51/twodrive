use crate::{
    recovery::{enqueue_recovery, recover_pending_delete_id, recover_pending_deletes},
    upload_queue::{UploadCommand, UploadQueue},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};
use twodrive_backend::{
    CloudBackend,
    diagnostics::{RequestFailure, RequestOperation},
};
use twodrive_core::{Database, DeleteFailureKind, MetadataEntry};

struct Backend {
    deny: AtomicBool,
    calls: AtomicUsize,
}
impl CloudBackend for Backend {
    fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
        Ok(vec![])
    }
    fn download(&self, _: &str) -> anyhow::Result<Vec<u8>> {
        unreachable!()
    }
    fn upload(&self, _: &str, _: Vec<u8>) -> anyhow::Result<MetadataEntry> {
        unreachable!()
    }
    fn create_folder(&self, _: &str) -> anyhow::Result<MetadataEntry> {
        unreachable!()
    }
    fn rename(&self, _: &str, _: &str) -> anyhow::Result<MetadataEntry> {
        unreachable!()
    }
    fn delete(&self, id: &str) -> anyhow::Result<()> {
        if id == "denied-id" {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.deny.load(Ordering::SeqCst) {
                return Err(RequestFailure::new(403, RequestOperation::DriveItemDelete).into());
            }
        }
        Ok(())
    }
}

#[test]
fn persistent_delete_denial_is_not_requeued_or_executed_until_due_or_manual_retry() {
    let root = std::env::temp_dir().join(format!(
        "td-delete-retry-{}-{}",
        std::process::id(),
        twodrive_core::now_unix()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = Database::new(root.join("db"));
    db.init().unwrap();
    db.queue_remote_delete("denied-id", "/same-name").unwrap();
    db.queue_remote_delete("healthy-id", "/unrelated").unwrap();
    let backend = Backend {
        deny: AtomicBool::new(true),
        calls: AtomicUsize::new(0),
    };
    assert_eq!(recover_pending_deletes(&db, &backend).unwrap(), 1);
    let pending = db.pending_deletes().unwrap().remove(0);
    assert_eq!(pending.attempt_count, 1);
    assert_eq!(pending.last_status, Some(403));
    assert_eq!(pending.failure_kind, Some(DeleteFailureKind::Permission));
    assert!(!pending.retry_due(twodrive_core::now_unix()));
    let reopened = Database::new(db.path());
    reopened.init().unwrap();
    let (sender, receiver) = mpsc::channel();
    let queue = UploadQueue {
        sender,
        pending: Arc::new(Mutex::new(Default::default())),
    };
    enqueue_recovery(&reopened, &queue, true).unwrap();
    assert!(receiver.try_recv().is_err());
    assert!(!recover_pending_delete_id(&reopened, &backend, "denied-id").unwrap());
    assert_eq!(recover_pending_deletes(&reopened, &backend).unwrap(), 0);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    assert!(
        reopened
            .is_pending_delete("new-identity", "/same-name")
            .unwrap()
    );
    // A newer same-name object remains protected while the old object is blocked.
    assert_eq!(
        reopened.pending_deletes().unwrap()[0].remote_id,
        "denied-id"
    );
    backend.deny.store(false, Ordering::SeqCst);
    reopened.retry_pending_deletes().unwrap();
    enqueue_recovery(&reopened, &queue, true).unwrap();
    assert!(matches!(receiver.try_recv().unwrap(), UploadCommand::Delete(id) if id == "denied-id"));
    assert_eq!(recover_pending_deletes(&reopened, &backend).unwrap(), 1);
    assert!(reopened.pending_deletes().unwrap().is_empty());
    assert_eq!(backend.calls.load(Ordering::SeqCst), 2);
    std::fs::remove_dir_all(root).unwrap();
}
