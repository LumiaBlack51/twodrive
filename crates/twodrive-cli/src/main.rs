use std::env;
use twodrive_backend::GraphBackend;
use twodrive_core::AppPaths;
use twodrive_fs::mount_mock;

mod presentation;
use presentation::{open_folder, open_settings, print_help, print_status, status_path};
mod commands;
use commands::{init_mock, logout, mount, pin, prune_cache, release, sync, unpin};
mod arguments;
use arguments::required_path;

fn main() -> anyhow::Result<()> {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    let command = args.first().cloned().unwrap_or_else(|| "help".to_string());
    if !args.is_empty() {
        args.remove(0);
    }
    let paths = AppPaths::discover()?;

    match command.as_str() {
        "login" => GraphBackend::login(&paths),
        "logout" => logout(&paths),
        "status" => print_status(&paths),
        "sync" => sync(&paths),
        "mount" => mount(paths),
        "pin" => pin(&paths, required_path(&args)?),
        "unpin" => unpin(&paths, required_path(&args)?),
        "release" => release(&paths, required_path(&args)?),
        "cache" if args.first().map(String::as_str) == Some("prune") => prune_cache(&paths),
        "status-path" => status_path(&paths, required_path(&args)?),
        "open-folder" => open_folder(&paths),
        "settings" => open_settings(),
        "init-mock" => init_mock(&paths),
        "mount-mock" => mount_mock(paths),
        "version" | "--version" | "-V" => {
            println!("twodrive {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "help" | "-h" | "--help" => {
            print_help();
            Ok(())
        }
        other => {
            print_help();
            anyhow::bail!("unknown command: {other}")
        }
    }
}
