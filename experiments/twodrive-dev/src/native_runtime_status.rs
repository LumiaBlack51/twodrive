//! A status heartbeat must not wait for a catalogue batch or read SQLite.
use crate::{native_control::NativeControl, state::State};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};

struct Shared {
    report: Value,
    heartbeat: u64,
    error: Option<String>,
}
pub struct NativeRuntimeStatus {
    state: Arc<State>,
    control: NativeControl,
    shared: Arc<Mutex<Shared>>,
    done: Arc<(Mutex<bool>, Condvar)>,
    worker: Option<JoinHandle<()>>,
}
fn save(state: &State, control: &NativeControl, shared: &mut Shared) -> anyhow::Result<()> {
    shared.heartbeat += 1;
    shared.report["updated_unix"] = json!(twodrive_core::now_unix());
    shared.report["heartbeat"] = json!(shared.heartbeat);
    shared.report["progress"] = serde_json::to_value(control.progress())?;
    shared.report["stop_requested"] = json!(control.stopping());
    state.save("native-status.json", &shared.report)
}
impl NativeRuntimeStatus {
    pub fn start(
        state: State,
        control: NativeControl,
        interval: Duration,
        should_stop: impl Fn() -> bool + Send + 'static,
    ) -> anyhow::Result<Self> {
        let state = Arc::new(state);
        let mut first = Shared {
            report: json!({"phase":"starting"}),
            heartbeat: 0,
            error: None,
        };
        save(&state, &control, &mut first)?;
        let shared = Arc::new(Mutex::new(first));
        let done = Arc::new((Mutex::new(false), Condvar::new()));
        let (s, c, r, d) = (state.clone(), control.clone(), shared.clone(), done.clone());
        let worker = std::thread::spawn(move || {
            let mut last = Instant::now();
            loop {
                let guard = d.0.lock().unwrap();
                let (guard, _) = d.1.wait_timeout(guard, Duration::from_millis(50)).unwrap();
                if *guard {
                    break;
                }
                drop(guard);
                if should_stop() {
                    c.request_stop();
                }
                if last.elapsed() >= interval {
                    let mut report = r.lock().unwrap();
                    if let Err(error) = save(&s, &c, &mut report) {
                        report.error = Some(error.to_string());
                        c.request_stop();
                        break;
                    }
                    last = Instant::now();
                }
            }
        });
        Ok(Self {
            state,
            control,
            shared,
            done,
            worker: Some(worker),
        })
    }
    pub fn publish(&self, report: Value) -> anyhow::Result<()> {
        let mut shared = self.shared.lock().unwrap();
        anyhow::ensure!(
            shared.error.is_none(),
            "native heartbeat persistence failed"
        );
        shared.report = report;
        save(&self.state, &self.control, &mut shared)
    }
    fn shutdown(&mut self) {
        *self.done.0.lock().unwrap() = true;
        self.done.1.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
    pub fn finish(mut self, report: Value) -> anyhow::Result<()> {
        self.shutdown();
        self.publish(report)
    }
}
impl Drop for NativeRuntimeStatus {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn external_stop_and_heartbeat_progress_without_a_batch_returning() {
        let private = tempfile::tempdir().unwrap();
        let control = NativeControl::default();
        let requested = Arc::new(AtomicBool::new(false));
        let observed = requested.clone();
        let status = NativeRuntimeStatus::start(
            State::new(private.path().to_owned()).unwrap(),
            control.clone(),
            Duration::from_millis(50),
            move || observed.load(Ordering::SeqCst),
        )
        .unwrap();
        status
            .publish(json!({"phase":"working","stage":"remote","summary_updated_unix":1}))
            .unwrap();
        requested.store(true, Ordering::SeqCst);
        let start = Instant::now();
        let report = loop {
            let value: Value = serde_json::from_slice(
                &std::fs::read(private.path().join("native-status.json")).unwrap(),
            )
            .unwrap();
            if value["heartbeat"].as_u64().unwrap() >= 4 && value["stop_requested"] == true {
                break value;
            }
            assert!(start.elapsed() < Duration::from_secs(3));
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(report["phase"], "working");
        assert_eq!(
            report["summary_updated_unix"], 1,
            "heartbeat is not a completed sync"
        );
        assert!(control.checkpoint("next-object").is_err());
        status.finish(json!({"phase":"stopped"})).unwrap();
        let final_bytes = std::fs::read(private.path().join("native-status.json")).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            std::fs::read(private.path().join("native-status.json")).unwrap(),
            final_bytes,
            "joined heartbeat overwrote stopped state"
        );
    }
}
