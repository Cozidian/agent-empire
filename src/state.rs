//! Locked, atomic storage for the empire's local records.

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
