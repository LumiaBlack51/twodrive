//! Windows adapter experiment. No protocol, credentials, Flutter or transport code.
use twodrive_core::FileState;
pub mod catalogue;
pub mod fixtures;
pub mod migration;
pub mod names;

// The production desktop dependency cannot compile registration/dehydration.
#[cfg(all(windows, feature = "fixture-probe"))]
pub mod native;
#[cfg(all(windows, feature = "native-local-fixture"))]
mod shell;

/// Keep residency, pin intent and synchronization separate. Download != upload.
#[derive(Debug, Clone, Copy)]
pub struct LocalStatus {
    pub resident: bool,
    pub partial: bool,
    pub pinned: bool,
    pub os_in_sync: bool,
    pub modified_bytes: i64,
    pub engine_state: FileState,
    pub pending_operations: bool,
    pub open_handles: bool,
}

impl LocalStatus {
    /// Fail closed even when OS attributes and durable engine state disagree.
    /// The caller must hold the engine item lock and an exclusive CFAPI oplock.
    pub fn releasable(self) -> bool {
        self.resident
            && !self.pinned
            && self.os_in_sync
            && self.modified_bytes == 0
            && self.engine_state == FileState::Cached
            && !self.pending_operations
            && !self.open_handles
    }

    pub fn sync_label(self) -> &'static str {
        match self.engine_state {
            FileState::Conflict => "conflict",
            FileState::Error => "error",
            FileState::Writing | FileState::Dirty | FileState::Uploading => "pending_sync",
            _ if self.pending_operations || !self.os_in_sync || self.modified_bytes != 0 => {
                "pending_sync"
            }
            _ => "in_sync",
        }
    }

    pub fn residency_label(self) -> &'static str {
        if self.pinned {
            if self.resident {
                "pinned"
            } else {
                "pin_pending"
            }
        } else if self.resident {
            "locally_available"
        } else if self.partial {
            "partially_available"
        } else {
            "online_only"
        }
    }
}

/// A bounded transfer plan. CFAPI transfers are 4KiB aligned except at EOF.
/// The immutable fixture source uses opaque identities, never paths from callbacks.
pub fn transfer_range(offset: i64, length: i64, size: usize) -> Option<(usize, usize)> {
    if offset < 0 || length <= 0 || offset as u64 >= size as u64 {
        return None;
    }
    let start = (offset as usize / 4096) * 4096;
    let end = (offset as u64).checked_add(length as u64)?.min(size as u64) as usize;
    let end = end.div_ceil(4096).checked_mul(4096)?;
    Some((start, end.min(size)))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn clean() -> LocalStatus {
        LocalStatus {
            resident: true,
            partial: false,
            pinned: false,
            os_in_sync: true,
            modified_bytes: 0,
            engine_state: FileState::Cached,
            pending_operations: false,
            open_handles: false,
        }
    }
    #[test]
    fn release_requires_both_engine_and_os_confirmation() {
        assert!(clean().releasable());
        for state in [
            FileState::OnlineOnly,
            FileState::Hydrating,
            FileState::Pinned,
            FileState::Writing,
            FileState::Dirty,
            FileState::Uploading,
            FileState::Conflict,
            FileState::Error,
        ] {
            assert!(
                !LocalStatus {
                    engine_state: state,
                    ..clean()
                }
                .releasable()
            );
        }
        assert!(
            !LocalStatus {
                pinned: true,
                ..clean()
            }
            .releasable()
        );
        assert!(
            !LocalStatus {
                os_in_sync: false,
                ..clean()
            }
            .releasable()
        );
        assert!(
            !LocalStatus {
                modified_bytes: 1,
                ..clean()
            }
            .releasable()
        );
        assert!(
            !LocalStatus {
                pending_operations: true,
                ..clean()
            }
            .releasable()
        );
        assert!(
            !LocalStatus {
                open_handles: true,
                ..clean()
            }
            .releasable()
        );
    }
    #[test]
    fn residency_does_not_claim_sync_completion() {
        let dirty = LocalStatus {
            engine_state: FileState::Dirty,
            ..clean()
        };
        assert_eq!(dirty.residency_label(), "locally_available");
        assert_eq!(dirty.sync_label(), "pending_sync");
        assert_eq!(
            LocalStatus {
                resident: false,
                pinned: true,
                ..clean()
            }
            .residency_label(),
            "pin_pending"
        );
    }
    #[test]
    fn ranges_are_bounded_and_aligned() {
        assert_eq!(transfer_range(17, 1, 10000), Some((0, 4096)));
        assert_eq!(transfer_range(4096, 4096, 10000), Some((4096, 8192)));
        assert_eq!(transfer_range(8193, 90000, 10000), Some((8192, 10000)));
        for (o, l) in [(-1, 1), (0, 0), (10000, 1), (i64::MAX, i64::MAX)] {
            assert_eq!(transfer_range(o, l, 10000), None);
        }
    }
}
