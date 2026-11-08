//! Explicit native staging/observation commands. No existing-root adoption,
//! startup installation or automatic cloud mutation is exposed here.
#[cfg(all(windows, feature = "native-sync"))]
mod enabled {
    use anyhow::ensure;
    use clap::{Parser, Subcommand};
    use std::{
        path::PathBuf,
        time::{Duration, Instant},
    };
    use twodrive_dev::native_coordinator::{CreateNativeSession, NativeSession};
    #[derive(Parser)]
    #[command(
        version,
        about = "TwoDrive development native staging; original roots are preserved"
    )]
    struct Cli {
        #[command(subcommand)]
        command: Command,
    }
    #[derive(Subcommand)]
    enum Command {
        /// Publish a new independent root from an account-verified catalogue.
        Prepare {
            #[arg(long)]
            account: PathBuf,
            #[arg(long)]
            expected_owner: String,
            #[arg(long)]
            catalogue: PathBuf,
            #[arg(long)]
            native_state: PathBuf,
            #[arg(long)]
            root: PathBuf,
        },
        /// Reconnect, verify all native identities without content reads, disconnect.
        Verify {
            #[arg(long)]
            native_state: PathBuf,
        },
        /// Keep verified hydration/observations online; never upload/move/delete.
        Observe {
            #[arg(long)]
            native_state: PathBuf,
            #[arg(long,default_value_t=30,value_parser=clap::value_parser!(u64).range(1..=86400))]
            seconds: u64,
        },
    }
    pub fn run() -> anyhow::Result<()> {
        match Cli::parse().command {
            Command::Prepare {
                account,
                expected_owner,
                catalogue,
                native_state,
                root,
            } => {
                let mut session = NativeSession::create(CreateNativeSession {
                    account,
                    expected_owner,
                    catalogue,
                    state: native_state,
                    root,
                })?;
                println!(
                    "{}",
                    serde_json::json!({"phase":"prepared","state":session.state_directory(),"root":session.coordinator.root(),"summary":session.coordinator.summary()?})
                );
                session.coordinator.disconnect()?;
            }
            Command::Verify { native_state } => {
                let mut session = NativeSession::resume(&native_state)?;
                session.coordinator.verify_bootstrap_inventory()?;
                println!(
                    "{}",
                    serde_json::json!({"phase":"verified","summary":session.coordinator.summary()?})
                );
                session.coordinator.disconnect()?;
            }
            Command::Observe {
                native_state,
                seconds,
            } => {
                ensure!(
                    (1..=86400).contains(&seconds),
                    "bounded observation required"
                );
                let mut session = NativeSession::resume(&native_state)?;
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                println!(
                    "{}",
                    serde_json::json!({"phase":"observing","cloud_mutation_consumers":false,"summary":session.coordinator.summary()?})
                );
                runtime.block_on(async {
                    let deadline=Instant::now()+Duration::from_secs(seconds);
                    loop {
                        session.coordinator.poll_observations(0)?;
                        if Instant::now()>=deadline {break;}
                        tokio::select!{signal=tokio::signal::ctrl_c()=>{signal?;break;},_=tokio::time::sleep(Duration::from_millis(100))=>{}}
                    }
                    Ok::<_,anyhow::Error>(())
                })?;
                println!(
                    "{}",
                    serde_json::json!({"phase":"observation_finished","summary":session.coordinator.summary()?})
                );
                session.coordinator.disconnect()?;
            }
        }
        Ok(())
    }
}
#[cfg(all(windows, feature = "native-sync"))]
fn main() {
    if let Err(error) = enabled::run() {
        eprintln!("{}", twodrive_backend::diagnostics::safe_error(&error));
        std::process::exit(1);
    }
}
#[cfg(not(all(windows, feature = "native-sync")))]
fn main() {
    eprintln!("requires Windows and --features native-sync");
    std::process::exit(2);
}
