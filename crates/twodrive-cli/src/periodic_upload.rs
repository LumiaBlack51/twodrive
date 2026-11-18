use twodrive_core::{AppPaths, Database, PeriodicUpload};

pub(crate) fn command(paths: &AppPaths, args: &[String]) -> anyhow::Result<()> {
    let db = Database::new(&paths.db_path);
    db.init()?;
    let policy = match args {
        [op] if op == "get" => PeriodicUpload::load(&db)?,
        [op, json] if op == "set" => {
            let p: PeriodicUpload = serde_json::from_str(json)?;
            p.save(&db)?;
            p
        }
        [op] if op == "reset" => {
            let p = PeriodicUpload::default();
            p.save(&db)?;
            p
        }
        _ => anyhow::bail!("usage: twodrive periodic-upload get | set <json> | reset"),
    };
    println!("{}", serde_json::to_string(&policy)?);
    Ok(())
}
