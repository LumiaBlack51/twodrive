use super::state::{FileSnapshot, KnownFolderRoot, KnownFolderState, KnownFolderUploadJob};
use super::uploads::process_known_folder_uploads;
use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use twodrive_backend::CloudBackend;
use twodrive_core::{
    AppPaths, Config, Database, KnownFolderSourceState, inspect_known_folder_source,
    join_cloud_path, known_folder_diagnostics, normalize_cloud_path,
};

pub(super) fn configured_known_folders(
    config: &Config,
    paths: &AppPaths,
) -> anyhow::Result<Vec<KnownFolderRoot>> {
    let mut roots = Vec::new();
    for diagnostic in known_folder_diagnostics(config, paths)? {
        for warning in &diagnostic.warnings {
            eprintln!("twodrive: known folder {}: {warning}", diagnostic.remote);
        }
        if diagnostic.state != KnownFolderSourceState::Ready {
            eprintln!(
                "twodrive: known folder {} source {}: {}",
                diagnostic.remote,
                diagnostic.state.as_str(),
                diagnostic.message
            );
            continue;
        }
        if config.known_folders.folders[diagnostic.index]
            .remote
            .trim()
            .is_empty()
        {
            continue;
        }
        roots.push(KnownFolderRoot {
            local: diagnostic.local,
            remote: diagnostic.remote,
        });
    }
    Ok(roots)
}

pub(super) fn sync_known_folder_paths<B: CloudBackend>(
    paths: &AppPaths,
    backend: &B,
    db: &Database,
    config: &Config,
    roots: &[KnownFolderRoot],
    pending: &HashSet<PathBuf>,
    state: &mut KnownFolderState,
) -> anyhow::Result<()> {
    let mut jobs = Vec::new();
    let mut removed = false;
    for path in pending {
        let Some(root) = roots.iter().find(|root| path.starts_with(&root.local)) else {
            continue;
        };
        if !path.exists() {
            state.remove_path(path);
            removed = true;
            continue;
        }
        collect_local_path(config, root, path, state, &mut jobs)?;
    }
    process_known_folder_uploads(paths, backend, db, config, jobs, state)?;
    if removed {
        state.save(&paths.data_dir)?;
    }
    Ok(())
}

pub(super) fn sync_known_folder_root<B: CloudBackend>(
    paths: &AppPaths,
    backend: &B,
    db: &Database,
    config: &Config,
    root: &KnownFolderRoot,
    state: &mut KnownFolderState,
) -> anyhow::Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
    let source_state = inspect_known_folder_source(&root.local, &home, &paths.mount_dir);
    if source_state != KnownFolderSourceState::Ready {
        eprintln!(
            "twodrive: known folder {} source {}: {}",
            root.remote,
            source_state.as_str(),
            source_state.message()
        );
        return Ok(());
    }
    let mut jobs = Vec::new();
    collect_local_path(config, root, &root.local, state, &mut jobs)?;
    process_known_folder_uploads(paths, backend, db, config, jobs, state)
}

pub(super) fn baseline_known_folder_path(
    config: &Config,
    path: &Path,
    state: &mut KnownFolderState,
) -> anyhow::Result<()> {
    baseline_path(config, path, path, state)
}

fn baseline_path(
    config: &Config,
    root: &Path,
    path: &Path,
    state: &mut KnownFolderState,
) -> anyhow::Result<()> {
    if should_skip(path.strip_prefix(root).unwrap_or(path), config) {
        return Ok(());
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            baseline_path(config, root, &entry?.path(), state)?;
        }
    } else if metadata.is_file() {
        state
            .files
            .entry(path.to_string_lossy().into_owned())
            .or_insert_with(|| FileSnapshot::from_metadata(&metadata));
    }
    Ok(())
}

pub(super) fn collect_local_path(
    config: &Config,
    root: &KnownFolderRoot,
    path: &Path,
    state: &KnownFolderState,
    jobs: &mut Vec<KnownFolderUploadJob>,
) -> anyhow::Result<()> {
    if should_skip(path.strip_prefix(&root.local).unwrap_or(path), config) {
        return Ok(());
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            collect_local_path(config, root, &entry.path(), state, jobs)?;
        }
        return Ok(());
    }
    if !metadata.is_file() {
        return Ok(());
    }

    let snapshot = FileSnapshot::from_metadata(&metadata);
    let local_key = path.to_string_lossy().into_owned();
    if state.files.get(&local_key) == Some(&snapshot) {
        return Ok(());
    }

    let remote_path = remote_path_for(root, path)?;
    jobs.push(KnownFolderUploadJob {
        local_path: path.to_path_buf(),
        remote_path,
        snapshot,
    });
    Ok(())
}

pub(super) fn remote_path_for(root: &KnownFolderRoot, path: &Path) -> anyhow::Result<String> {
    let relative = path.strip_prefix(&root.local)?;
    let mut remote = root.remote.clone();
    for part in relative.components() {
        let value = part.as_os_str().to_string_lossy();
        if !value.is_empty() {
            remote = join_cloud_path(&remote, &value);
        }
    }
    Ok(normalize_cloud_path(&remote))
}

pub(super) fn parent_cloud_path(path: &str) -> Option<String> {
    let path = normalize_cloud_path(path);
    let (parent, _) = path.rsplit_once('/')?;
    if parent.is_empty() {
        Some("/".to_string())
    } else {
        Some(parent.to_string())
    }
}

pub(super) fn should_skip(path: &Path, config: &Config) -> bool {
    path.components().any(|part| {
        part.as_os_str()
            .to_str()
            .is_some_and(|name| skip_name(name, config))
    })
}

pub(super) fn should_skip_source(path: &Path, config: &Config) -> bool {
    let root = config
        .known_folders
        .folders
        .iter()
        .filter_map(|folder| twodrive_core::expand_known_folder_home(&folder.local).ok())
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count());
    should_skip(
        root.as_ref()
            .and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(path),
        config,
    )
}

fn skip_name(name: &str, config: &Config) -> bool {
    if config
        .known_folders
        .exclude_names
        .iter()
        .any(|excluded| name == excluded)
    {
        return true;
    }
    if matches!(name, "." | "..") {
        return true;
    }
    if name.starts_with(".") && name != "." {
        return true;
    }
    if name.starts_with("~$")
        || name.ends_with('~')
        || name.ends_with(".swp")
        || name.ends_with(".swo")
        || name.ends_with(".swx")
        || matches!(name, "Thumbs.db" | "desktop.ini" | ".DS_Store")
    {
        return true;
    }
    config
        .known_folders
        .exclude_suffixes
        .iter()
        .any(|suffix| name.ends_with(suffix))
}
