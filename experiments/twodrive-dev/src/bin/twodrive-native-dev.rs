#[cfg(all(windows, feature = "native-sync"))]
#[path = "../native_runtime_cli.rs"]
mod native_runtime;
// Native staging and an explicit writable runtime; existing roots are not adopted.
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
        /// Move this provider's disconnected root with a sealed recovery intent.
        Relocate {
            #[arg(long)]
            native_state: PathBuf,
            #[arg(long)]
            root: PathBuf,
        },
        /// Copy service-verified complete personal replicas into the staged root.
        SourceCopy {
            #[arg(long)]
            native_state: PathBuf,
            #[arg(long, default_value_t = 0)]
            max_files: usize,
        },
        /// Verify a small sample of complete source data against service checksums.
        SourceVerify {
            #[arg(long)]
            native_state: PathBuf,
            #[arg(long,default_value_t=20,value_parser=clap::value_parser!(u64).range(1..=20))]
            sample: u64,
        },
        /// Inspect the matched personal client's source metadata, without content reads.
        SourceAudit {
            #[arg(long)]
            native_state: PathBuf,
        },
        /// Continuously synchronize a previously verified, account-bound root.
        Run {
            #[arg(long)]
            native_state: PathBuf,
            /// Zero runs until Ctrl+C or the stop command.
            #[arg(long, default_value_t = 0)]
            seconds: u64,
        },
        /// Request a graceful stop without opening a competing cache owner.
        Stop {
            #[arg(long)]
            native_state: PathBuf,
        },
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
            Command::Relocate { native_state, root } => {
                let mut session = NativeSession::relocate(&native_state, &root)?;
                println!(
                    "{}",
                    serde_json::json!({"phase":"owned_root_relocated","root":session.coordinator.root(),"summary":session.coordinator.summary()?})
                );
                session.coordinator.disconnect()?;
            }
            Command::SourceCopy {
                native_state,
                max_files,
            } => {
                let mut session = NativeSession::resume(&native_state)?;
                session.refresh_remote()?;
                let summary = session.copy_personal_source(max_files)?;
                println!(
                    "{}",
                    serde_json::json!({"phase":"verified_source_copy","source_mutations":false,"cloud_content_uploads":false,"summary":summary})
                );
                session.coordinator.disconnect()?;
            }
            Command::SourceVerify {
                native_state,
                sample,
            } => {
                let mut session = NativeSession::resume(&native_state)?;
                let summary = session.verify_personal_source_sample(usize::try_from(sample)?)?;
                println!(
                    "{}",
                    serde_json::json!({"phase":"personal_source_checksum_sample","cloud_content_downloads":false,"source_mutations":false,"summary":summary})
                );
                session.coordinator.disconnect()?;
            }
            Command::SourceAudit { native_state } => {
                let mut session = NativeSession::resume(&native_state)?;
                let summary = match session.audit_personal_source() {
                    Ok(summary) => summary,
                    Err(error) => {
                        if let Some(detail) = error
                            .downcast_ref::<twodrive_dev::native_coordinator::SourceAuditFailure>(
                        ) {
                            eprintln!(
                                "{}",
                                serde_json::json!({"phase":"personal_source_metadata_failed","diagnostic":detail,"content_reads":false,"source_mutations":false})
                            );
                        }
                        return Err(error);
                    }
                };
                println!(
                    "{}",
                    serde_json::json!({"phase":"personal_source_metadata","content_reads":false,"source_mutations":false,"summary":summary})
                );
                session.coordinator.disconnect()?;
            }
            Command::Run {
                native_state,
                seconds,
            } => crate::native_runtime::run(native_state, seconds)?,
            Command::Stop { native_state } => crate::native_runtime::stop(native_state)?,
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
