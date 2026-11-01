use notify::{RecursiveMode, Watcher};
use std::collections::HashSet;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};
use twodrive_backend::GraphBackend;
use twodrive_core::{AppPaths, Config, Database};
mod events;
use events::{WatchWake, queue_addition_event, recv_watch_wake, wait_for_relevant_quiet};
mod scan;
use scan::{
    baseline_known_folder_path, configured_known_folders, sync_known_folder_paths,
    sync_known_folder_root,
};
mod uploads;
use uploads::retry_known_folder_uploads;
mod rate;
mod state;
mod store;
use state::KnownFolderState;
mod activity;

pub(crate) fn start_known_folder_sync(paths: AppPaths) {
    let Ok(config) = Config::load_or_create(&paths) else {
        return;
    };
    if !config.known_folders.enabled {
        return;
    }
    if config.known_folders.mode != "upload_only" {
        eprintln!(
            "twodrive: known folders mode '{}' is unsupported; expected upload_only",
            config.known_folders.mode
        );
        return;
    }
    if config.known_folders.upload_deletes {
        eprintln!("twodrive: known folders upload_deletes=true is ignored for safety");
    }

    thread::spawn(move || {
        if let Err(err) = run_known_folder_sync(paths, config) {
            eprintln!("twodrive: known folder sync stopped: {err:#}");
        }
    });
}

fn run_known_folder_sync(paths: AppPaths, config: Config) -> anyhow::Result<()> {
    let debounce = Duration::from_secs(config.known_folder_debounce_seconds()?);
    let retry_interval = Duration::from_secs(config.known_folder_upload_rate_seconds()?.min(60));
    let rescan_interval = match config.known_folder_rescan_seconds()? {
        0 => None,
        seconds => Some(Duration::from_secs(seconds)),
    };
    let roots = configured_known_folders(&config, &paths)?;
    if roots.is_empty() {
        return Ok(());
    }

    paths.ensure()?;
    let db = Database::new(paths.db_path.clone());
    db.init()?;
    let backend = GraphBackend::from_paths(&paths)?;
    let mut state = KnownFolderState::load(&paths.data_dir)?;

    let (tx, rx) = mpsc::sync_channel(events::MAX_PENDING_PATHS);
    let overflow = Arc::new(AtomicBool::new(false));
    let watch_overflow = Arc::clone(&overflow);
    let watch_roots = roots.clone();
    let watch_config = config.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        events::enqueue_watch_event(&tx, &watch_overflow, &watch_roots, &watch_config, event);
    })?;
    for root in &roots {
        if root.local.exists() {
            watcher.watch(&root.local, RecursiveMode::Recursive)?;
            eprintln!(
                "twodrive: watching known folder {} -> {}",
                root.local.display(),
                root.remote
            );
        }
    }

    retry_known_folder_uploads(&paths, &backend, &db, &config, &mut state)?;

    if !state.baseline_initialized {
        for root in &roots {
            baseline_known_folder_path(&config, &root.local, &mut state)?;
        }
        state.baseline_initialized = true;
        state.save(&paths.data_dir)?;
        eprintln!(
            "twodrive: initialized known folder baseline with {} files",
            state.files.len()
        );
    }

    if config.known_folders.startup_scan {
        for root in &roots {
            sync_known_folder_root(&paths, &backend, &db, &config, root, &mut state)?;
        }
    }

    let mut pending = HashSet::new();
    let mut last_scan = Instant::now();
    let mut last_retry = Instant::now();
    loop {
        // Dropped event payloads are replaced with a scan, including when the
        // periodic rescan is disabled. Never block the OS watcher on uploads.
        if overflow.swap(false, Ordering::AcqRel) {
            for root in &roots {
                sync_known_folder_root(&paths, &backend, &db, &config, root, &mut state)?;
            }
        }
        let retry_after = if state.rate_deferred {
            retry_interval
        } else {
            Duration::from_secs(60)
        };
        if last_retry.elapsed() >= retry_after {
            retry_known_folder_uploads(&paths, &backend, &db, &config, &mut state)?;
            last_retry = Instant::now();
        }
        let wake_interval = rescan_interval.unwrap_or(retry_after).min(retry_after);
        match recv_watch_wake(&rx, Some(wake_interval)) {
            WatchWake::Event(Ok(event)) => {
                if !queue_addition_event(&event, &roots, &config, &mut pending) {
                    continue;
                }
                wait_for_relevant_quiet(&rx, debounce, &roots, &config, &mut pending);
                sync_known_folder_paths(
                    &paths, &backend, &db, &config, &roots, &pending, &mut state,
                )?;
                pending.clear();
            }
            WatchWake::Event(Err(err)) => {
                eprintln!("twodrive: known folder watcher event error: {err}");
            }
            WatchWake::Rescan => {
                if rescan_interval.is_none_or(|interval| last_scan.elapsed() < interval) {
                    continue;
                }
                last_scan = Instant::now();
                for root in &roots {
                    sync_known_folder_root(&paths, &backend, &db, &config, root, &mut state)?;
                }
            }
            WatchWake::Disconnected => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
