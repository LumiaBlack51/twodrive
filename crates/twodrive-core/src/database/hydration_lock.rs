use super::Database;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::PathBuf;

impl Database {
    // All clients of this database use the same inode, across retries and generations.
    // Keep the empty lock files: unlinking one can split an owner from its waiters.
    fn hydration_lock_path(&self, id: &str) -> io::Result<PathBuf> {
        Ok(self
            .path()
            .canonicalize()?
            .with_added_extension("hydration-locks")
            .join(format!("{:x}", Sha256::digest(id.as_bytes()))))
    }

    pub fn lock_hydration(&self, id: &str) -> io::Result<File> {
        let path = self.hydration_lock_path(id)?;
        fs::create_dir_all(path.parent().expect("lock directory"))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.lock()?;
        Ok(file)
    }

    // Called with a SQLite write transaction open. Never wait for an owner
    // here: that owner needs the database to publish or abandon its download.
    pub(super) fn hydration_in_progress(&self, id: &str) -> io::Result<bool> {
        let file = match File::open(self.hydration_lock_path(id)?) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(err),
        };
        match file.try_lock_shared() {
            Ok(()) => Ok(false),
            Err(TryLockError::WouldBlock) => Ok(true),
            Err(TryLockError::Error(err)) => Err(err),
        }
    }
}
