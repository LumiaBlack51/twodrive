//! Provider-neutral storage contract and the existing providers.
pub mod diagnostics;
mod mock;
pub mod onedrive;
mod storage;

pub use mock::MockBackend;
pub use storage::{CloudBackend, DeltaResult, UploadReader};
// Compatibility re-export for existing consumers.
pub use onedrive::GraphBackend;

pub mod control;
