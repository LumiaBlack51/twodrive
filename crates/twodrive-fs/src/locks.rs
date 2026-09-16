use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

pub(crate) fn item_sync_lock(local_id: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Weak<Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks.lock().expect("item sync lock registry is poisoned");
    if let Some(lock) = locks.get(local_id).and_then(Weak::upgrade) {
        return lock;
    }
    locks.retain(|_, lock| lock.strong_count() > 0);
    let lock = Arc::new(Mutex::new(()));
    locks.insert(local_id.to_string(), Arc::downgrade(&lock));
    lock
}
