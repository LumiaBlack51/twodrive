use anyhow::Context;
use anyhow::ensure;
use sha2::{Digest, Sha256};
use std::os::windows::ffi::OsStrExt;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use twodrive_dev::native_coordinator::NativeSession;
use windows_sys::Win32::{Foundation::*, System::Threading::*};

struct StopEvent(HANDLE);
impl Drop for StopEvent {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}
fn event_name(state: &std::path::Path) -> anyhow::Result<Vec<u16>> {
    let canonical = state.canonicalize()?;
    let name = format!(
        "Local\\TwoDriveDevNativeStop-{:x}",
        Sha256::digest(canonical.to_string_lossy().as_bytes())
    );
    Ok(std::ffi::OsStr::new(&name)
        .encode_wide()
        .chain(Some(0))
        .collect())
}
pub(super) fn stop(state: PathBuf) -> anyhow::Result<()> {
    let name = event_name(&state)?;
    let raw = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) };
    ensure!(!raw.is_null(), "native runtime is not running");
    let event = StopEvent(raw);
    ensure!(
        unsafe { SetEvent(event.0) } != 0,
        "cannot request native runtime stop"
    );
    println!("{}", serde_json::json!({"phase":"stop_requested"}));
    Ok(())
}
pub(super) fn run(state: PathBuf, seconds: u64) -> anyhow::Result<()> {
    let mut session = NativeSession::resume(&state)?;
    let name = event_name(session.state_directory())?;
    let event = StopEvent(unsafe { CreateEventW(std::ptr::null(), 1, 0, name.as_ptr()) });
    ensure!(!event.0.is_null(), "native stop event unavailable");
    let stop_requested = || unsafe { WaitForSingleObject(event.0, 0) == WAIT_OBJECT_0 };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let signal = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let signal_writer = signal.clone();
    runtime.spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_writer.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    });
    let start = Instant::now();
    let cancelled = || {
        stop_requested()
            || signal.load(std::sync::atomic::Ordering::SeqCst)
            || (seconds != 0 && start.elapsed() >= Duration::from_secs(seconds))
    };
    let mut next_remote = start;
    let mut next_residency = start;
    let mut previous = String::new();
    println!(
        "{}",
        serde_json::json!({"phase":"running","cloud_mutation_consumers":true,"summary":session.coordinator.summary()?})
    );
    let outcome = (|| -> anyhow::Result<()> {
        while !cancelled() {
            let local_started = Instant::now();
            let local = session.coordinator.synchronize_once();
            let mut local = match local {
                Ok(value) => serde_json::to_value(value)?,
                Err(error) => {
                    serde_json::json!({"error":twodrive_backend::diagnostics::safe_error(&error).to_string()})
                }
            };
            if local_started.elapsed() >= Duration::from_secs(1) {
                local
                    .as_object_mut()
                    .context("local progress must be an object")?
                    .insert(
                        "duration_ms".into(),
                        serde_json::json!(local_started.elapsed().as_millis()),
                    );
            }
            let mut remote = serde_json::Value::Null;
            let mut residency = serde_json::Value::Null;
            if !cancelled() && Instant::now() >= next_remote {
                remote = match session.refresh_remote_controlled(&mut || {
                    ensure!(!cancelled(), "native runtime stopping");
                    Ok(())
                }) {
                    Ok(value) => serde_json::to_value(value)?,
                    Err(error) => {
                        serde_json::json!({"error":twodrive_backend::diagnostics::safe_error(&error).to_string()})
                    }
                };
                next_remote = Instant::now() + Duration::from_secs(30);
            }
            if !cancelled() && Instant::now() >= next_residency {
                residency = match session.coordinator.synchronize_residency() {
                    Ok(value) => serde_json::to_value(value)?,
                    Err(error) => {
                        serde_json::json!({"error":twodrive_backend::diagnostics::safe_error(&error).to_string()})
                    }
                };
                next_residency = Instant::now() + Duration::from_secs(10);
            }
            let value=serde_json::json!({"phase":"sync","local":local,"remote":remote,"residency":residency}).to_string();
            if value != previous {
                println!("{value}");
                previous = value;
            }
            for _ in 0..10 {
                if cancelled() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Ok(())
    })();
    session
        .coordinator
        .disconnect()
        .context("disconnect native runtime")?;
    println!(
        "{}",
        serde_json::json!({"phase":"stopped","registration_retained":true})
    );
    outcome
}
