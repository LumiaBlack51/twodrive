use anyhow::ensure;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use twodrive_backend::GraphBackend;
use twodrive_peer::{
    identity::{nonce, platform},
    local,
    protocol::Message,
    runtime, update,
};

#[derive(Parser)]
#[command(
    version,
    about = "Isolated TwoDrive device control peer (no mount/service installation)"
)]
struct Cli {
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Microsoft OAuth/PKCE in a browser; uses the existing public Application ID.
    Login,
    /// Create a persistent identity and print its fingerprint for out-of-band verification.
    Init,
    /// Discover currently present devices (cloud discovery does not imply trust).
    Peers,
    /// Trust a full fingerprint verified on the OTHER device's local console.
    Trust {
        fingerprint: String,
    },
    Untrust {
        fingerprint: String,
    },
    /// Separate foreground supervisor/worker processes. No service or FUSE mount.
    Run {
        #[arg(long)]
        auto_update: bool,
    },
    /// Queue an encrypted test ping for a trusted peer; run must be active.
    Ping {
        #[arg(long)]
        to: String,
    },
    RequestStatus {
        #[arg(long)]
        to: String,
    },
    /// Send a release TAG hint. Receiver independently verifies the publisher's signature.
    NotifyUpdate {
        #[arg(long)]
        to: String,
        #[arg(long)]
        tag: String,
    },
    Status,
    /// Local-only directory authorization. This control-only version never scans files.
    AllowDirectory {
        path: PathBuf,
    },
    ClearDirectories,
    /// Verify and stage a downloaded release package using the pinned release public key.
    StageUpdate {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        binary: PathBuf,
    },
    /// Fetch a signed release from the fixed TwoDrive repository and stage it.
    Update {
        #[arg(long)]
        tag: String,
    },
    #[command(hide = true)]
    Worker {
        #[arg(long)]
        auto_update: bool,
        #[arg(long)]
        ready: PathBuf,
    },
    #[command(hide = true)]
    HealthCheck {
        #[arg(long)]
        output: PathBuf,
    },
}
fn main() {
    if let Err(error) = run() {
        // Never dump Graph/OAuth response bodies, tokens, URLs or private local paths.
        eprintln!(
            "TwoDrive Peer: operation failed. {}",
            if error.to_string().starts_with("peer startup") {
                "Run login, then run."
            } else {
                "Check the command, login, connectivity and local peer state."
            }
        );
        std::process::exit(1);
    }
}
fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Action::HealthCheck { output } = &cli.command {
        let a = twodrive_peer::identity::Identity::generate();
        let b = twodrive_peer::identity::Identity::generate();
        let session = nonce();
        let peer = b.presence(&session, 1000);
        let env = twodrive_peer::crypto::seal(&a, &session, &peer, Message::StatusRequest, 1000)?;
        ensure!(
            twodrive_peer::crypto::open(&b, &session, &a.presence(&session, 1000), &env, 1000)?
                == Message::StatusRequest,
            "self test failed"
        );
        return local::save(
            output,
            &serde_json::json!({"ok":true,"version":twodrive_peer::VERSION,"platform":platform()}),
        );
    }
    let home = local::prepare(&cli.state_dir.unwrap_or(local::default_home()?))?;
    match cli.command {
        Action::Login => {
            let _lock = local::lock(&home, "worker.lock")?;
            let paths = local::app_paths(&home);
            std::fs::create_dir_all(&paths.config_dir)?;
            let mut config = twodrive_core::Config::load_or_create(&paths)?;
            if !config
                .graph
                .scopes
                .iter()
                .any(|s| s == "Files.ReadWrite.AppFolder")
            {
                config.graph.scopes.push("Files.ReadWrite.AppFolder".into());
                config.save(&paths)?;
            }
            GraphBackend::login(&paths)
        }
        Action::Init => {
            let _lock = local::lock(&home, "worker.lock")?;
            println!("Device fingerprint: {}", runtime::identity(&home)?.id());
            println!("State directory: {}", home.display());
            Ok(())
        }
        Action::Peers => {
            let graph = GraphBackend::from_paths(&local::app_paths(&home))?;
            for p in runtime::discover(&graph, twodrive_core::now_unix())? {
                println!("{}  {}  {}", p.device, p.platform, p.version);
            }
            Ok(())
        }
        Action::Trust { fingerprint } => runtime::trust(&home, &fingerprint, false),
        Action::Untrust { fingerprint } => runtime::trust(&home, &fingerprint, true),
        Action::Run { auto_update } => runtime::supervise(&home, auto_update),
        Action::Ping { to } => runtime::queue(&home, &to, Message::Ping { nonce: nonce() }),
        Action::RequestStatus { to } => runtime::queue(&home, &to, Message::StatusRequest),
        Action::NotifyUpdate { to, tag } => {
            update::validate_tag(&tag)?;
            runtime::queue(&home, &to, Message::UpdateAvailable { tag })
        }
        Action::Status => {
            println!(
                "{}",
                String::from_utf8(local::read(&home.join("status.json"), 1024 * 1024)?)?
            );
            Ok(())
        }
        Action::AllowDirectory { path } => {
            let _lock = local::lock(&home, "worker.lock")?;
            local::select_root(&home, &path)
        }
        Action::ClearDirectories => {
            let _lock = local::lock(&home, "worker.lock")?;
            local::save(&home.join("selected-roots.json"), &Vec::<PathBuf>::new())
        }
        Action::StageUpdate { manifest, binary } => update::stage_local(&home, &manifest, &binary),
        Action::Update { tag } => update::download_and_stage(&home, &tag),
        Action::Worker { auto_update, ready } => {
            let code = runtime::worker(&home, auto_update, Some(&ready))?;
            std::process::exit(code)
        }
        Action::HealthCheck { .. } => unreachable!(),
    }
}
