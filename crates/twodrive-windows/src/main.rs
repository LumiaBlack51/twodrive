use std::{io::Read, path::PathBuf};
use twodrive_windows::{engine::Engine, ipc, protocol::*};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--version") {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    anyhow::ensure!(
        args.len() >= 3 && args[1] == "--state",
        "usage: twodrive-engine serve|ipc --state ABSOLUTE_DIRECTORY [--mock]"
    );
    let root = PathBuf::from(&args[2]);
    anyhow::ensure!(
        root.is_absolute(),
        "explicit absolute state directory required"
    );
    match args[0].as_str() {
        "serve" => {
            anyhow::ensure!(
                args.len() == 3
                    || args.get(3).map(String::as_str) == Some("--mock") && args.len() == 4,
                "unknown serve option"
            );
            let _lock = ipc::lock(&root, "engine")?;
            let engine = Engine::open(&root, args.len() == 4)?;
            engine.start();
            ipc::listen(&root, engine).await?;
        }
        "ipc" => {
            anyhow::ensure!(args.len() == 3, "unknown ipc option");
            let mut input = Vec::new();
            std::io::stdin()
                .take(MAX_FRAME as u64 + 1)
                .read_to_end(&mut input)?;
            anyhow::ensure!(input.len() <= MAX_FRAME, "request_too_large");
            let request: Request = serde_json::from_slice(&input)?;
            let reply = ipc::request(&root, &request).await?;
            println!("{}", serde_json::to_string(&reply)?);
        }
        _ => anyhow::bail!("unknown command"),
    }
    Ok(())
}
