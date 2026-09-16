use super::scan::should_skip;
use super::state::KnownFolderRoot;
use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use twodrive_core::Config;

pub(super) enum WatchWake {
    Event(notify::Result<Event>),
    Rescan,
    Disconnected,
}

pub(super) fn recv_watch_wake(
    rx: &mpsc::Receiver<notify::Result<Event>>,
    rescan_interval: Option<Duration>,
) -> WatchWake {
    match rescan_interval {
        Some(interval) => match rx.recv_timeout(interval) {
            Ok(event) => WatchWake::Event(event),
            Err(mpsc::RecvTimeoutError::Timeout) => WatchWake::Rescan,
            Err(mpsc::RecvTimeoutError::Disconnected) => WatchWake::Disconnected,
        },
        None => match rx.recv() {
            Ok(event) => WatchWake::Event(event),
            Err(_) => WatchWake::Disconnected,
        },
    }
}

pub(super) fn queue_addition_event(
    event: &Event,
    roots: &[KnownFolderRoot],
    config: &Config,
    pending: &mut HashSet<PathBuf>,
) -> bool {
    if !is_addition_event(event) {
        return false;
    }

    let before = pending.len();
    for path in &event.paths {
        if roots.iter().any(|root| path.starts_with(&root.local)) && !should_skip(path, config) {
            pending.insert(path.clone());
        }
    }
    pending.len() > before
}

pub(super) fn wait_for_relevant_quiet(
    rx: &mpsc::Receiver<notify::Result<Event>>,
    debounce: Duration,
    roots: &[KnownFolderRoot],
    config: &Config,
    pending: &mut HashSet<PathBuf>,
) {
    let mut quiet_until = Instant::now() + debounce;
    while let Some(remaining) = quiet_until.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(Ok(event)) => {
                let queued = queue_addition_event(&event, roots, config, pending);
                if queued || event_touches_pending(&event, pending) {
                    quiet_until = Instant::now() + debounce;
                }
            }
            Ok(Err(err)) => {
                eprintln!("twodrive: known folder watcher event error: {err}");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

pub(super) fn is_addition_event(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_)
            | EventKind::Modify(ModifyKind::Name(RenameMode::To | RenameMode::Both))
    )
}

pub(super) fn event_touches_pending(event: &Event, pending: &HashSet<PathBuf>) -> bool {
    event.paths.iter().any(|path| {
        pending
            .iter()
            .any(|pending_path| path.starts_with(pending_path) || pending_path.starts_with(path))
    })
}
