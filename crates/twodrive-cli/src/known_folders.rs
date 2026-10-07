use std::{env, fs, io::Write, path::PathBuf};
use twodrive_core::{
    AppPaths, Config, KnownFolderSourceState, expand_known_folder_home,
    inspect_known_folder_source, known_folder_diagnostics,
};

pub(crate) fn command(paths: &AppPaths, args: &[String]) -> anyhow::Result<()> {
    match args.first().map(String::as_str) {
        Some("status") if args.len() == 1 => {
            let config = Config::load_or_create(paths)?;
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "enabled": config.known_folders.enabled,
                    "mode": config.known_folders.mode,
                    "folders": known_folder_diagnostics(&config, paths)?,
                }))?
            );
            Ok(())
        }
        Some("set-source")
            if args.len() == 3 || (args.len() == 5 && args[3] == "--expected-local") =>
        {
            let index: usize = args[1].parse()?;
            set_source(paths, index, &args[2], args.get(4).map(String::as_str))?;
            println!(
                "Source saved. Restart TwoDrive to apply the upload mapping. System special directories are configured separately."
            );
            Ok(())
        }
        _ => anyhow::bail!(
            "usage: twodrive known-folders status | set-source <index> <directory> [--expected-local <old-source>]"
        ),
    }
}

fn set_source(
    paths: &AppPaths,
    index: usize,
    source: &str,
    expected: Option<&str>,
) -> anyhow::Result<()> {
    // Validate before creating/writing anything. A HOME fallback is not a safe upload source.
    let local = expand_known_folder_home(source)?;
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
    let state = inspect_known_folder_source(&local, &home, &paths.mount_dir);
    anyhow::ensure!(
        state == KnownFolderSourceState::Ready,
        "{}",
        state.message()
    );
    let local = local
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("source path must be UTF-8"))?;
    let original = fs::read_to_string(&paths.config_path)?;
    let mut document: toml::Value = toml::from_str(&original)?;
    let folders = document
        .get_mut("known_folders")
        .and_then(|value| value.get_mut("folders"))
        .and_then(toml::Value::as_array_mut)
        .ok_or_else(|| anyhow::anyhow!("no configured upload mappings"))?;
    let folder = folders
        .get_mut(index)
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("upload mapping index does not exist"))?;
    if let Some(expected) = expected {
        anyhow::ensure!(
            folder.get("local").and_then(toml::Value::as_str) == Some(expected),
            "upload mapping changed; refresh TwoDrive Settings before selecting a source"
        );
    }
    folder.insert("local".to_string(), toml::Value::String(local.to_string()));
    // Preserve unknown settings, destinations and all other mappings; replace atomically.
    let updated = toml::to_string_pretty(&document)?;
    let _: Config = toml::from_str(&updated)?;
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let temp = paths.config_dir.join(format!(
        ".config-source-{}-{suffix}.tmp",
        std::process::id()
    ));
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let result = (|| -> anyhow::Result<()> {
        output.set_permissions(fs::metadata(&paths.config_path)?.permissions())?;
        output.write_all(updated.as_bytes())?;
        output.sync_all()?;
        anyhow::ensure!(
            fs::read_to_string(&paths.config_path)? == original,
            "configuration changed; refresh TwoDrive Settings before saving"
        );
        fs::rename(&temp, &paths.config_path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
