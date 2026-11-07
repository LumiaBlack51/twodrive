//! At-least-once native operation hints. Callback persistence is independent of
//! upload state, so an in-flight immutable upload keeps its recovery claim.
use super::Database;
use rusqlite::{TransactionBehavior, params};

impl Database {
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

#[cfg(test)]
mod tests {
    use super::*;
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
