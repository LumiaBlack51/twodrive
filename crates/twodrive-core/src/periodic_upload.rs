use crate::Database;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeriodicUpload {
    pub enabled: bool,
    #[serde(default)] // Previously saved policies retain their fixed intervals.
    pub adaptive: bool,
    pub small_mib: u32,
    pub medium_mib: u32,
    pub minutes: [u32; 3],
}

impl Default for PeriodicUpload {
    fn default() -> Self {
        Self {
            enabled: true,
            adaptive: true,
            small_mib: 10,
            medium_mib: 100,
            minutes: [1, 3, 5],
        }
    }
}

impl PeriodicUpload {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.small_mib > 0 && self.small_mib < self.medium_mib,
            "size limits must be positive and strictly increasing"
        );
        anyhow::ensure!(
            self.minutes.iter().all(|n| *n > 0),
            "intervals must be positive"
        );
        Ok(())
    }

    pub fn interval(&self, bytes: u64) -> Option<std::time::Duration> {
        let index = if bytes <= u64::from(self.small_mib) << 20 {
            0
        } else if bytes <= u64::from(self.medium_mib) << 20 {
            1
        } else {
            2
        };
        self.enabled
            .then(|| std::time::Duration::from_secs(u64::from(self.minutes[index]) * 60))
    }

    /// Delay from the first unpublished write. A transfer in progress owns its
    /// frozen revision, so only the latest bytes can be captured on the next run.
    pub fn due(&self, bytes: u64, since: i64, now: i64, last_upload_secs: u64) -> bool {
        self.interval(bytes).is_some_and(|tier| {
            let adaptive = if self.adaptive {
                last_upload_secs.saturating_add(last_upload_secs.div_ceil(4))
            } else {
                0
            };
            u64::try_from(now.saturating_sub(since))
                .is_ok_and(|age| age >= tier.as_secs().max(adaptive))
        })
    }

    pub fn load(db: &Database) -> anyhow::Result<Self> {
        let policy: Self = db
            .get_state_value("periodic_upload")?
            .map(|s| serde_json::from_str(&s))
            .transpose()?
            .unwrap_or_default();
        policy.validate()?;
        Ok(policy)
    }

    pub fn save(&self, db: &Database) -> anyhow::Result<()> {
        self.validate()?;
        db.set_state_value("periodic_upload", &serde_json::to_string(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adaptive_deadline_tracks_the_last_transfer_and_keeps_legacy_settings() {
        let old: PeriodicUpload = serde_json::from_str(
            r#"{"enabled":true,"small_mib":8,"medium_mib":64,"minutes":[2,4,9]}"#,
        )
        .unwrap();
        let size = 2_u64 << 40;
        assert!(!old.adaptive);
        assert!(old.due(size, 100, 640, 601));
        let mut p = PeriodicUpload::default();
        assert!(!p.due(size, 100, 399, 0));
        assert!(p.due(size, 100, 400, 0));
        assert!(!p.due(size, 100, 851, 601));
        assert!(p.due(size, 100, 852, 601));
        assert!(p.due(size, 100, 400, 1)); // faster subsequent transfer
        assert!(!p.due(size, 100, 99, 0));
        p.enabled = false;
        assert!(!p.due(size, 0, i64::MAX, 0));
    }

    #[test]
    fn policy_boundaries_and_validation() {
        let mut p = PeriodicUpload::default();
        for (size, seconds) in [
            (0, 60),
            (10 << 20, 60),
            ((10 << 20) + 1, 180),
            (100 << 20, 180),
            ((100 << 20) + 1, 300),
        ] {
            assert_eq!(p.interval(size).unwrap().as_secs(), seconds);
        }
        p.enabled = false;
        assert!(p.interval(0).is_none());
        p.minutes[1] = 0;
        assert!(p.validate().is_err());
        p.minutes[1] = 1;
        p.medium_mib = p.small_mib;
        assert!(p.validate().is_err());
    }
}
