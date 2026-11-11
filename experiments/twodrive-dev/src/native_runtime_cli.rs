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
fn event_signaled(handle: usize) -> bool {
    unsafe { WaitForSingleObject(handle as HANDLE, 0) == WAIT_OBJECT_0 }
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
    let state = state.canonicalize()?;
    // Validate ownership and obtain the exclusive native cache owner before
    // publishing status or creating a runtime event for this directory.
    let mut session = NativeSession::resume(&state)?;
    let name = event_name(&state)?;
    let event = StopEvent(unsafe { CreateEventW(std::ptr::null(), 1, 0, name.as_ptr()) });
    let event_error = unsafe { GetLastError() };
    ensure!(!event.0.is_null(), "native stop event unavailable");
    ensure!(
        event_error != ERROR_ALREADY_EXISTS,
        "native runtime already running"
    );
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
    let control = twodrive_dev::native_control::NativeControl::default();
    let event_handle = event.0 as usize;
    let status = twodrive_dev::native_runtime_status::NativeRuntimeStatus::start(
        twodrive_dev::state::State::new(state.clone())?,
        control.clone(),
        Duration::from_secs(1),
        move || {
            // The event outlives status; finish/Drop joins this observer before
            // its handle is closed. No mutating I/O is interrupted mid-object.
            event_signaled(event_handle)
                || signal.load(std::sync::atomic::Ordering::SeqCst)
                || (seconds != 0 && start.elapsed() >= Duration::from_secs(seconds))
        },
    )?;
    session.coordinator.set_control(control.clone());
    let mut next_remote = start;
    let mut next_residency = start;
    let mut previous = String::new();
    let mut summary_at = Instant::now();
    let mut summary = serde_json::to_value(session.coordinator.summary()?)?;
    let mut summary_updated = twodrive_core::now_unix();
    println!(
        "{}",
        serde_json::json!({"phase":"running","cloud_mutation_consumers":true,"summary":summary})
    );
    let outcome = (|| -> anyhow::Result<()> {
        while !control.stopping() {
            if session
                .state_directory()
                .parent()
                .is_some_and(|p| p.join("paused").exists())
            {
                status.publish(serde_json::json!({"phase":"paused","summary":summary,"summary_updated_unix":summary_updated}))?;
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            status.publish(serde_json::json!({"phase":"working","stage":"local","summary":summary,"summary_updated_unix":summary_updated}))?;
            let local_started = Instant::now();
            let local = session.coordinator.synchronize_once();
            if control.stopping() {
                break;
            }
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
            if !control.stopping() && Instant::now() >= next_remote {
                status.publish(serde_json::json!({"phase":"working","stage":"remote","summary":summary,"summary_updated_unix":summary_updated}))?;
                remote = match session.refresh_remote_controlled(&mut || {
                    control.checkpoint("remote.index")?;
                    Ok(())
                }) {
                    Ok(value) => serde_json::to_value(value)?,
                    Err(error) => {
                        serde_json::json!({"error":twodrive_backend::diagnostics::safe_error(&error).to_string()})
                    }
                };
                if control.stopping() {
                    break;
                }
                next_remote = Instant::now() + Duration::from_secs(30);
            }
            if !control.stopping() && Instant::now() >= next_residency {
                status.publish(serde_json::json!({"phase":"working","stage":"residency","summary":summary,"summary_updated_unix":summary_updated}))?;
                residency = match session.coordinator.synchronize_residency() {
                    Ok(value) => serde_json::to_value(value)?,
                    Err(error) => {
                        serde_json::json!({"error":twodrive_backend::diagnostics::safe_error(&error).to_string()})
                    }
                };
                if control.stopping() {
                    break;
                }
                next_residency = Instant::now() + Duration::from_secs(10);
            }
            if summary_at.elapsed() >= Duration::from_secs(10) {
                summary = serde_json::to_value(session.coordinator.summary()?)?;
                summary_updated = twodrive_core::now_unix();
                summary_at = Instant::now();
            }
            let report = serde_json::json!({"phase":"sync","local":local,"remote":remote,"residency":residency,"summary":summary,"summary_updated_unix":summary_updated});
            let value = report.to_string();
            status.publish(report)?;
            if value != previous {
                println!("{value}");
                previous = value;
            }
            for _ in 0..10 {
                if control.stopping() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Ok(())
    })();
    let stopping_status = status.publish(serde_json::json!({"phase":"stopping"}));
    let disconnected = session
        .coordinator
        .disconnect()
        .context("disconnect native runtime");
    let final_report = serde_json::json!({"phase":if disconnected.is_ok(){"stopped"}else{"stop_failed"},"registration_retained":true});
    println!("{final_report}");
    let final_status = status.finish(final_report);
    outcome
        .and(stopping_status)
        .and(disconnected)
        .and(final_status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use twodrive_dev::{
        native_control::NativeControl, native_runtime_status::NativeRuntimeStatus, state::State,
    };

    #[test]
    fn stop_command_is_observed_without_waiting_for_the_busy_batch() {
        let private = tempfile::tempdir().unwrap();
        let name = event_name(private.path()).unwrap();
        let event = StopEvent(unsafe { CreateEventW(std::ptr::null(), 1, 0, name.as_ptr()) });
        assert!(!event.0.is_null());
        let handle = event.0 as usize;
        let control = NativeControl::default();
        let status = NativeRuntimeStatus::start(
            State::new(private.path().to_owned()).unwrap(),
            control.clone(),
            Duration::from_millis(50),
            move || event_signaled(handle),
        )
        .unwrap();
        status
            .publish(serde_json::json!({"phase":"working","stage":"remote"}))
            .unwrap();
        stop(private.path().to_owned()).unwrap();
        let started = Instant::now();
        loop {
            let progress: serde_json::Value = serde_json::from_slice(
                &std::fs::read(private.path().join("native-status.json")).unwrap(),
            )
            .unwrap();
            if progress["stop_requested"] == true && progress["heartbeat"].as_u64().unwrap() >= 3 {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "named stop was not observed by the independent heartbeat"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(control.checkpoint("next-object").is_err());
        status
            .finish(serde_json::json!({"phase":"stopped"}))
            .unwrap();
    }
}
