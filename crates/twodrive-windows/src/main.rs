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
        "usage: twodrive-engine serve|ipc|list --state ABSOLUTE_DIRECTORY [--drive ID --item ID | --next QUERY_ID]"
    );
    let root = PathBuf::from(&args[2]);
    anyhow::ensure!(
        root.is_absolute(),
        "explicit absolute state directory required"
    );
    match args[0].as_str() {
        "index" | "refresh" | "tasks" | "download" | "cancel-download" => {
            let command = match args[0].as_str() {
                "refresh" => Command::RefreshIndex,
                "index" => Command::IndexPage {
                    offset: args.get(3).map(|s| s.parse()).transpose()?.unwrap_or(0),
                },
                "download" => Command::DownloadCloud {
                    id: args
                        .get(3)
                        .ok_or_else(|| anyhow::anyhow!("item ID required"))?
                        .clone(),
                },
                "cancel-download" => Command::CancelDownload {
                    id: args
                        .get(3)
                        .ok_or_else(|| anyhow::anyhow!("item ID required"))?
                        .clone(),
                },
                _ => Command::Snapshot,
            };
            let reply = ipc::request(
                &root,
                &Request {
                    version: VERSION,
                    id: format!(
                        "cli-{}-{}",
                        std::process::id(),
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)?
                            .as_nanos()
                    ),
                    command,
                },
            )
            .await?;
            anyhow::ensure!(
                reply.ok,
                "{}",
                reply.error.as_deref().unwrap_or("request_failed")
            );
            println!("{}", serde_json::to_string(&reply.snapshot.cloud)?);
        }
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
        "list" => {
            let mut drive_id = None;
            let mut item_id = None;
            let mut continuation = None;
            for option in args[3..].chunks(2) {
                anyhow::ensure!(option.len() == 2, "option requires a value");
                match option[0].as_str() {
                    "--drive" => drive_id = Some(option[1].clone()),
                    "--item" => item_id = Some(option[1].clone()),
                    "--next" => continuation = Some(option[1].clone()),
                    _ => anyhow::bail!("unknown list option"),
                }
            }
            anyhow::ensure!(
                continuation.is_none() || (drive_id.is_none() && item_id.is_none()),
                "next cannot be combined with drive/item"
            );
            let query_id = continuation.clone().unwrap_or_else(|| {
                format!("cli-{}-{}", std::process::id(), twodrive_core::now_unix())
            });
            let command = if continuation.is_some() {
                Command::BrowseNext {
                    query_id: query_id.clone(),
                }
            } else {
                Command::Browse {
                    query_id: query_id.clone(),
                    drive_id,
                    item_id,
                }
            };
            let request = Request {
                version: VERSION,
                id: format!("{query_id}-start"),
                command,
            };
            let mut reply = ipc::request(&root, &request).await?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
            loop {
                anyhow::ensure!(
                    reply.ok,
                    "{}",
                    reply.error.as_deref().unwrap_or("query_failed")
                );
                let view = reply
                    .snapshot
                    .directory
                    .as_ref()
                    .filter(|d| d.query_id == query_id)
                    .ok_or_else(|| anyhow::anyhow!("query_cancelled_or_replaced"))?;
                if view.status != "loading" {
                    println!("{}", serde_json::to_string(view)?);
                    anyhow::ensure!(
                        view.status != "failed",
                        "{}",
                        view.error.as_deref().unwrap_or("query_failed")
                    );
                    break;
                }
                anyhow::ensure!(std::time::Instant::now() < deadline, "query_timeout");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                reply = ipc::request(
                    &root,
                    &Request {
                        version: VERSION,
                        id: format!("{query_id}-poll"),
                        command: Command::Snapshot,
                    },
                )
                .await?;
            }
        }
        _ => anyhow::bail!("unknown command"),
    }
    Ok(())
}
