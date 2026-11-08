//! Two-phase relocation of this provider's own root; existing destinations and
//! foreign registrations are never adopted or unregistered.
use super::*;
use twodrive_local_files::native::persistent::{ReadOnlyRegisteredRoot, relocate_owned_root};

#[derive(Serialize, Deserialize)]
struct Relocation {
    schema: u32,
    identity: CloudIdentity,
    receipt: RootReceipt,
    from: PathBuf,
    to: PathBuf,
    state: String,
}
fn owned_at(path: &Path, receipt: &RootReceipt) -> anyhow::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
        Ok(_) => {}
    }
    let root = ReadOnlyRegisteredRoot::open(path).map_err(anyhow::Error::msg)?;
    ensure!(
        root.receipt() == receipt,
        "relocation path is occupied by another identity"
    );
    Ok(true)
}
fn save(state: &Path, relocation: &Relocation) -> anyhow::Result<()> {
    twodrive_core::private_file::write_secret(
        &state.join("native-relocation.dat"),
        &serde_json::to_vec(relocation)?,
    )
}
pub(super) fn recover(state: &Path, configuration: &mut Configuration) -> anyhow::Result<()> {
    let path = state.join("native-relocation.dat");
    if !path.exists() {
        return Ok(());
    }
    let mut relocation: Relocation =
        serde_json::from_slice(&twodrive_core::private_file::read_secret(&path)?)?;
    ensure!(
        relocation.schema == 1
            && relocation.identity == configuration.identity
            && configuration.receipt.as_ref() == Some(&relocation.receipt)
            && relocation.receipt.identity == configuration.registration
            && relocation.from.is_absolute()
            && relocation.to.is_absolute()
            && (configuration.root == relocation.from || configuration.root == relocation.to),
        "relocation authority/configuration changed"
    );
    if relocation.state == "aborted" {
        ensure!(
            configuration.root == relocation.from
                && owned_at(&relocation.from, &relocation.receipt)?,
            "aborted relocation source changed"
        );
        return Ok(());
    }
    if relocation.state == "committed" {
        ensure!(
            configuration.root == relocation.to && owned_at(&relocation.to, &relocation.receipt)?,
            "committed relocation root changed"
        );
        return Ok(());
    }
    ensure!(
        relocation.state == "prepared",
        "unknown root relocation phase"
    );
    let from = owned_at(&relocation.from, &relocation.receipt)?;
    if from {
        ensure!(
            configuration.root == relocation.from,
            "prepared relocation source disagrees with configuration"
        );
        relocation.state = "aborted".into();
        save(state, &relocation)?;
        return Ok(());
    }
    let to = owned_at(&relocation.to, &relocation.receipt)?;
    ensure!(to, "relocation root identity is missing");
    if to {
        configuration.root = relocation.to.clone();
        NativeSession::seal(state, configuration)?;
        relocation.state = "committed".into();
        save(state, &relocation)?;
    } else {
        ensure!(
            configuration.root == relocation.from,
            "prepared relocation source disagrees with configuration"
        );
    }
    Ok(())
}
impl NativeSession {
    pub fn relocate(state: &Path, destination: &Path) -> anyhow::Result<Self> {
        ensure!(
            destination.is_absolute() && !destination.exists(),
            "new absolute root destination required"
        );
        let mut session = Self::resume(state)?;
        session.coordinator.verify_bootstrap_inventory()?;
        ensure!(
            !session.coordinator.bridge.journal().native_work_pending()?,
            "root relocation waits for durable work"
        );
        let mut relocation = Relocation {
            schema: 1,
            identity: session.configuration.identity.clone(),
            receipt: session.coordinator.receipt().clone(),
            from: session.configuration.root.clone(),
            to: destination.to_path_buf(),
            state: "prepared".into(),
        };
        save(&session.state, &relocation)?;
        session.coordinator.disconnect()?;
        let NativeSession {
            coordinator,
            state,
            mut configuration,
        } = session;
        let cache_owner = coordinator.bridge.clone();
        drop(coordinator);
        #[cfg(all(test, feature = "native-test-hooks"))]
        if COLLIDE_AFTER_PREPARE.with(|f| f.replace(false)) {
            std::fs::create_dir(&relocation.to)?;
            std::fs::write(
                relocation.to.join("generated-foreign-sentinel.txt"),
                b"generated collision must remain untouched",
            )?;
        }
        relocate_owned_root(&relocation.from, &relocation.to, &relocation.receipt)
            .map_err(anyhow::Error::msg)?;
        #[cfg(all(test, feature = "native-test-hooks"))]
        if INTERRUPT_AFTER_ROOT_MOVE.with(|f| f.replace(false)) {
            anyhow::bail!("generated interruption after owned root move");
        }
        configuration.root = relocation.to.clone();
        Self::seal(&state, &configuration)?;
        relocation.state = "committed".into();
        save(&state, &relocation)?;
        drop(cache_owner);
        Self::resume(&state)
    }
}
#[cfg(all(test, feature = "native-test-hooks"))]
thread_local! {pub(super) static INTERRUPT_AFTER_ROOT_MOVE:std::cell::Cell<bool>=const {std::cell::Cell::new(false)};}

#[cfg(all(test, feature = "native-test-hooks"))]
thread_local! {pub(super) static COLLIDE_AFTER_PREPARE:std::cell::Cell<bool>=const {std::cell::Cell::new(false)};}
