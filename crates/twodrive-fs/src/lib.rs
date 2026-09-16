//! Files On-Demand, recovery and the FUSE adapter.
mod activity;
mod cache_io;
mod conflicts;
mod filesystem;
mod hydration;
mod inodes;
mod locks;
mod metadata;
mod mount;
mod probes;
mod read_pool;
mod recovery;
mod upload_queue;

pub use filesystem::TwoDriveFs;
pub use hydration::{hydrate_pending_pins, hydrate_record, pin_path, unpin_path};
pub use metadata::{sync_delta_metadata, sync_metadata};
pub use mount::{mount_backend, mount_graph, mount_mock};
pub use recovery::{
    recover_dirty_uploads, recover_dirty_uploads_concurrent, recover_pending_deletes,
    recover_pending_metadata_operations,
};
