//! Provider-neutral storage contract and the existing providers.
mod mock;
pub mod onedrive;
mod storage;

pub use mock::MockBackend;
pub use storage::{CloudBackend, DeltaResult};
// Compatibility re-export for existing consumers.
pub use onedrive::GraphBackend;
