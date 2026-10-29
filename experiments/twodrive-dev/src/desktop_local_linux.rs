//! Metadata-only Linux preflight. No file content reads, copying or hydration.
use super::*;
use std::{fs, os::unix::fs::MetadataExt, path::Component};
use twodrive_local_files::migration::Disposition;

fn decode_mount_path(value: &str) -> std::io::Result<std::path::PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let mut output = Vec::new();
    let mut bytes = value.as_bytes().iter().copied();
    while let Some(byte) = bytes.next() {
        if byte == b'\\' {
            let mut decoded = 0u8;
            for _ in 0..3 {
                let digit = bytes
                    .next()
                    .filter(|b| (b'0'..=b'7').contains(b))
                    .ok_or_else(|| std::io::Error::other("invalid mount path"))?;
                decoded = decoded
                    .checked_mul(8)
                    .and_then(|n| n.checked_add(digit - b'0'))
                    .ok_or_else(|| std::io::Error::other("invalid mount path"))?;
            }
            output.push(decoded);
        } else {
            output.push(byte);
        }
    }
    Ok(std::ffi::OsString::from_vec(output).into())
}

fn filesystem<'a>(path: &Path, table: &'a str) -> std::io::Result<&'a str> {
    let mut best = None;
    for line in table.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let Some(target) = left.split_whitespace().nth(4) else {
            continue;
        };
        let target = decode_mount_path(target)?;
        if path.starts_with(&target)
            && best.is_none_or(|(length, _)| target.as_os_str().len() > length)
        {
            best = Some((
                target.as_os_str().len(),
                right
                    .split_whitespace()
                    .next()
                    .ok_or_else(|| std::io::Error::other("invalid mount table"))?,
            ));
        }
    }
    best.map(|(_, kind)| kind)
        .ok_or_else(|| std::io::Error::other("unknown filesystem"))
}

pub(super) fn inspect(path: &Path, kind: SourceKind) -> std::io::Result<Disposition> {
    inspect_with_mounts(path, kind, &fs::read_to_string("/proc/self/mountinfo")?)
}

fn inspect_with_mounts(
    path: &Path,
    kind: SourceKind,
    mounts: &str,
) -> std::io::Result<Disposition> {
    match kind {
        SourceKind::PrivateCache => return Ok(Disposition::UnsupportedCache),
        SourceKind::VirtualMount => return Ok(Disposition::UnsupportedVirtualMount),
        SourceKind::Unknown => return Ok(Disposition::UnknownProvenance),
        _ => {}
    }
    if !path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err(std::io::Error::other("non-normal absolute path"));
    }
    // Resolve the mount kind before touching the target inode. FUSE/network
    // attribute lookups can themselves request hydration from another client.
    let fs_kind = filesystem(path, mounts)?;
    if fs_kind.starts_with("fuse")
        || matches!(
            fs_kind,
            "nfs" | "nfs4" | "cifs" | "smb3" | "9p" | "autofs" | "ceph" | "afs"
        )
    {
        return Ok(Disposition::UnsupportedVirtualMount);
    }
    if !matches!(
        fs_kind,
        "ext2"
            | "ext3"
            | "ext4"
            | "btrfs"
            | "xfs"
            | "f2fs"
            | "zfs"
            | "tmpfs"
            | "ramfs"
            | "overlay"
            | "erofs"
    ) {
        return Ok(Disposition::UnknownProvenance);
    }
    let mut current = std::path::PathBuf::new();
    for part in path.components() {
        current.push(part.as_os_str());
        if fs::symlink_metadata(&current)?.file_type().is_symlink() {
            return Ok(Disposition::SkipReparseOrOffline);
        }
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        return Ok(Disposition::SkipDirectory);
    }
    if !metadata.is_file() || metadata.blocks().saturating_mul(512) < metadata.len() {
        return Ok(Disposition::SkipReparseOrOffline);
    }
    // Linux stat has no common cross-client "fully hydrated" contract.
    if kind == SourceKind::OtherClientSyncRoot {
        return Ok(Disposition::PlaceholderNeedsOriginalClientExport);
    }
    Ok(Disposition::FullFileNeedsApprovedPlan)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_virtual_mount_before_any_target_lookup_and_uses_longest_escaped_mount() {
        let table = "1 0 0:1 / / rw - ext4 /dev/local rw\n2 1 0:2 / /not\\040local rw - fuse.rclone TWODRIVE rw\n";
        assert_eq!(
            inspect_with_mounts(
                Path::new("/not local/does-not-exist"),
                SourceKind::OrdinaryDirectory,
                table
            )
            .unwrap(),
            Disposition::UnsupportedVirtualMount
        );
        assert_eq!(
            filesystem(Path::new("/not local/file"), table).unwrap(),
            "fuse.rclone"
        );
        assert_eq!(
            filesystem(Path::new("/not localish/file"), table).unwrap(),
            "ext4"
        );
    }
    #[test]
    fn local_metadata_refuses_symlink_parents_sparse_and_unverified_sync_sources() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("files");
        fs::create_dir(&dir).unwrap();
        let file = dir.join("ordinary");
        fs::write(&file, b"generated metadata fixture").unwrap();
        let table = "1 0 0:1 / / rw - ext4 /dev/local rw\n";
        assert_eq!(
            inspect_with_mounts(&file, SourceKind::OrdinaryDirectory, table).unwrap(),
            Disposition::FullFileNeedsApprovedPlan
        );
        assert_eq!(
            inspect_with_mounts(&file, SourceKind::OtherClientSyncRoot, table).unwrap(),
            Disposition::PlaceholderNeedsOriginalClientExport
        );
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert_eq!(
            inspect_with_mounts(&link.join("ordinary"), SourceKind::OrdinaryDirectory, table)
                .unwrap(),
            Disposition::SkipReparseOrOffline
        );
        assert_eq!(
            inspect_with_mounts(&dir, SourceKind::OrdinaryDirectory, table).unwrap(),
            Disposition::SkipDirectory
        );
        let sparse = dir.join("sparse");
        fs::File::create(&sparse)
            .unwrap()
            .set_len(1024 * 1024)
            .unwrap();
        assert_eq!(
            inspect_with_mounts(&sparse, SourceKind::OrdinaryDirectory, table).unwrap(),
            Disposition::SkipReparseOrOffline
        );
        let fifo = dir.join("pipe");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert_eq!(
            inspect_with_mounts(&fifo, SourceKind::OrdinaryDirectory, table).unwrap(),
            Disposition::SkipReparseOrOffline
        );
        assert_eq!(fs::read(&file).unwrap(), b"generated metadata fixture");
    }
}
