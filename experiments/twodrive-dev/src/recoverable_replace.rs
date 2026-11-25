//! Journaled replacement for providers without atomic compare-and-swap.
//! Every destructive step follows verified staging; ambiguous results preserve
//! both remote versions and the private before/edited recovery files.
use crate::{
    model::{CloudBackend, MetadataEntry},
    provider::Progress,
    state::{State, random_secret},
    write_recovery::{self, Recovery},
};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

pub(crate) trait Operations: CloudBackend {
    fn identity(&self, entry: &MetadataEntry) -> anyhow::Result<String>;
    fn create_staged(&self, path: &str, source: &Path, progress: &Progress) -> anyhow::Result<()>;
    /// Must refuse an observed destination, and check the source identity/version.
    /// This is best effort, not an atomic provider-side precondition.
    fn move_staged(&self, entry: &MetadataEntry, to: &str) -> anyhow::Result<()>;
}
#[derive(Serialize, Deserialize)]
struct Journal {
    path: String,
    hash: String,
    expected: Option<MetadataEntry>,
    recovery: std::path::PathBuf,
    temporary: String,
    previous: String,
    staged_id: Option<String>,
    completed: bool,
}
fn digest(source: &Path) -> anyhow::Result<String> {
    let mut input = File::open(source)?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
pub(crate) fn replace<B: Operations>(
    backend: &B,
    root: &State,
    path: &str,
    source: &Path,
    id: Option<&str>,
    version: Option<&str>,
    progress: Progress,
) -> anyhow::Result<MetadataEntry> {
    let hash = digest(source)?;
    let key = format!("replace-{:x}.json", Sha256::digest(path.as_bytes()));
    let mut journal: Journal = if root.dir.join(&key).exists() {
        root.read(&key)?
    } else {
        prepare(backend, root, path, source, (id, version), &hash, &progress)?
    };
    if journal.completed
        && journal.hash == hash
        && journal.expected.as_ref().map(|m| m.remote_id.as_str()) == id
        && journal.expected.as_ref().map(|m| m.etag.as_str()) == version
    {
        let current = backend
            .get_metadata_by_path(path)?
            .context("已确认的替换目标消失")?;
        ensure!(
            journal.staged_id.as_ref() == Some(&backend.identity(&current)?),
            "已确认目标身份改变，未重放替换"
        );
        let receipt = Recovery {
            state: State::new(journal.recovery.clone())?,
            edited: Some(source.to_path_buf()),
        };
        receipt.verify(backend, &current, &mut || progress(0, current.size))?;
        return Ok(current);
    }
    if journal.completed {
        // A later generation must start from its own freshly checked baseline.
        journal = prepare(backend, root, path, source, (id, version), &hash, &progress)?;
    }
    ensure!(
        journal.path == path
            && journal.hash == hash
            && journal.expected.as_ref().map(|m| m.remote_id.as_str()) == id
            && journal.expected.as_ref().map(|m| m.etag.as_str()) == version,
        crate::provider_guard::VersionConflict("存在未完成替换，请先恢复；两个版本均已保留")
    );
    root.save(&key, &journal)?;
    let recovery = Recovery {
        state: State::new(journal.recovery.clone())?,
        edited: Some(journal.recovery.join("edited.bin")),
    };
    ensure!(
        digest(recovery.edited.as_ref().unwrap())? == hash,
        "恢复副本校验失败，未替换"
    );
    let size = std::fs::metadata(source)?.len();
    let mut check = || progress(0, size);
    // A lost response after the final move is recognized only by our saved
    // staged identity AND content hash, never by the target filename alone.
    if let Some(current) = backend.get_metadata_by_path(path)?
        && journal
            .staged_id
            .as_ref()
            .is_some_and(|id| backend.identity(&current).as_ref().ok() == Some(id))
    {
        recovery.verify(backend, &current, &mut check)?;
        journal.completed = true;
        root.save(&key, &journal)?;
        recovery.complete(path)?;
        return Ok(current);
    }
    let staged = match backend.get_metadata_by_path(&journal.temporary)? {
        Some(entry) => entry,
        None => {
            ensure!(
                journal.staged_id.is_none(),
                "已暂存文件消失，请检查恢复记录"
            );
            backend.create_staged(
                &journal.temporary,
                recovery.edited.as_ref().unwrap(),
                &progress,
            )?;
            backend
                .get_metadata_by_path(&journal.temporary)?
                .context("尚未确认临时文件，恢复数据保留")?
        }
    };
    recovery.verify(backend, &staged, &mut check)?;
    let staged_id = backend.identity(&staged)?;
    ensure!(
        journal.staged_id.as_ref().is_none_or(|id| id == &staged_id),
        "临时文件身份已改变，未替换"
    );
    journal.staged_id = Some(staged_id.clone());
    root.save(&key, &journal)?;
    if let Some(expected) = &journal.expected {
        match backend.get_metadata_by_path(&journal.previous)? {
            Some(previous) => ensure!(
                backend.identity(&previous)? == backend.identity(expected)?
                    && previous.etag == expected.etag,
                "恢复路径已改变，两个版本保留，未继续替换"
            ),
            None => {
                write_recovery::snapshot(backend, path, id, version)?;
                backend.move_staged(expected, &journal.previous)?;
                let previous = backend
                    .get_metadata_by_path(&journal.previous)?
                    .context("原文件移动未确认，恢复副本保留")?;
                ensure!(
                    backend.identity(&previous)? == backend.identity(expected)?
                        && previous.etag == expected.etag,
                    "原文件身份已改变，停止替换并保留两份内容"
                );
            }
        }
    }
    ensure!(
        backend.get_metadata_by_path(path)?.is_none(),
        "目标出现其他版本，临时和原始副本均已保留"
    );
    check()?;
    backend.move_staged(&staged, path)?;
    let uploaded = backend
        .get_metadata_by_path(path)?
        .context("未确认替换结果，恢复副本保留")?;
    ensure!(
        backend.identity(&uploaded)? == staged_id,
        "目标身份改变，恢复副本保留"
    );
    recovery.verify(backend, &uploaded, &mut check)?;
    journal.completed = true;
    root.save(&key, &journal)?;
    // Keep the previous cloud object as recovery evidence. No garbage collector
    // is allowed to remove it before a separately verified retention decision.
    recovery.complete(path)?;
    progress(size, size)?;
    Ok(uploaded)
}
fn prepare<B: Operations>(
    backend: &B,
    root: &State,
    path: &str,
    source: &Path,
    baseline: (Option<&str>, Option<&str>),
    hash: &str,
    progress: &Progress,
) -> anyhow::Result<Journal> {
    let (id, version) = baseline;
    let expected = write_recovery::snapshot(backend, path, id, version)?;
    let size = std::fs::metadata(source)?.len();
    let recovery = Recovery::prepare(
        backend,
        &root.dir,
        path,
        Some(source),
        expected.as_ref(),
        &mut || progress(0, size),
    )?;
    let parent = path.rsplit_once('/').context("invalid replacement path")?.0;
    let nonce = random_secret();
    Ok(Journal {
        path: path.into(),
        hash: hash.into(),
        expected,
        recovery: recovery.state.dir,
        temporary: format!("{parent}/.twodrive-{nonce}.upload"),
        previous: format!("{parent}/.twodrive-{nonce}.previous"),
        staged_id: None,
        completed: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use twodrive_backend::MockBackend;
    struct Fixture {
        inner: MockBackend,
        step: AtomicUsize,
        fail: AtomicUsize,
        after: bool,
        collide: bool,
    }
    impl Fixture {
        fn new(fail: usize, after: bool) -> Self {
            Self {
                inner: MockBackend::new(),
                step: AtomicUsize::new(0),
                fail: AtomicUsize::new(fail),
                after,
                collide: false,
            }
        }
        fn operation<T>(&self, action: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
            let step = self.step.fetch_add(1, Ordering::SeqCst) + 1;
            let fails = self.fail.load(Ordering::SeqCst) == step;
            ensure!(
                !fails || self.after,
                "synthetic interruption before mutation"
            );
            let value = action()?;
            ensure!(!fails || !self.after, "synthetic lost mutation response");
            Ok(value)
        }
    }
    impl CloudBackend for Fixture {
        fn list_all(&self) -> anyhow::Result<Vec<MetadataEntry>> {
            self.inner.list_all()
        }
        fn download(&self, id: &str) -> anyhow::Result<Vec<u8>> {
            self.inner.download(id)
        }
        fn upload(&self, path: &str, bytes: Vec<u8>) -> anyhow::Result<MetadataEntry> {
            let created = self
                .inner
                .upload(&format!("/mock-{}", random_secret()), bytes)?;
            self.inner.rename(&created.remote_id, path)
        }
        fn create_folder(&self, path: &str) -> anyhow::Result<MetadataEntry> {
            self.inner.create_folder(path)
        }
        fn rename(&self, id: &str, to: &str) -> anyhow::Result<MetadataEntry> {
            self.inner.rename(id, to)
        }
        fn delete(&self, id: &str) -> anyhow::Result<()> {
            self.inner.delete(id)
        }
    }
    impl Operations for Fixture {
        fn identity(&self, entry: &MetadataEntry) -> anyhow::Result<String> {
            Ok(entry.remote_id.clone())
        }
        fn create_staged(
            &self,
            path: &str,
            source: &Path,
            progress: &Progress,
        ) -> anyhow::Result<()> {
            self.operation(|| {
                ensure!(self.get_metadata_by_path(path)?.is_none(), "collision");
                self.upload(path, std::fs::read(source)?)?;
                progress(3, 3)
            })
        }
        fn move_staged(&self, entry: &MetadataEntry, to: &str) -> anyhow::Result<()> {
            self.operation(|| {
                if self.collide && to == "/target" {
                    self.upload(to, b"foreign".to_vec())?;
                }
                write_recovery::snapshot(
                    self,
                    &entry.path,
                    Some(&entry.remote_id),
                    Some(&entry.etag),
                )?;
                ensure!(
                    self.get_metadata_by_path(to)?.is_none(),
                    "destination collision"
                );
                self.rename(&entry.remote_id, to)?;
                Ok(())
            })
        }
    }
    #[test]
    fn every_mutation_boundary_recovers_without_overwriting_unknown_content() {
        for step in 1..=3 {
            for after in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let state = State::new(temp.path().join("recovery")).unwrap();
                let source = temp.path().join("source");
                std::fs::write(&source, b"new").unwrap();
                let backend = Fixture::new(step, after);
                let old = backend.upload("/target", b"old".to_vec()).unwrap();
                let run = || {
                    replace(
                        &backend,
                        &state,
                        "/target",
                        &source,
                        Some(&old.remote_id),
                        Some(&old.etag),
                        Arc::new(|_, _| Ok(())),
                    )
                };
                assert!(run().is_err(), "step {step} after={after}");
                backend.fail.store(0, Ordering::SeqCst);
                let uploaded = run().unwrap();
                assert_eq!(backend.download(&uploaded.remote_id).unwrap(), b"new");
                let previous = backend
                    .list_all()
                    .unwrap()
                    .into_iter()
                    .find(|m| m.path.ends_with(".previous"))
                    .unwrap();
                assert_eq!(backend.download(&previous.remote_id).unwrap(), b"old");
                // Retrying even an acknowledged receipt must not make another copy.
                let before = backend.step.load(Ordering::SeqCst);
                run().unwrap();
                assert_eq!(before, backend.step.load(Ordering::SeqCst));
            }
        }
    }
    #[test]
    fn replacement_conflict_preserves_foreign_target_staging_and_original() {
        let temp = tempfile::tempdir().unwrap();
        let state = State::new(temp.path().join("recovery")).unwrap();
        let source = temp.path().join("source");
        std::fs::write(&source, b"new").unwrap();
        let mut backend = Fixture::new(0, false);
        backend.collide = true;
        let old = backend.upload("/target", b"old".to_vec()).unwrap();
        assert!(
            replace(
                &backend,
                &state,
                "/target",
                &source,
                Some(&old.remote_id),
                Some(&old.etag),
                Arc::new(|_, _| Ok(()))
            )
            .is_err()
        );
        let all = backend.list_all().unwrap();
        for (suffix, bytes) in [
            ("/target", b"foreign".as_slice()),
            (".upload", b"new".as_slice()),
            (".previous", b"old".as_slice()),
        ] {
            let entry = all.iter().find(|m| m.path.ends_with(suffix)).unwrap();
            assert_eq!(backend.download(&entry.remote_id).unwrap(), bytes);
        }
        assert!(
            std::fs::read_dir(&state.dir)
                .unwrap()
                .flatten()
                .any(|e| e.path().join("edited.bin").is_file())
        );
    }
    #[test]
    fn an_untracked_target_is_never_adopted_by_a_backup_create() {
        let temp = tempfile::tempdir().unwrap();
        let state = State::new(temp.path().join("recovery")).unwrap();
        let source = temp.path().join("source");
        std::fs::write(&source, b"new").unwrap();
        let backend = Fixture::new(0, false);
        backend.upload("/target", b"foreign".to_vec()).unwrap();
        assert!(
            replace(
                &backend,
                &state,
                "/target",
                &source,
                None,
                None,
                Arc::new(|_, _| Ok(()))
            )
            .is_err()
        );
        assert_eq!(backend.step.load(Ordering::SeqCst), 0);
    }
}
