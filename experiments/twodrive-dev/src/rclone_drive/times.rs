//! Locally known creation times, isolated by login generation and real object
//! identity. They never participate in remote versions or conditional writes.
use super::*;
use std::{collections::BTreeMap, sync::Mutex};

type Stamp = (u64, Option<std::time::SystemTime>);
type Saved = (Option<Stamp>, BTreeMap<String, i64>);
#[derive(Default)]
pub(super) struct CreationTimes(Mutex<Option<Saved>>);
impl CreationTimes {
    fn key(binding: &str, id: &str) -> String {
        let identity = id.split_once('#').map_or(id, |(id, _)| id);
        format!("{:x}", Sha256::digest(format!("{binding}\0{identity}")))
    }
    fn stamp(state: &State) -> anyhow::Result<Option<Stamp>> {
        match fs::metadata(state.dir.join("created-times.json")) {
            Ok(meta) => {
                ensure!(meta.len() <= 2 * 1024 * 1024, "创建时间记录超出预算");
                Ok(Some((meta.len(), meta.modified().ok())))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    pub fn get(&self, state: &State, binding: &str, id: &str) -> anyhow::Result<Option<i64>> {
        let stamp = Self::stamp(state)?;
        let mut saved = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("创建时间记录不可用"))?;
        if saved.as_ref().is_none_or(|(before, _)| *before != stamp) {
            let values = if stamp.is_some() {
                state.read("created-times.json")?
            } else {
                BTreeMap::new()
            };
            *saved = Some((stamp, values));
        }
        Ok(saved
            .as_ref()
            .unwrap()
            .1
            .get(&Self::key(binding, id))
            .copied())
    }
    pub fn record(&self, state: &State, binding: &str, id: &str, when: i64) -> anyhow::Result<()> {
        ensure!(when > 0, "无效创建时间");
        let lock = private_lock(state, "created-times.lock")?;
        // This optional display record must never queue a successful cloud
        // mutation behind another process indefinitely.
        FileExt::try_lock_exclusive(&lock)?;
        let mut values: BTreeMap<String, i64> = if Self::stamp(state)?.is_some() {
            state.read("created-times.json")?
        } else {
            BTreeMap::new()
        };
        ensure!(
            values.len() < 20000 || values.contains_key(&Self::key(binding, id)),
            "创建时间记录超出预算"
        );
        values.entry(Self::key(binding, id)).or_insert(when);
        state.save("created-times.json", &values)?;
        *self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("创建时间记录不可用"))? =
            Some((Self::stamp(state)?, values));
        Ok(())
    }
}

impl RcloneDrive {
    pub(super) fn with_created_time(
        &self,
        mut entry: MetadataEntry,
        id: &str,
    ) -> anyhow::Result<MetadataEntry> {
        // An unavailable optional display record must not break cloud reads.
        let when = self
            .created_times
            .get(&self.state, &self.binding, id)
            .ok()
            .flatten();
        entry.created_unix = when;
        // Rclone's default directory time is 2000-01-01 when Apple omits it.
        // A timestamp observed at our own successful create is preferable for
        // those new objects; existing unknown dates are not rewritten to now.
        if entry.is_dir
            && (entry.modified_unix <= 0 || entry.modified_unix == 946684800)
            && let Some(when) = when
        {
            entry.modified_unix = when;
        }
        Ok(entry)
    }
    pub(super) fn remember_created(
        &self,
        entry: MetadataEntry,
        when: i64,
    ) -> anyhow::Result<MetadataEntry> {
        let snapshot = Snapshot::decode(&entry.remote_id, &self.binding)?;
        if self
            .created_times
            .record(&self.state, &self.binding, &snapshot.id, when)
            .is_err()
        {
            eprintln!("TwoDrive: local creation-time persistence unavailable");
        }
        let mut entry = entry;
        entry.created_unix = Some(when);
        if entry.modified_unix <= 0 || entry.modified_unix == 946684800 {
            entry.modified_unix = when;
        }
        Ok(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn records_merge_between_instances_and_follow_identity_across_rename_and_etag_changes() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::new(dir.path().join("private")).unwrap();
        let a = CreationTimes::default();
        let b = CreationTimes::default();
        assert_eq!(b.get(&state, "account-a", "real-id#v1").unwrap(), None);
        a.record(&state, "account-a", "real-id#v1", 1800000000)
            .unwrap();
        assert_eq!(
            b.get(&state, "account-a", "real-id#v2").unwrap(),
            Some(1800000000)
        );
        b.record(&state, "account-a", "other-id#v1", 1800000001)
            .unwrap();
        assert_eq!(
            a.get(&state, "account-a", "other-id#v2").unwrap(),
            Some(1800000001)
        );
        assert_eq!(
            a.get(&state, "account-a", "real-id#v3").unwrap(),
            Some(1800000000)
        );
        assert_eq!(a.get(&state, "account-b", "real-id#v3").unwrap(), None);
        assert_eq!(
            a.get(&state, "account-a", "replacement-id#v3").unwrap(),
            None
        );
        let restart = CreationTimes::default();
        assert_eq!(
            restart.get(&state, "account-a", "real-id#v4").unwrap(),
            Some(1800000000)
        );
    }
    #[test]
    fn contended_or_corrupt_optional_records_fail_without_overwriting_saved_history() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::new(dir.path().join("private")).unwrap();
        let times = CreationTimes::default();
        let lock = private_lock(&state, "created-times.lock").unwrap();
        FileExt::lock_exclusive(&lock).unwrap();
        assert!(times.record(&state, "a", "id", 1800000000).is_err());
        drop(lock);
        fs::write(state.dir.join("created-times.json"), b"corrupt").unwrap();
        assert!(times.record(&state, "a", "id", 1800000000).is_err());
        assert_eq!(
            fs::read(state.dir.join("created-times.json")).unwrap(),
            b"corrupt"
        );
    }
}
