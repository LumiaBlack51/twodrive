//! Compile the actual shared model/provider/cache bridge without the unrelated
//! transport stack. Native source tests additionally run on Windows.
#![allow(dead_code)]
#[path = "../../twodrive-dev/src/local_files.rs"]
mod local_files;
#[path = "../../twodrive-dev/src/model.rs"]
mod model;
#[cfg(all(windows, feature = "native-sync"))]
#[path = "../../twodrive-dev/src/native_source.rs"]
mod native_source;
#[path = "../../twodrive-dev/src/provider.rs"]
mod provider;
#[path = "../../twodrive-dev/src/provider_guard.rs"]
mod provider_guard;
#[cfg(all(
    windows,
    any(feature = "native-local-fixture", feature = "native-sync")
))]
extern crate twodrive_cfapi_probe as twodrive_local_files;
#[cfg(all(windows, feature = "native-local-fixture"))]
#[path = "../../twodrive-dev/src/native_local_fixture.rs"]
mod native_local_fixture;

#[path = "../../twodrive-dev/src/sync_policy.rs"]
mod sync_policy;
