//! One storage contract on every platform. Windows native synchronization uses
//! the same metadata identities and recovery interface as the Linux engine.
pub use twodrive_backend::{CloudBackend, DeltaResult};
pub use twodrive_core::MetadataEntry;
