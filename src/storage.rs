//! Small, private, crash-safe local files. Never include file contents in errors.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

fn ensure_parent(path: &Path) -> io::Result<()> {
    // Overrides can point into an existing shared directory (even /tmp).
    // Restrict directories we create, never chmod the user's existing parent.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

pub(crate) fn read_table(path: &Path) -> io::Result<toml::Table> {
    match std::fs::read_to_string(path) {
        Ok(text) => text.parse().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid TOML; repair the file before saving (existing contents preserved)",
            )
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(e) => Err(e),
    }
}

pub(crate) fn merge_table(target: &mut toml::Table, source: toml::Table) {
    for (key, value) in source {
        if let (Some(toml::Value::Table(old)), toml::Value::Table(new)) =
            (target.get_mut(&key), &value)
        {
            merge_table(old, new.clone());
        } else {
            target.insert(key, value);
        }
    }
}

/// Serialize read-modify-write operations, including those from another instance.
pub(crate) fn update_table(
    path: &Path,
    edit: impl FnOnce(&mut toml::Table) -> io::Result<()>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("file has no parent"))?;
    ensure_parent(parent)?;
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(".lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path)?;
    // SAFETY: flock only uses the live file descriptor; closing releases the lock.
    while unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    let mut table = read_table(path)?;
    edit(&mut table)?;
    let text = toml::to_string_pretty(&table).map_err(io::Error::other)?;
    atomic_write(path, text.as_bytes())
}

/// A unique sibling file; cancellation/failure removes it, success renames it.
pub(crate) struct PendingFile {
    path: PathBuf,
}

impl PendingFile {
    pub(crate) fn new(destination: &Path) -> io::Result<Self> {
        let parent = destination
            .parent()
            .ok_or_else(|| io::Error::other("file has no parent"))?;
        ensure_parent(parent)?;
        loop {
            let seq = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(".omg-{}-{seq}.part", std::process::id()));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(_) => return Ok(Self { path }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn commit(self, destination: &Path) -> io::Result<()> {
        let file = File::open(&self.path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.sync_all()?;
        std::fs::rename(&self.path, destination)?;
        if let Some(parent) = destination.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
}

impl Drop for PendingFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let pending = PendingFile::new(path)?;
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(pending.path())?;
    file.write_all(bytes)?;
    drop(file);
    pending.commit(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writing_an_override_preserves_existing_parent_permissions() {
        let dir = std::env::temp_dir().join(format!(
            "omg-parent-{}-{}",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = dir.join("settings.toml");
        update_table(&path, |table| {
            table.insert("enabled".into(), true.into());
            Ok(())
        })
        .unwrap();
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        atomic_write(&dir.join("new/private/settings.toml"), b"enabled = true").unwrap();
        assert_eq!(
            std::fs::metadata(dir.join("new/private"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_file_is_preserved_and_parallel_updates_do_not_overwrite() {
        let dir = std::env::temp_dir().join(format!(
            "omg-storage-{}-{}",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        ensure_parent(&dir).unwrap();
        let path = dir.join("settings.toml");
        atomic_write(&path, b"broken = [").unwrap();
        assert!(update_table(&path, |_| Ok(())).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"broken = [");
        atomic_write(&path, b"count = 0\n").unwrap();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let path = &path;
                scope.spawn(move || {
                    update_table(path, |table| {
                        let count = table["count"].as_integer().unwrap();
                        table.insert("count".into(), (count + 1).into());
                        Ok(())
                    })
                    .unwrap();
                });
            }
        });
        assert_eq!(read_table(&path).unwrap()["count"].as_integer(), Some(8));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let pending = PendingFile::new(&path).unwrap();
        let incomplete = pending.path().to_owned();
        drop(pending);
        assert!(!incomplete.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
