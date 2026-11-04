//! Compile the actual Windows model/provider/cache bridge without the unrelated
//! network stack. Full desktop CI also compiles/runs the same tests in the engine.
#![cfg(windows)]
#![allow(dead_code)]
#[path = "../../twodrive-dev/src/local_files.rs"]
mod local_files;
#[path = "../../twodrive-dev/src/model.rs"]
mod model;
#[path = "../../twodrive-dev/src/provider.rs"]
mod provider;
#[path = "../../twodrive-dev/src/provider_guard.rs"]
mod provider_guard;
#[cfg(feature = "native-local-fixture")]
extern crate twodrive_cfapi_probe as twodrive_local_files;
#[cfg(feature = "native-local-fixture")]
#[path = "../../twodrive-dev/src/native_local_fixture.rs"]
mod native_local_fixture;
