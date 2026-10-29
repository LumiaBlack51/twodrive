// Compile the actual desktop boundary against this lightweight adapter without
// starting the unrelated transport stack on a memory-constrained host.
#![allow(dead_code)]
#[cfg(all(windows, feature = "native-local-fixture"))]
extern crate self as twodrive_dev;
extern crate twodrive_cfapi_probe as twodrive_local_files;
#[path = "../../twodrive-dev/src/desktop_local_files.rs"]
mod desktop_local_files;
#[cfg(all(windows, feature = "native-local-fixture"))]
#[path = "../../twodrive-dev/src/local_files.rs"]
mod local_files;
#[cfg(all(windows, feature = "native-local-fixture"))]
#[path = "../../twodrive-dev/src/model.rs"]
mod model;
#[cfg(all(windows, feature = "native-local-fixture"))]
#[path = "../../twodrive-dev/src/native_local_fixture.rs"]
mod native_local_fixture;
#[cfg(all(windows, feature = "native-local-fixture"))]
#[path = "../../twodrive-dev/src/provider.rs"]
mod provider;
#[cfg(all(windows, feature = "native-local-fixture"))]
#[path = "../../twodrive-dev/src/provider_guard.rs"]
mod provider_guard;
