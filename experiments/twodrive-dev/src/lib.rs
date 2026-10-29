pub mod capfs;
pub mod delta;
mod delta_http;
pub mod discovery;
pub mod google;
pub mod local_files;
pub mod model;
#[cfg(all(windows, feature = "native-local-fixture"))]
pub mod native_local_fixture;
pub mod network;
pub mod onedrive;
pub mod peer;
pub mod profile;
pub mod provider;
pub mod state;
pub mod test_agent;
pub mod webdav;
