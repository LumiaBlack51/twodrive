//! Bounded safe transport diagnostics plus persistent connection route status.
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
pub(super) const SYSTEM_TCP_OPEN: &str = "network: system TCP tunnel opened";
pub(super) const SYSTEM_TCP_CLOSE: &str = "network: system TCP tunnel closed";

#[derive(Default)]
pub struct Diagnostics {
    lines: Mutex<Vec<String>>,
    fallback: AtomicBool,
    component_fallback: AtomicBool,
    system_tunnels: AtomicUsize,
}
impl Diagnostics {
    pub fn record(&self, line: &str) {
        if !line.starts_with("network:") {
            return;
        }
        // The owned peer CLI emits this exact protocol marker. Keep the route
        // flag independently of the bounded, human-readable diagnostic history.
        if line == SYSTEM_TCP_OPEN {
            self.system_tunnels.fetch_add(1, Ordering::Relaxed);
        } else if line == SYSTEM_TCP_CLOSE {
            let _ = self
                .system_tunnels
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
        } else if line.starts_with("network: route=system/proxy fallback; component=") {
            self.component_fallback.store(true, Ordering::Relaxed);
        } else if matches!(
            line,
            "network: route=system/proxy fallback; physical setup or relay registration failed"
                | "network: route=system/proxy fallback; peer transport connection failed; authorization has not been sent"
        ) {
            self.fallback.store(true, Ordering::Relaxed);
        }
        let mut lines = self.lines.lock().unwrap();
        if lines.len() >= 24 {
            lines.remove(0);
        }
        lines.push(line.chars().take(512).collect());
    }
    pub fn snapshot(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
    pub fn system_tunnel_active(&self) -> bool {
        self.system_tunnels.load(Ordering::Relaxed) > 0
    }
    pub fn component_fallback(&self) -> bool {
        self.component_fallback.load(Ordering::Relaxed)
    }
    pub fn fallback(&self) -> bool {
        self.fallback.load(Ordering::Relaxed)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn route_warning_survives_diagnostic_rotation_and_resets_per_connection() {
        let d = Diagnostics::default();
        d.record("network: unrelated text route=system/proxy fallback");
        assert!(!d.fallback());
        d.record(
            "network: route=system/proxy fallback; component=DNS TXT; physical connection failed",
        );
        assert!(
            !d.fallback(),
            "a discovery lookup cannot select the peer transport"
        );
        assert!(d.component_fallback());
        d.record(SYSTEM_TCP_OPEN);
        d.record(SYSTEM_TCP_OPEN);
        d.record(SYSTEM_TCP_CLOSE);
        assert!(d.system_tunnel_active());
        d.record(SYSTEM_TCP_CLOSE);
        assert!(!d.system_tunnel_active());
        assert!(!d.fallback());
        d.record(
            "network: route=system/proxy fallback; physical setup or relay registration failed",
        );
        for _ in 0..100 {
            d.record("network: route details");
        }
        assert!(d.fallback());
        assert_eq!(d.snapshot().len(), 24);
        assert!(!d.snapshot().iter().any(|s| s.contains("fallback")));
        assert!(!Diagnostics::default().fallback());
    }
}
