//! Non-test compilation catches cfg/import mistakes hidden by test harnesses.
#![allow(dead_code)]
#[cfg(windows)]
#[path = "../../twodrive-dev/src/local_files.rs"]
mod local_files;
#[cfg(windows)]
#[path = "../../twodrive-dev/src/model.rs"]
mod model;
#[cfg(windows)]
#[path = "../../twodrive-dev/src/provider.rs"]
mod provider;
#[cfg(windows)]
#[path = "../../twodrive-dev/src/provider_guard.rs"]
mod provider_guard;
#[cfg(all(windows, feature = "native-local-fixture"))]
extern crate twodrive_cfapi_probe as twodrive_local_files;
#[cfg(all(windows, feature = "native-local-fixture"))]
#[path = "../../twodrive-dev/src/native_local_fixture.rs"]
mod native_local_fixture;
fn main() {}
