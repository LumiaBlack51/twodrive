#![cfg_attr(windows, allow(dead_code))]
//! Files On-Demand, recovery and the FUSE adapter.
mod activity;
mod cache_io;
mod conflicts;
#[cfg(unix)]
mod filesystem;
mod hydration;
#[cfg(unix)]
mod inodes;
mod locks;
mod metadata;
#[cfg(unix)]
mod mount;
#[cfg(unix)]
mod probes;
#[cfg(unix)]
mod read_pool;
mod recovery;
mod upload_queue;

#[cfg(unix)]
pub use filesystem::TwoDriveFs;
pub use hydration::{hydrate_pending_pins, hydrate_record, pin_path, unpin_path};
pub use metadata::{sync_delta_metadata, sync_metadata};
#[cfg(unix)]
pub use mount::{mount_backend, mount_graph, mount_mock};
pub use recovery::{
    recover_dirty_uploads, recover_dirty_uploads_concurrent, recover_pending_deletes,
    recover_pending_metadata_operations,
};

pub use recovery::recover_dirty_record;
