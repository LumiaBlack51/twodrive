mod known_folders;
use known_folders::start_known_folder_sync;
use std::env;
use twodrive_core::{AppPaths, Database};
use twodrive_fs::{mount_graph, mount_mock};
fn main() -> anyhow::Result<()> {
    if env::args()
        .nth(1)
        .is_some_and(|arg| matches!(arg.as_str(), "--version" | "-V" | "version"))
    {
        println!("twodrive-daemon {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let paths = AppPaths::discover()?;
    paths.ensure()?;
    Database::new(paths.db_path.clone()).init()?;
    if env::var("TWODRIVE_BACKEND").as_deref() == Ok("mock") {
        mount_mock(paths)
    } else {
        start_known_folder_sync(paths.clone());
        mount_graph(paths)
    }
}
