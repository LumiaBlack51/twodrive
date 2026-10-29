//! Short-lived browsing snapshots only. Mutation and content verification never
//! read this cache. Per-key flights keep unrelated directories independent.
use crate::model::MetadataEntry;
use anyhow::ensure;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};

struct Sample<T> {
    checked: Instant,
    value: T,
}
#[derive(Default)]
struct State {
    generation: u64,
    directories: HashMap<String, Sample<Vec<MetadataEntry>>>,
    items: HashMap<String, Sample<Option<MetadataEntry>>>,
    flights: HashMap<String, Weak<Mutex<()>>>,
}
pub(super) struct Cache {
    ttl: Duration,
    state: Mutex<State>,
}
impl Cache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            state: Mutex::default(),
        }
    }
    pub fn invalidate(&self) {
        let mut s = self.state.lock().unwrap();
        s.generation = s.generation.wrapping_add(1);
        s.directories.clear();
        s.items.clear();
    }
    fn flight(&self, key: String) -> (u64, Arc<Mutex<()>>) {
        let mut s = self.state.lock().unwrap();
        s.flights.retain(|_, lock| lock.strong_count() > 0);
        let lock = s
            .flights
            .get(&key)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let lock = Arc::new(Mutex::new(()));
                s.flights.insert(key, Arc::downgrade(&lock));
                lock
            });
        (s.generation, lock)
    }
    fn item(&self, path: &str) -> Option<Option<MetadataEntry>> {
        let s = self.state.lock().unwrap();
        if let Some(v) = s.items.get(path).filter(|v| v.checked.elapsed() < self.ttl) {
            return Some(v.value.clone());
        }
        let parent = path.rsplit_once('/')?.0;
        let parent = if parent.is_empty() { "/" } else { parent };
        s.directories
            .get(parent)
            .filter(|v| v.checked.elapsed() < self.ttl)
            // Only a positive exact match is provider-neutral. Absence from a
            // listing cannot reject aliases (e.g. iCloud ASCII case lookup).
            .and_then(|v| {
                v.value
                    .iter()
                    .find(|entry| entry.path == path)
                    .cloned()
                    .map(Some)
            })
    }
    fn directory(&self, path: &str) -> Option<Vec<MetadataEntry>> {
        self.state
            .lock()
            .unwrap()
            .directories
            .get(path)
            .filter(|v| v.checked.elapsed() < self.ttl)
            .map(|v| v.value.clone())
    }
    pub fn metadata(
        &self,
        path: &str,
        mut check: impl FnMut() -> anyhow::Result<()>,
        fetch: impl FnOnce() -> anyhow::Result<Option<MetadataEntry>>,
    ) -> anyhow::Result<Option<MetadataEntry>> {
        check()?;
        if let Some(v) = self.item(path) {
            return Ok(v);
        }
        let (generation, flight) = self.flight(format!("item:{path}"));
        let _held = loop {
            check()?;
            match flight.try_lock() {
                Ok(lock) => break lock,
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(_) => anyhow::bail!("metadata flight unavailable"),
            }
        };
        if let Some(v) = self.item(path) {
            return Ok(v);
        }
        let value = fetch()?;
        check()?;
        let mut s = self.state.lock().unwrap();
        if s.generation == generation {
            if s.items.len() >= 20000 {
                s.items.clear();
            }
            s.items.insert(
                path.into(),
                Sample {
                    checked: Instant::now(),
                    value: value.clone(),
                },
            );
        }
        Ok(value)
    }
    pub fn list(
        &self,
        path: &str,
        mut check: impl FnMut() -> anyhow::Result<()>,
        fetch: impl FnOnce() -> anyhow::Result<Vec<MetadataEntry>>,
    ) -> anyhow::Result<Vec<MetadataEntry>> {
        check()?;
        if let Some(v) = self.directory(path) {
            return Ok(v);
        }
        let (generation, flight) = self.flight(format!("directory:{path}"));
        let _held = loop {
            check()?;
            match flight.try_lock() {
                Ok(lock) => break lock,
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(_) => anyhow::bail!("metadata flight unavailable"),
            }
        };
        if let Some(v) = self.directory(path) {
            return Ok(v);
        }
        let value = fetch()?;
        check()?;
        ensure!(value.len() <= 100000, "directory response exceeds budget");
        let mut s = self.state.lock().unwrap();
        if s.generation == generation && value.len() <= 20000 {
            if s.directories.len() >= 8
                || s.directories.values().map(|v| v.value.len()).sum::<usize>() + value.len()
                    > 20000
            {
                s.directories.clear();
            }
            s.directories.insert(
                path.into(),
                Sample {
                    checked: Instant::now(),
                    value: value.clone(),
                },
            );
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    fn entry() -> MetadataEntry {
        MetadataEntry::new_file("id", "/a/file", 3, 1, "v1")
    }
    #[test]
    fn directory_snapshot_answers_child_stats_and_expires_without_caching_failures() {
        let cache = Cache::new(Duration::from_millis(60));
        cache.list("/a", || Ok(()), || Ok(vec![entry()])).unwrap();
        assert!(
            cache
                .metadata("/a/file", || Ok(()), || panic!("duplicate remote query"))
                .unwrap()
                .is_some()
        );
        assert!(
            cache
                .metadata("/a/missing", || Ok(()), || Ok(None))
                .unwrap()
                .is_none()
        );
        let alias = cache
            .metadata("/a/FILE", || Ok(()), || Ok(Some(entry())))
            .unwrap()
            .unwrap();
        assert_eq!(alias.path, "/a/file");
        std::thread::sleep(Duration::from_millis(80));
        assert!(
            cache
                .list("/a", || Ok(()), || anyhow::bail!("network failure"))
                .is_err()
        );
        assert_eq!(cache.list("/a", || Ok(()), || Ok(vec![])).unwrap().len(), 0);
    }
    #[test]
    fn concurrent_same_directory_coalesces_while_another_directory_progresses() {
        let cache = Arc::new(Cache::new(Duration::from_secs(2)));
        let calls = Arc::new(AtomicUsize::new(0));
        let (started, receive) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let slow = {
            let c = cache.clone();
            let n = calls.clone();
            std::thread::spawn(move || {
                c.list(
                    "/a",
                    || Ok(()),
                    || {
                        n.fetch_add(1, Ordering::SeqCst);
                        started.send(()).unwrap();
                        released.recv().unwrap();
                        Ok(vec![entry()])
                    },
                )
                .unwrap()
            })
        };
        receive.recv_timeout(Duration::from_secs(2)).unwrap();
        let other = cache.list("/b", || Ok(()), || Ok(vec![])).unwrap();
        assert!(other.is_empty());
        let waiters: Vec<_> = (0..5)
            .map(|_| {
                let c = cache.clone();
                let n = calls.clone();
                std::thread::spawn(move || {
                    c.list(
                        "/a",
                        || Ok(()),
                        || {
                            n.fetch_add(1, Ordering::SeqCst);
                            Ok(vec![entry()])
                        },
                    )
                    .unwrap()
                })
            })
            .collect();
        release.send(()).unwrap();
        slow.join().unwrap();
        for w in waiters {
            w.join().unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn mutation_invalidation_prevents_an_older_inflight_response_from_becoming_cached() {
        let cache = Arc::new(Cache::new(Duration::from_secs(2)));
        let (started, receive) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let worker = {
            let c = cache.clone();
            std::thread::spawn(move || {
                c.list(
                    "/a",
                    || Ok(()),
                    || {
                        started.send(()).unwrap();
                        released.recv().unwrap();
                        Ok(vec![entry()])
                    },
                )
                .unwrap()
            })
        };
        receive.recv_timeout(Duration::from_secs(2)).unwrap();
        cache.invalidate();
        release.send(()).unwrap();
        worker.join().unwrap();
        assert!(
            cache
                .list("/a", || Ok(()), || Ok(vec![]))
                .unwrap()
                .is_empty()
        );
        let other = Cache::new(Duration::from_secs(2));
        assert!(other.item("/a/file").is_none());
        assert!(
            cache
                .metadata("/a/file", || anyhow::bail!("paused"), || Ok(None))
                .is_err()
        );
    }
}
