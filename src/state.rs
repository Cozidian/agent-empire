//! Locked, atomic storage for the empire's local records.

/// A private directory for mission text, screenshots and session records.
pub fn private_dir(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "state directory must be owned by this user and must not be a symlink"));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

pub fn private_write(path: &std::path::Path, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes.as_ref())
}

#[cfg(test)]
mod privacy_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn private_records_tighten_existing_modes_and_refuse_symlinks() {
        let dir = std::env::temp_dir().join(format!("empire-private-records-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&dir).unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        let file = dir.join("record");
        std::fs::write(&file, "old").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        private_write(&file, "private").unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        let link = dir.join("linked-record"); symlink(&file, &link).unwrap();
        assert!(private_write(&link, "overwrite").is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "private");
        let linked_dir = dir.join("linked-directory"); symlink(&dir, &linked_dir).unwrap();
        assert!(private_dir(&linked_dir).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

/// Serialize readers and writers, across galaxy windows and reporting processes.
/// A corrupt book is left untouched; writes replace it only after a complete temporary file.
pub fn edit<T>(path: &std::path::Path, change: impl FnOnce(&mut T)) -> std::io::Result<T>
where T: serde::de::DeserializeOwned + serde::Serialize + Default + Clone + PartialEq {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
    let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o600).open(path.with_extension("lock"))?;
    // dropping this file releases the advisory lock, including on errors
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 { return Err(std::io::Error::last_os_error()); }
    let mut book = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<T>(&bytes).map_err(std::io::Error::other)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => T::default(),
        Err(e) => return Err(e),
    };
    let before = book.clone();
    change(&mut book);
    if before != book {
        let tmp = path.with_extension("tmp");
        let mut file = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(&book).map_err(std::io::Error::other)?)?;
        file.sync_all()?;
        fs::rename(tmp, path)?;
    }
    Ok(book)
}
