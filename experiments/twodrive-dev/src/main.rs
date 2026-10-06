use anyhow::{Context, ensure};
use clap::{Args, Parser, Subcommand};
use serde::de::DeserializeOwned;
use std::{
    fs,
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
};
use tokio::net::TcpListener;
use twodrive_dev::model::CloudBackend;
use twodrive_dev::{
    peer::{self, Network, PeerClient},
    state::{Credentials, Invitation, State},
    webdav::WebDavBackend,
};

#[cfg(all(feature = "mount", target_os = "linux"))]
mod mount;

#[derive(Parser)]
#[command(
    version,
    about = "Experimental WebDAV and encrypted QUIC sharing; separate from stable TwoDrive"
)]
struct Cli {
    /// Dedicated private state; never use the stable TwoDrive data directory.
    #[arg(long, global = true)]
    state: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Args, Clone)]
struct NetworkArgs {
    /// Custom HTTPS iroh relay (repeatable); disables public DNS discovery.
    #[arg(long)]
    relay: Vec<String>,
    /// Offline/LAN mode; disables public relay and discovery.
    #[arg(long, conflicts_with_all = ["relay", "relay_only"])]
    no_relay: bool,
    /// Use only encrypted relay transport, useful for diagnostics.
    #[arg(long)]
    relay_only: bool,
}
impl From<NetworkArgs> for Network {
    fn from(args: NetworkArgs) -> Self {
        Self {
            relays: args.relay,
            no_relay: args.no_relay,
            relay_only: args.relay_only,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Export one directory over authenticated, encrypted QUIC.
    Serve {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        write: bool,
        #[arg(long)]
        invite_file: Option<PathBuf>,
        #[arg(long, default_value_t = 8 * 1024 * 1024 * 1024)]
        max_upload: u64,
        #[command(flatten)]
        network: NetworkArgs,
    },
    /// Issue/replace the ten-minute single-use invitation for this server.
    Invite {
        #[arg(long)]
        out: PathBuf,
    },
    /// Pair or reconnect, exposing the peer as password-protected loopback WebDAV.
    Connect {
        #[arg(long)]
        ticket_file: Option<PathBuf>,
        #[arg(long, default_value = "127.0.0.1:4918")]
        listen: SocketAddr,
        /// Also mount using the isolated Linux FUSE adapter (requires --features mount).
        #[arg(long)]
        mount: Option<PathBuf>,
        #[arg(long)]
        write: bool,
        #[command(flatten)]
        network: NetworkArgs,
    },
    /// List device IDs authorized for this share.
    Peers,
    /// Revoke a device; subsequent requests on existing connections are denied.
    Revoke { device: String },
    /// Serve authenticated WebDAV on loopback; use Serve for remote encrypted access.
    DavServe {
        #[arg(long)]
        root: PathBuf,
        #[arg(long, default_value = "127.0.0.1:4918")]
        listen: SocketAddr,
        #[arg(long)]
        write: bool,
        #[arg(long, default_value_t = 8 * 1024 * 1024 * 1024)]
        max_upload: u64,
    },
    /// Connect to an existing HTTPS WebDAV service or a local peer gateway.
    Webdav {
        #[arg(long)]
        url: String,
        /// Private JSON with username/password; never include passwords in the URL.
        #[arg(long)]
        auth_file: Option<PathBuf>,
        #[arg(long)]
        write: bool,
        #[command(subcommand)]
        action: DavAction,
    },
}
#[derive(Subcommand)]
enum DavAction {
    List,
    Get {
        path: String,
        output: PathBuf,
    },
    /// Creates a new file; overwrites require an explicit current ETag.
    Put {
        source: PathBuf,
        path: String,
        #[arg(long)]
        if_match: Option<String>,
    },
    Mkdir {
        path: String,
    },
    Mv {
        from: String,
        to: String,
    },
    Rm {
        path: String,
    },
    Mount {
        target: PathBuf,
    },
}

fn read_private<T: DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && !meta.is_symlink() && meta.len() <= 64 * 1024,
        "private input must be a regular file smaller than 64 KiB"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            meta.permissions().mode() & 0o077 == 0,
            "private input file must have mode 0600"
        );
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn default_state() -> anyhow::Result<PathBuf> {
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(data).join("twodrive-dev"));
    }
    #[cfg(windows)]
    if let Some(data) = std::env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(data).join("twodrive-dev"));
    }
    Ok(
        PathBuf::from(std::env::var_os("HOME").context("pass --state when HOME is unset")?)
            .join(".local/share/twodrive-dev"),
    )
}
fn print_gateway(address: SocketAddr, state: &State) {
    println!("WebDAV: http://{address}/");
    println!(
        "Credentials: {} (private JSON)",
        state.dir.join("credentials.json").display()
    );
}

