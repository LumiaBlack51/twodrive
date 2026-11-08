//! Generated-only real personal OneDrive namespace validation. No user path is
//! accepted. Failures retain a private receipt for exact-scope recovery.
#[cfg(windows)]
fn run() -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use std::{fs, path::PathBuf};
    use twodrive_dev::{model::CloudBackend, onedrive::OneDrive};
    let mut args = std::env::args_os().skip(1);
    ensure!(
        args.next().as_deref() == Some(std::ffi::OsStr::new("--allow-generated-cloud-write")),
        "explicit generated cloud write opt-in required"
    );
    let account = PathBuf::from(args.next().context("independent account state required")?);
    let expected = args
        .next()
        .context("expected personal owner required")?
        .to_string_lossy()
        .into_owned();
    let resume = match args.next() {
        None => None,
        Some(flag) => {
            ensure!(
                flag == "--resume-empty-generated-scope",
                "unknown validation option"
            );
            Some(PathBuf::from(
                args.next()
                    .context("retained generated receipt directory required")?,
            ))
        }
    };
    ensure!(
        account.is_absolute() && args.next().is_none(),
        "invalid generated validation arguments"
    );
    let provider = OneDrive::open(&account, true)?;
    let identity = provider.graph.readonly_identity(&mut || Ok(()))?;
    ensure!(
        identity.account.eq_ignore_ascii_case(&expected),
        "personal account mismatch"
    );
    let state = if resume.is_none() {
        Some(
            tempfile::Builder::new()
                .prefix("twodrive-native-namespace-")
                .tempdir()?,
        )
    } else {
        None
    };
    let private = match &state {
        Some(state) => state.path().to_path_buf(),
        None => {
            let requested = resume.as_ref().unwrap();
            ensure!(
                requested.is_absolute()
                    && !fs::symlink_metadata(requested)?.file_type().is_symlink(),
                "invalid recovery directory"
            );
            let path = requested.canonicalize()?;
            ensure!(
                path.parent() == Some(std::env::temp_dir().canonicalize()?.as_path())
                    && path.file_name().is_some_and(|name| name
                        .to_string_lossy()
                        .starts_with("twodrive-native-namespace-")),
                "recovery directory outside generated scope"
            );
            path
        }
    };
    let scope = if resume.is_some() {
        let receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(private.join("scope.json"))?)?;
        receipt["scope"]
            .as_str()
            .context("missing retained scope")?
            .to_string()
    } else {
        format!(
            "/TwoDrive-native-namespace-validation-{:032x}",
            rand::random::<u128>()
        )
    };
    let nonce = scope
        .strip_prefix("/TwoDrive-native-namespace-validation-")
        .context("foreign recovery scope")?;
    ensure!(
        nonce.len() == 32 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid generated scope nonce"
    );
    if resume.is_none() {
        fs::write(
            private.join("scope.json"),
            serde_json::to_vec(&serde_json::json!({"scope":scope}))?,
        )?;
    }
    let result = (|| -> anyhow::Result<()> {
        let folder = if resume.is_some() {
            ensure!(
                !private.join("objects.json").exists(),
                "only an empty pre-content scope may resume automatically"
            );
            let receipt: serde_json::Value =
                serde_json::from_slice(&fs::read(private.join("folder.json"))?)?;
            let folder = provider
                .get_metadata(
                    receipt["id"]
                        .as_str()
                        .context("missing generated folder identity")?,
                )?
                .context("generated folder disappeared")?;
            ensure!(
                folder.path == scope
                    && folder.is_dir
                    && receipt["path"].as_str() == Some(scope.as_str()),
                "retained scope binding changed"
            );
            let children = provider.graph.desktop_directory(&scope, &mut || Ok(()))?;
            for child in children {
                ensure!(
                    child.is_dir
                        && child.path == format!("{scope}/generated-child")
                        && provider
                            .graph
                            .desktop_directory(&child.path, &mut || Ok(()))?
                            .is_empty(),
                    "retained scope contains unexpected data"
                );
                provider.graph.delete_item_bound(
                    &identity,
                    &child.remote_id,
                    &child.etag,
                    &mut || Ok(()),
                )?;
            }
            folder
        } else {
            ensure!(
                provider.get_metadata_by_path(&scope)?.is_none(),
                "generated scope already exists"
            );
            provider.create_folder(&scope)?
        };
        fs::write(
            private.join("folder.json"),
            serde_json::to_vec(&serde_json::json!({"id":folder.remote_id,"path":folder.path}))?,
        )?;
        let child_path = format!("{scope}/generated-child");
        let child = provider.graph.create_directory_bound(
            &identity,
            &folder.remote_id,
            &child_path,
            &mut || Ok(()),
        )?;
        let duplicate = provider.graph.create_directory_bound(
            &identity,
            &folder.remote_id,
            &child_path,
            &mut || Ok(()),
        );
        ensure!(
            duplicate.is_err(),
            "existing generated directory was not refused"
        );
        let input = private.join("generated.bin");
        let contents = b"generated namespace content, preserved through identity-bound moves";
        fs::write(&input, contents)?;
        let path = format!("{scope}/generated-source.txt");
        let original =
            provider.upload_file_with_version(&path, &input, None, None, &mut |_, _| Ok(()))?;
        let blocked_path = format!("{child_path}/occupied.txt");
        fs::write(&input, b"generated destination must survive")?;
        let occupied =
            provider
                .upload_file_with_version(&blocked_path, &input, None, None, &mut |_, _| Ok(()))?;
        fs::write(
            private.join("objects.json"),
            serde_json::to_vec(
                &serde_json::json!({"child":child.remote_id,"source":original.remote_id,"occupied":occupied.remote_id}),
            )?,
        )?;
        ensure!(
            provider
                .graph
                .move_item_bound(
                    &identity,
                    &original.remote_id,
                    &original.etag,
                    &child.remote_id,
                    &blocked_path,
                    &mut || Ok(())
                )
                .is_err(),
            "name conflict unexpectedly succeeded"
        );
        ensure!(
            provider.download(&occupied.remote_id)? == b"generated destination must survive",
            "name conflict changed destination bytes"
        );
        ensure!(
            provider
                .get_metadata(&original.remote_id)?
                .context("source disappeared")?
                .path
                == path,
            "name conflict moved source"
        );
        let renamed_path = format!("{child_path}/renamed.txt");
        let moved = provider.graph.move_item_bound(
            &identity,
            &original.remote_id,
            &original.etag,
            &child.remote_id,
            &renamed_path,
            &mut || Ok(()),
        )?;
        ensure!(
            moved.remote_id == original.remote_id
                && moved.path == renamed_path
                && provider.download(&moved.remote_id)? == contents,
            "move identity or contents changed"
        );
        let stale_version = "\"twodrive-generated-noncurrent-version\"";
        ensure!(
            provider
                .graph
                .delete_item_bound(&identity, &moved.remote_id, stale_version, &mut || Ok(()))
                .is_err(),
            "stale delete was accepted"
        );
        let stale_move = provider.graph.move_item_bound(
            &identity,
            &moved.remote_id,
            stale_version,
            &folder.remote_id,
            &path,
            &mut || Ok(()),
        );
        ensure!(
            stale_move.is_err() && provider.download(&moved.remote_id)? == contents,
            "stale move changed contents"
        );
        for item in [&moved, &occupied] {
            let observed = provider
                .get_metadata(&item.remote_id)?
                .context("generated item missing before cleanup")?;
            ensure!(
                observed.path == item.path && observed.etag == item.etag,
                "generated cleanup receipt changed"
            );
            provider.graph.delete_item_bound(
                &identity,
                &observed.remote_id,
                &observed.etag,
                &mut || Ok(()),
            )?;
            ensure!(
                provider.get_metadata(&observed.remote_id)?.is_none(),
                "generated file cleanup unconfirmed"
            );
        }
        for directory in [&child, &folder] {
            ensure!(
                provider
                    .graph
                    .desktop_directory(&directory.path, &mut || Ok(()))?
                    .is_empty(),
                "unexpected generated child; cleanup stopped"
            );
            let observed = provider
                .get_metadata(&directory.remote_id)?
                .context("generated directory missing")?;
            ensure!(
                observed.is_dir && observed.path == directory.path,
                "generated directory moved"
            );
            provider.graph.delete_item_bound(
                &identity,
                &observed.remote_id,
                &observed.etag,
                &mut || Ok(()),
            )?;
        }
        ensure!(
            provider.get_metadata_by_path(&scope)?.is_none(),
            "generated scope retirement unconfirmed"
        );
        println!(
            "{}",
            serde_json::json!({"actual_personal_onedrive":true,"create_directory_by_parent_id":true,"name_conflict_preserves_both_objects":true,"move_keeps_id_and_bytes":true,"stale_move_and_delete_refused":true,"conditional_recycle_and_scope_cleanup":true,"production_native_consumer_enabled":false})
        );
        Ok(())
    })();
    if result.is_err() {
        eprintln!(
            "generated namespace recovery state retained at {}",
            state.map_or(private, |state| state.keep()).display()
        );
    } else {
        if let Some(state) = state {
            state.close()?;
        }
    }
    result
}
#[cfg(windows)]
fn main() {
    if let Err(error) = run() {
        eprintln!("{}", twodrive_backend::diagnostics::safe_error(&error));
        for cause in error.chain() {
            let message = cause.to_string();
            if message.starts_with("namespace receipt identity mismatch:")
                || message.starts_with("namespace readback mismatch:")
            {
                eprintln!("{message}");
            }
        }
        std::process::exit(1);
    }
}
#[cfg(not(windows))]
fn main() {
    eprintln!(
        "requires Windows with an independent personal account and explicit generated write opt-in"
    );
    std::process::exit(2);
}
