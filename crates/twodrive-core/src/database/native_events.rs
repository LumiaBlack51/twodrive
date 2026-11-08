//! At-least-once native operation hints. Callback persistence is independent of
//! upload state, so an in-flight immutable upload keeps its recovery claim.
use super::Database;
use rusqlite::{TransactionBehavior, params};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeWatchState {
    pub epoch: i64,
    pub generation: i64,
    pub rescan_required: bool,
    pub through_sequence: i64,
}

impl Database {
    /// Consume individual decisions without acknowledging unrelated operations
    /// interleaved by other application threads. Compare the original payload.
    pub fn acknowledge_native_event_decisions(
        &self,
        binding: &str,
        decisions: &[(i64, String)],
    ) -> anyhow::Result<()> {
        anyhow::ensure!(decisions.len() <= 10_000, "too many native decisions");
        let mut conn = self.connect()?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (sequence, payload) in decisions {
            tx.execute(
                "DELETE FROM native_event_log WHERE sequence=?1 AND binding=?2 AND payload=?3",
                params![sequence, binding, payload],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Every new OS subscription starts with an unknown gap. A new inventory
    /// must reconcile that gap; old observations remain available for recovery.
    pub fn begin_native_watch(&self, binding: &str) -> anyhow::Result<NativeWatchState> {
        anyhow::ensure!(
            !binding.is_empty() && binding.len() <= 16384,
            "invalid watch binding"
        );
        let mut conn = self.connect()?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO native_watch_state(binding,epoch,generation,rescan_required) VALUES(?1,1,0,1)
            ON CONFLICT(binding) DO UPDATE SET epoch=epoch+1,generation=0,rescan_required=1", [binding])?;
        let state = watch_state(&tx, binding)?;
        tx.commit()?;
        Ok(state)
    }

    pub fn native_watch_state(&self, binding: &str) -> anyhow::Result<NativeWatchState> {
        watch_state(&self.connect()?, binding)
    }

    /// Commit the entire observed batch, including overflow, before returning
    /// it to consumers. This is post-operation evidence, never deletion consent.
    pub fn append_native_observations(
        &self,
        binding: &str,
        epoch: i64,
        payloads: &[String],
        overflow: bool,
    ) -> anyhow::Result<NativeWatchState> {
        anyhow::ensure!(payloads.len() <= 10_000, "observation batch too large");
        let mut bytes = 0usize;
        for payload in payloads {
            bytes = bytes
                .checked_add(payload.len())
                .ok_or_else(|| anyhow::anyhow!("observation size overflow"))?;
            anyhow::ensure!(bytes <= 4 * 1024 * 1024, "observation payloads too large");
            let _: serde_json::Value = serde_json::from_str(payload)?;
        }
        let mut conn = self.connect()?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = watch_state(&tx, binding)?;
        anyhow::ensure!(state.epoch == epoch, "superseded native watch");
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM native_observation_log WHERE binding=?1",
            [binding],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            count + payloads.len() as i64 <= 100_000,
            "observation queue needs recovery"
        );
        for payload in payloads {
            tx.execute(
                "INSERT INTO native_observation_log(binding,epoch,payload) VALUES(?1,?2,?3)",
                params![binding, epoch, payload],
            )?;
        }
        tx.execute("UPDATE native_watch_state SET generation=generation+1,rescan_required=CASE WHEN ?2 THEN 1 ELSE rescan_required END WHERE binding=?1", params![binding,overflow])?;
        let state = watch_state(&tx, binding)?;
        tx.commit()?;
        Ok(state)
    }

    /// Only an inventory reconciled through this exact watcher watermark may
    /// clear the gap. This does not authorize deletion based on missing paths.
    pub fn complete_native_watch_scan(
        &self,
        binding: &str,
        state: NativeWatchState,
    ) -> anyhow::Result<bool> {
        let conn = self.connect()?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        Ok(conn.execute("UPDATE native_watch_state SET rescan_required=0 WHERE binding=?1 AND epoch=?2 AND generation=?3", params![binding,state.epoch,state.generation])? == 1)
    }

    pub fn pending_native_observations(
        &self,
        binding: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<(i64, i64, String)>> {
        anyhow::ensure!(
            (1..=1000).contains(&limit),
            "bounded observation batch required"
        );
        let conn = self.connect()?;
        let mut stmt = conn.prepare("SELECT sequence,epoch,payload FROM native_observation_log WHERE binding=?1 ORDER BY sequence LIMIT ?2")?;
        Ok(stmt
            .query_map(params![binding, limit as i64], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<_, _>>()?)
    }

    /// The caller must first persist all effects/recovery jobs for this prefix.
    pub fn acknowledge_native_observations(
        &self,
        binding: &str,
        through: i64,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(through > 0, "invalid observation sequence");
        let conn = self.connect()?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute(
            "DELETE FROM native_observation_log WHERE binding=?1 AND sequence<=?2",
            params![binding, through],
        )?;
        Ok(())
    }

    /// Commit before acknowledging a native rename/delete request. Payloads are
    /// private local metadata; callers must never expose them through public IPC.
    pub fn append_native_event(&self, binding: &str, payload: &str) -> anyhow::Result<i64> {
        anyhow::ensure!(
            !binding.is_empty() && binding.len() <= 16384,
            "invalid native event binding"
        );
        anyhow::ensure!(payload.len() <= 512 * 1024, "native event too large");
        let _: serde_json::Value = serde_json::from_str(payload)?;
        let mut conn = self.connect()?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM native_event_log WHERE binding=?1",
            [binding],
            |row| row.get(0),
        )?;
        anyhow::ensure!(count < 100_000, "native event queue requires recovery");
        anyhow::ensure!(
            tx.execute(
                "INSERT INTO native_event_log(binding,payload) VALUES(?1,?2)",
                params![binding, payload]
            )? == 1,
            "native event was not persisted"
        );
        let sequence = tx.last_insert_rowid();
        tx.commit()?;
        Ok(sequence)
    }

    pub fn pending_native_events(
        &self,
        binding: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<(i64, String)>> {
        anyhow::ensure!(
            (1..=1000).contains(&limit),
            "bounded native event batch required"
        );
        let conn = self.connect()?;
        let mut statement = conn.prepare("SELECT sequence,payload FROM native_event_log WHERE binding=?1 ORDER BY sequence LIMIT ?2")?;
        Ok(statement
            .query_map(params![binding, limit as i64], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<Result<_, _>>()?)
    }

    /// Call only after the corresponding engine mutation/recovery job is
    /// durable. Replaying unacknowledged events must be idempotent by identity.
    pub fn acknowledge_native_events(&self, binding: &str, through: i64) -> anyhow::Result<()> {
        anyhow::ensure!(through > 0, "invalid native event sequence");
        let conn = self.connect()?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute(
            "DELETE FROM native_event_log WHERE binding=?1 AND sequence<=?2",
            params![binding, through],
        )?;
        Ok(())
    }
}

fn watch_state(conn: &rusqlite::Connection, binding: &str) -> anyhow::Result<NativeWatchState> {
    Ok(conn.query_row(
        "SELECT epoch,generation,rescan_required,COALESCE((SELECT MAX(sequence) FROM native_observation_log WHERE binding=?1),0) FROM native_watch_state WHERE binding=?1",
        [binding],
        |row| {
            Ok(NativeWatchState {
                epoch: row.get(0)?,
                generation: row.get(1)?,
                rescan_required: row.get(2)?,
                through_sequence:row.get(3)?,
            })
        },
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scan_watermark_cannot_acknowledge_a_newer_epoch_observation() {
        let root = tempfile::tempdir().unwrap();
        let db = Database::new(root.path().join("watch.sqlite3"));
        db.init().unwrap();
        let first = db.begin_native_watch("root-a").unwrap();
        let observed = db
            .append_native_observations("root-a", first.epoch, &["{}".into()], false)
            .unwrap();
        assert!(observed.through_sequence > 0);
        let next = db.begin_native_watch("root-a").unwrap();
        let latest = db
            .append_native_observations("root-a", next.epoch, &[r#"{"action":3}"#.into()], false)
            .unwrap();
        assert!(latest.through_sequence > observed.through_sequence);
        db.acknowledge_native_observations("root-a", observed.through_sequence)
            .unwrap();
        let retained = db.pending_native_observations("root-a", 100).unwrap();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].0, latest.through_sequence);
        assert!(!db.complete_native_watch_scan("root-a", observed).unwrap());
    }
    #[test]
    fn watch_restart_overflow_and_concurrent_batches_require_reconciliation() {
        let root = tempfile::tempdir().unwrap();
        let db = Database::new(root.path().join("watch.sqlite3"));
        db.init().unwrap();
        let first = db.begin_native_watch("root-a").unwrap();
        assert!(first.rescan_required);
        assert!(db.complete_native_watch_scan("root-a", first).unwrap());
        let next = db
            .append_native_observations(
                "root-a",
                first.epoch,
                &[r#"{"action":1,"id":123}"#.into()],
                false,
            )
            .unwrap();
        assert!(!next.rescan_required);
        let overflow = db
            .append_native_observations("root-a", first.epoch, &[], true)
            .unwrap();
        assert!(overflow.rescan_required);
        assert!(!db.complete_native_watch_scan("root-a", next).unwrap());
        let reopened = Database::new(db.path());
        reopened.init().unwrap();
        let second = reopened.begin_native_watch("root-a").unwrap();
        assert!(second.epoch > first.epoch && second.rescan_required);
        assert!(
            reopened
                .append_native_observations("root-a", first.epoch, &[], false)
                .is_err()
        );
        assert!(
            !reopened
                .complete_native_watch_scan("root-a", overflow)
                .unwrap()
        );
        let foreign = reopened.begin_native_watch("root-b").unwrap();
        reopened
            .append_native_observations("root-b", foreign.epoch, &["{}".into()], false)
            .unwrap();
        let retained = reopened.pending_native_observations("root-a", 100).unwrap();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].1, first.epoch);
        let conn = reopened.connect().unwrap();
        conn.execute_batch("CREATE TRIGGER fail_second BEFORE INSERT ON native_observation_log WHEN NEW.payload='null' BEGIN SELECT RAISE(ABORT,'injected append failure'); END;").unwrap();
        assert!(
            reopened
                .append_native_observations(
                    "root-a",
                    second.epoch,
                    &["{}".into(), "null".into()],
                    false
                )
                .is_err()
        );
        assert_eq!(reopened.native_watch_state("root-a").unwrap(), second);
        assert_eq!(
            reopened.pending_native_observations("root-a", 100).unwrap(),
            retained
        );
        reopened
            .acknowledge_native_observations("root-a", retained[0].0)
            .unwrap();
        assert!(
            reopened
                .pending_native_observations("root-a", 100)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            reopened
                .pending_native_observations("root-b", 100)
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn native_events_survive_restart_and_acknowledge_only_the_owned_prefix() {
        let root = tempfile::tempdir().unwrap();
        let db = Database::new(root.path().join("events.sqlite3"));
        db.init().unwrap();
        let first = db
            .append_native_event("root-a", r#"{"kind":"rename"}"#)
            .unwrap();
        let foreign = db
            .append_native_event("root-b", r#"{"kind":"delete"}"#)
            .unwrap();
        let second = db
            .append_native_event("root-a", r#"{"kind":"completed"}"#)
            .unwrap();
        assert!(first < foreign && foreign < second);
        let reopened = Database::new(db.path());
        reopened.init().unwrap();
        assert_eq!(
            reopened.pending_native_events("root-a", 1).unwrap()[0].0,
            first
        );
        reopened.acknowledge_native_events("root-a", first).unwrap();
        assert_eq!(
            reopened.pending_native_events("root-a", 100).unwrap()[0].0,
            second
        );
        assert_eq!(
            reopened.pending_native_events("root-b", 100).unwrap()[0].0,
            foreign
        );
        let conn = reopened.connect().unwrap();
        conn.execute_batch("CREATE TRIGGER refuse_native_event BEFORE INSERT ON native_event_log BEGIN SELECT RAISE(ABORT,'injected event persistence failure'); END;").unwrap();
        assert!(reopened.append_native_event("root-a", "{}").is_err());
        assert_eq!(
            reopened.pending_native_events("root-a", 100).unwrap().len(),
            1
        );
        assert!(reopened.append_native_event("root-a", "not json").is_err());
    }
}
