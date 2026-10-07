//! Read-only cloud index. Staging never replaces the last committed generation.
use super::Database;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloudIdentity {
    pub account: String,
    pub drive: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(id: &str, parent: Option<&str>, kind: &str) -> IndexedItem {
        IndexedItem {
            id: id.into(),
            parent: parent.map(str::to_owned),
            name: id.into(),
            kind: kind.into(),
            size: 10,
            etag: "v1".into(),
            modified: None,
            deleted: false,
        }
    }
    #[test]
    fn atomic_delta_identity_moves_deletes_and_interrupted_rebuild() {
        let root = tempfile::tempdir().unwrap();
        let db = Database::new(root.path().join("db"));
        db.init_cloud().unwrap();
        let identity = CloudIdentity {
            account: "a".into(),
            drive: "d".into(),
        };
        let folder = item("folder", None, "folder");
        let child = item("child", Some("folder"), "file");
        db.cloud_begin(&identity).unwrap();
        db.cloud_stage(&identity, &[folder.clone(), child.clone()])
            .unwrap();
        assert!(db.cloud_items(&identity).unwrap().is_empty());
        assert!(db.cloud_delta(&identity).unwrap().is_none());
        db.cloud_commit(&identity, "cursor1", true).unwrap();
        let mut moved = child.clone();
        moved.parent = None;
        moved.name = "renamed".into();
        moved.etag = "v2".into();
        let mut deleted = folder;
        deleted.deleted = true;
        db.cloud_begin(&identity).unwrap();
        db.cloud_stage(&identity, &[deleted, moved.clone()])
            .unwrap();
        assert_eq!(db.cloud_items(&identity).unwrap().len(), 2);
        db.cloud_commit(&identity, "cursor2", false).unwrap();
        assert_eq!(db.cloud_items(&identity).unwrap(), vec![moved.clone()]);
        db.cloud_begin(&identity).unwrap();
        db.cloud_stage(&identity, &[item("new", None, "file")])
            .unwrap();
        // Simulate process loss after any page: last committed index and cursor remain usable.
        let reopened = Database::new(db.path());
        reopened.init_cloud().unwrap();
        assert_eq!(reopened.cloud_items(&identity).unwrap(), vec![moved]);
        assert_eq!(
            reopened.cloud_delta(&identity).unwrap().as_deref(),
            Some("cursor2")
        );
        reopened.cloud_begin(&identity).unwrap();
        reopened.cloud_stage(&identity, &[child]).unwrap();
        assert!(reopened.cloud_commit(&identity, "", true).is_err());
        assert_eq!(
            reopened.cloud_delta(&identity).unwrap().as_deref(),
            Some("cursor2")
        );
        reopened.cloud_commit(&identity, "cursor3", true).unwrap();
        assert_eq!(reopened.cloud_items(&identity).unwrap()[0].name, "child");
        let other = CloudIdentity {
            account: "other".into(),
            drive: "d".into(),
        };
        assert!(reopened.cloud_items(&other).unwrap().is_empty());
    }
    #[test]
    fn duplicate_delta_last_occurrence_wins_and_nonempty_folder_is_retained() {
        let root = tempfile::tempdir().unwrap();
        let db = Database::new(root.path().join("db"));
        db.init_cloud().unwrap();
        let identity = CloudIdentity {
            account: "a".into(),
            drive: "d".into(),
        };
        let folder = item("folder", None, "folder");
        db.cloud_stage(
            &identity,
            &[folder.clone(), item("child", Some("folder"), "file")],
        )
        .unwrap();
        db.cloud_commit(&identity, "one", true).unwrap();
        let mut tombstone = folder;
        tombstone.deleted = true;
        db.cloud_stage(&identity, &[tombstone]).unwrap();
        db.cloud_commit(&identity, "two", false).unwrap();
        assert_eq!(db.cloud_items(&identity).unwrap().len(), 2);
        let mut child = item("child", None, "file");
        child.deleted = true;
        db.cloud_stage(&identity, &[child]).unwrap();
        let restored = item("child", None, "file");
        db.cloud_stage(&identity, std::slice::from_ref(&restored))
            .unwrap();
        db.cloud_commit(&identity, "three", false).unwrap();
        assert!(db.cloud_items(&identity).unwrap().contains(&restored));
        // The retained tombstone must survive a refresh boundary: once the child
        // moves away, the folder disappears even without a repeated folder tombstone.
        assert_eq!(db.cloud_items(&identity).unwrap(), vec![restored]);
    }
    #[test]
    fn cursor_write_failure_rolls_back_corresponding_item_changes() {
        let root = tempfile::tempdir().unwrap();
        let db = Database::new(root.path().join("db"));
        db.init_cloud().unwrap();
        let identity = CloudIdentity {
            account: "a".into(),
            drive: "d".into(),
        };
        let original = item("old", None, "file");
        db.cloud_stage(&identity, std::slice::from_ref(&original))
            .unwrap();
        db.cloud_commit(&identity, "old-cursor", true).unwrap();
        db.cloud_stage(&identity, &[item("new", None, "file")])
            .unwrap();
        db.connect().unwrap().execute_batch("CREATE TRIGGER fail_cursor BEFORE INSERT ON cloud_delta BEGIN SELECT RAISE(ABORT,'injected transaction failure'); END;").unwrap();
        assert!(db.cloud_commit(&identity, "new-cursor", true).is_err());
        assert_eq!(db.cloud_items(&identity).unwrap(), vec![original]);
        assert_eq!(
            db.cloud_delta(&identity).unwrap().as_deref(),
            Some("old-cursor")
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexedItem {
    pub id: String,
    pub parent: Option<String>,
    pub name: String,
    pub kind: String,
    pub size: u64,
    pub etag: String,
    pub modified: Option<String>,
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadTask {
    pub identity: CloudIdentity,
    pub item: IndexedItem,
    pub state: String,
    pub done: u64,
    pub error: Option<String>,
}

impl Database {
    pub fn init_cloud(&self) -> anyhow::Result<()> {
        self.connect()?.execute_batch("CREATE TABLE IF NOT EXISTS cloud_items(account TEXT NOT NULL,drive TEXT NOT NULL,id TEXT NOT NULL,body TEXT NOT NULL,PRIMARY KEY(account,drive,id));
            CREATE TABLE IF NOT EXISTS cloud_stage(account TEXT NOT NULL,drive TEXT NOT NULL,id TEXT NOT NULL,body TEXT NOT NULL,PRIMARY KEY(account,drive,id));
            CREATE TABLE IF NOT EXISTS cloud_delta(account TEXT NOT NULL,drive TEXT NOT NULL,link TEXT NOT NULL,PRIMARY KEY(account,drive));
            CREATE TABLE IF NOT EXISTS cloud_tasks(account TEXT NOT NULL,drive TEXT NOT NULL,id TEXT NOT NULL,body TEXT NOT NULL,PRIMARY KEY(account,drive,id));")?;
        Ok(())
    }
    pub fn cloud_delta(&self, identity: &CloudIdentity) -> anyhow::Result<Option<String>> {
        Ok(self
            .connect()?
            .query_row(
                "SELECT link FROM cloud_delta WHERE account=?1 AND drive=?2",
                params![identity.account, identity.drive],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn cloud_begin(&self, identity: &CloudIdentity) -> anyhow::Result<()> {
        self.connect()?.execute(
            "DELETE FROM cloud_stage WHERE account=?1 AND drive=?2",
            params![identity.account, identity.drive],
        )?;
        Ok(())
    }
    pub fn cloud_stage(
        &self,
        identity: &CloudIdentity,
        items: &[IndexedItem],
    ) -> anyhow::Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        for item in items {
            tx.execute(
                "INSERT OR REPLACE INTO cloud_stage VALUES(?1,?2,?3,?4)",
                params![
                    identity.account,
                    identity.drive,
                    item.id,
                    serde_json::to_string(item)?
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn cloud_commit(
        &self,
        identity: &CloudIdentity,
        link: &str,
        replace: bool,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!link.is_empty(), "missing_delta_link");
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        if replace {
            tx.execute(
                "DELETE FROM cloud_items WHERE account=?1 AND drive=?2",
                params![identity.account, identity.drive],
            )?;
        }
        tx.execute("INSERT OR REPLACE INTO cloud_items SELECT * FROM cloud_stage WHERE account=?1 AND drive=?2 AND json_extract(body,'$.deleted')=0", params![identity.account,identity.drive])?;
        // Apply all moves first, then delete children before empty folders.
        tx.execute("UPDATE cloud_items SET body=json_set(body,'$.deleted',json('true')) WHERE account=?1 AND drive=?2 AND id IN (SELECT id FROM cloud_stage WHERE account=?1 AND drive=?2 AND json_extract(body,'$.deleted')=1)", params![identity.account,identity.drive])?;
        loop {
            let removed=tx.execute("DELETE FROM cloud_items AS i WHERE account=?1 AND drive=?2 AND json_extract(body,'$.deleted')=1 AND NOT EXISTS(SELECT 1 FROM cloud_items AS child WHERE child.account=i.account AND child.drive=i.drive AND json_extract(child.body,'$.parent')=i.id)", params![identity.account,identity.drive])?;
            if removed == 0 {
                break;
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO cloud_delta VALUES(?1,?2,?3)",
            params![identity.account, identity.drive, link],
        )?;
        tx.execute(
            "DELETE FROM cloud_stage WHERE account=?1 AND drive=?2",
            params![identity.account, identity.drive],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn cloud_items(&self, identity: &CloudIdentity) -> anyhow::Result<Vec<IndexedItem>> {
        let conn = self.connect()?;
        let mut stmt =
            conn.prepare("SELECT body FROM cloud_items WHERE account=?1 AND drive=?2 ORDER BY id")?;
        let rows = stmt.query_map(params![identity.account, identity.drive], |r| {
            r.get::<_, String>(0)
        })?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }
    pub fn cloud_task_save(&self, task: &DownloadTask) -> anyhow::Result<()> {
        self.connect()?.execute(
            "INSERT OR REPLACE INTO cloud_tasks VALUES(?1,?2,?3,?4)",
            params![
                task.identity.account,
                task.identity.drive,
                task.item.id,
                serde_json::to_string(task)?
            ],
        )?;
        Ok(())
    }
    pub fn cloud_tasks(&self, identity: &CloudIdentity) -> anyhow::Result<Vec<DownloadTask>> {
        let conn = self.connect()?;
        let mut stmt =
            conn.prepare("SELECT body FROM cloud_tasks WHERE account=?1 AND drive=?2 ORDER BY id")?;
        let rows = stmt.query_map(params![identity.account, identity.drive], |r| {
            r.get::<_, String>(0)
        })?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }
}