fn mount_dav(
    state: State,
    target: PathBuf,
    backend: WebDavBackend,
    binding: String,
    writable: bool,
) -> anyhow::Result<()> {
    #[cfg(all(feature = "mount", target_os = "linux"))]
    {
        mount::mount(state, target, backend, binding, writable)
    }
    #[cfg(not(all(feature = "mount", target_os = "linux")))]
    {
        let _ = (state, target, backend, binding, writable);
        anyhow::bail!(
            "FUSE mounting requires Linux and a build with --features mount; loopback WebDAV works without it"
        )
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let state = State::new(match cli.state {
        Some(path) => path,
        None => default_state()?,
    })?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    match cli.command {
        Command::Webdav {
            url,
            auth_file,
            write,
            action,
        } => {
            let credentials = auth_file
                .map(|path| read_private::<Credentials>(&path))
                .transpose()?;
            let username = credentials
                .as_ref()
                .map(|auth| auth.username.clone())
                .unwrap_or_default();
            let backend = WebDavBackend::new(&url, credentials, write)?;
            match action {
                DavAction::List => {
                    for entry in backend.list_all()? {
                        println!(
                            "{}\t{}\t{}\t{}",
                            if entry.is_dir { "dir" } else { "file" },
                            entry.size,
                            entry.etag,
                            entry.path
                        );
                    }
                }
                DavAction::Get { path, output } => {
                    let parent = output
                        .parent()
                        .filter(|path| !path.as_os_str().is_empty())
                        .unwrap_or(Path::new("."));
                    let mut file = tempfile::NamedTempFile::new_in(parent)?;
                    backend.download_to(&path, &mut file, &mut |_| Ok(()))?;
                    file.flush()?;
                    file.as_file().sync_all()?;
                    file.persist_noclobber(output)?;
                }
                DavAction::Put {
                    source,
                    path,
                    if_match,
                } => {
                    backend.upload_file_with_version(
                        &path,
                        &source,
                        None,
                        if_match.as_deref(),
                        &mut |_, _| Ok(()),
                    )?;
                }
                DavAction::Mkdir { path } => {
                    backend.create_folder(&path)?;
                }
                DavAction::Mv { from, to } => {
                    backend.rename(&from, &to)?;
                }
                DavAction::Rm { path } => backend.delete(&path)?,
                DavAction::Mount { target } => {
                    let binding = format!("webdav:{}:{username}", backend.root_url());
                    mount_dav(state, target, backend, binding, write)?;
                }
            }
        }
        Command::Peers => {
            for id in state.control(|control| Ok(control.peers.clone()))? {
                println!("{id}");
            }
        }
        Command::Revoke { device } => {
            let id: iroh::EndpointId = device.parse()?;
            ensure!(
                state.control(|control| Ok(control.peers.remove(&id.to_string())))?,
                "device was not authorized"
            );
        }
        Command::Invite { out } => state.invite(&out)?,
        Command::Serve {
            root,
            write,
            invite_file,
            max_upload,
            network,
        } => {
            let _lock = state.run_lock()?;
            runtime.block_on(async {
                let endpoint = peer::endpoint(&state, &network.into()).await?;
                peer::prepare_share(&endpoint, &state, &root, write)?;
                if let Some(path) = invite_file { state.invite(&path)?; println!("Invitation saved (expires in 10 minutes)"); }
                println!("Serving device={} mode={}", endpoint.id(), if write { "read-write" } else { "read-only" });
                tokio::select! { result = peer::serve(endpoint.clone(), state, &root, write, max_upload) => result, _ = tokio::signal::ctrl_c() => { endpoint.close().await; Ok(()) } }
            })?;
        }
        Command::Connect {
            ticket_file,
            listen,
            mount,
            write,
            network,
        } => {
            ensure!(listen.ip().is_loopback(), "gateway must bind loopback");
            let _lock = state.run_lock()?;
            let invitation: Option<Invitation> =
                ticket_file.map(|path| read_private(&path)).transpose()?;
            let address = invitation
                .as_ref()
                .map(|ticket| ticket.address.clone())
                .map(Ok)
                .unwrap_or_else(|| state.read("remote.json"))?;
            let credentials = state.credentials()?;
            let endpoint = runtime.block_on(peer::endpoint(&state, &network.into()))?;
            let client = PeerClient::new(endpoint.clone(), address.clone());
            runtime.block_on(client.pair(&state, invitation))?;
            let listener = runtime.block_on(TcpListener::bind(listen))?;
            let address_local = listener.local_addr()?;
            print_gateway(address_local, &state);
            let gateway = runtime.spawn(peer::gateway(listener, credentials.clone(), client));
            if let Some(target) = mount {
                let backend = WebDavBackend::new(
                    &format!("http://{address_local}/"),
                    Some(credentials),
                    write,
                )?;
                let mount_state = State::new(state.dir.join("mount"))?;
                mount_dav(
                    mount_state,
                    target,
                    backend,
                    format!("peer:{}", address.id),
                    write,
                )?;
            } else {
                runtime.block_on(async { tokio::select! { result = gateway => result??, _ = tokio::signal::ctrl_c() => {} } Ok::<_, anyhow::Error>(()) })?;
            }
            runtime.block_on(endpoint.close());
        }
        Command::DavServe {
            root,
            listen,
            write,
            max_upload,
        } => {
            let _lock = state.run_lock()?;
            let root = root.canonicalize()?;
            ensure!(
                root.is_dir() && !state.dir.starts_with(&root),
                "share must be a directory separate from private state"
            );
            let credentials = state.credentials()?;
            runtime.block_on(async {
                let listener = TcpListener::bind(listen).await?; print_gateway(listener.local_addr()?, &state);
                tokio::select! { result = peer::local_webdav(listener, credentials, &root, write, max_upload) => result, _ = tokio::signal::ctrl_c() => Ok(()) }
            })?;
        }
    }
    Ok(())
}
