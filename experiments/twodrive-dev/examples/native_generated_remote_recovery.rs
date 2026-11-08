//! Retire only interrupted generated remote-version fixtures. No real provider.
#[cfg(all(windows, feature = "native-sync", feature = "native-test-hooks"))]
fn main() -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use std::{
        collections::HashMap,
        path::PathBuf,
        sync::{Arc, Mutex, atomic::AtomicBool},
    };
    use twodrive_local_files::native::{
        HydrationSource, NativeReadContext,
        persistent::{PersistentSession, RootReceipt},
    };
    const REMOTE_BYTES: &[u8] = b"generated newer remote complete content";
    const SOURCE_BYTES: &[u8] = b"generated initial provider content";
    const EXPIRED_BYTES: &[u8] = b"unrelated progress after expired identity";
    const RETIRED_BYTES: &[u8] = b"new work after retired hints";
    struct Generated {
        receipt: RootReceipt,
        bytes: &'static [u8],
        tags: Mutex<HashMap<Vec<u8>, u64>>,
    }
    impl HydrationSource for Generated {
        fn size(&self, tag: &[u8]) -> Result<usize, String> {
            if self.tags.lock().unwrap().contains_key(tag) {
                Ok(self.bytes.len())
            } else {
                Err("unowned generated tag".into())
            }
        }
        fn begin_native_read(
            &self,
            tag: &[u8],
            ctx: NativeReadContext<'_>,
            _: &AtomicBool,
        ) -> Result<Option<Box<dyn twodrive_local_files::native::HydrationRead>>, String> {
            if ctx.root_identity != self.receipt.identity
                || ctx.root_file_id != self.receipt.file_id
                || ctx.volume_serial != self.receipt.volume_serial
                || ctx.file_size != self.bytes.len() as u64
                || self.tags.lock().unwrap().get(tag) != Some(&ctx.file_id)
            {
                return Err("generated recovery physical binding changed".into());
            }
            Ok(None)
        }
        fn read_range(
            &self,
            tag: &[u8],
            offset: usize,
            length: usize,
            _: &AtomicBool,
        ) -> Result<Vec<u8>, String> {
            self.size(tag)?;
            self.bytes
                .get(offset..offset.checked_add(length).ok_or("range overflow")?)
                .map(|b| b.to_vec())
                .ok_or("generated range invalid".into())
        }
    }
    let mut args = std::env::args_os().skip(1);
    ensure!(
        args.next().as_deref()
            == Some(std::ffi::OsStr::new("--retire-known-generated-remote-root")),
        "generated retirement opt-in required"
    );
    let root = PathBuf::from(args.next().context("generated root required")?);
    let parent = std::env::temp_dir().canonicalize()?;
    ensure!(
        root.is_absolute()
            && root
                .parent()
                .context("root parent missing")?
                .canonicalize()?
                == parent
            && args.next().is_none(),
        "only a direct temporary generated root is accepted"
    );
    let name = root
        .file_name()
        .context("root name missing")?
        .to_string_lossy();
    let retired = name.starts_with("twodrive-cfapi-fixture-office-retired-");
    let expired = name.starts_with("twodrive-cfapi-fixture-office-expired-id-");
    let lifecycle = name.starts_with("twodrive-cfapi-fixture-office-lifecycle-");
    let suffix = name
        .strip_prefix(if retired {
            "twodrive-cfapi-fixture-office-retired-"
        } else if expired {
            "twodrive-cfapi-fixture-office-expired-id-"
        } else if lifecycle {
            "twodrive-cfapi-fixture-office-lifecycle-"
        } else {
            "twodrive-cfapi-fixture-office-pinned-remote-"
        })
        .context("fixture prefix mismatch")?;
    let bytes = if retired {
        RETIRED_BYTES
    } else if expired {
        EXPIRED_BYTES
    } else if lifecycle {
        SOURCE_BYTES
    } else {
        REMOTE_BYTES
    };
    let directory = if lifecycle {
        "B"
    } else {
        "remote-folder-renamed"
    };
    let relative = if retired {
        "after-retired-events.txt"
    } else if expired {
        "unrelated-after-delete.txt"
    } else if lifecycle {
        "B/generated-中文.txt"
    } else {
        "remote-folder-renamed/remote-renamed.txt"
    };
    ensure!(
        suffix.len() == 32 && suffix.bytes().all(|b| b.is_ascii_hexdigit()),
        "fixture nonce invalid"
    );
    let checkpoint = root.with_extension("receipt.json");
    let receipt: RootReceipt = serde_json::from_slice(&std::fs::read(&checkpoint)?)?;
    ensure!(
        receipt.identity == b"twodrive-dev-native-v1:generated-office-only",
        "fixture registration mismatch"
    );
    let source = Arc::new(Generated {
        receipt: receipt.clone(),
        bytes,
        tags: Mutex::default(),
    });
    let mut owner =
        PersistentSession::resume(&root, &receipt, source.clone()).map_err(anyhow::Error::msg)?;
    let entries = owner.inventory().map_err(anyhow::Error::msg)?;
    ensure!(
        entries.len() == if expired || retired { 1 } else { 2 },
        "unexpected generated inventory"
    );
    for entry in &entries {
        if entry.directory {
            ensure!(
                entry.relative_path == std::path::Path::new(directory),
                "unexpected generated directory"
            );
        } else {
            ensure!(
                entry.relative_path == std::path::Path::new(relative)
                    && entry.size == bytes.len() as u64,
                "unexpected generated file"
            );
            source.tags.lock().unwrap().insert(
                entry.identity.clone().context("generated tag absent")?,
                entry.file_id,
            );
        }
    }
    let path = root.join(relative);
    ensure!(
        std::fs::read(&path)? == bytes,
        "generated full byte comparison failed"
    );
    let retained = root.with_extension("retained");
    ensure!(!retained.exists(), "retention target exists");
    std::fs::create_dir(&retained)?;
    std::fs::rename(&path, retained.join("generated-remote.txt"))?;
    ensure!(
        std::fs::read(retained.join("generated-remote.txt"))? == bytes,
        "retained generated bytes differ"
    );
    if !expired && !retired {
        std::fs::remove_dir(root.join(directory))?;
    }
    owner
        .retire_empty_office_fixture()
        .map_err(anyhow::Error::msg)?;
    println!(
        "{}",
        serde_json::json!({"retired_generated_root":root,"retained_generated_bytes":retained,"receipt":checkpoint})
    );
    Ok(())
}
#[cfg(not(all(windows, feature = "native-sync", feature = "native-test-hooks")))]
fn main() {
    eprintln!("requires Windows native-sync/native-test-hooks");
    std::process::exit(2);
}
