//! Cooperative local-file commits shared by plan and controller persistence.
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    path: PathBuf,
    digest: [u8; 32],
}

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("state changed since it was read; reload before saving")]
    Conflict,
    #[error("state is busy; another writer holds the lock")]
    Busy,
    #[error("state persistence requires regular files, not links or directories")]
    UnsafePath,
    #[error("storage revision exhausted")]
    RevisionExhausted,
    #[error(transparent)]
    Io(#[from] io::Error),
}

fn regular_or_absent(path: &Path) -> Result<bool, PersistenceError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(PersistenceError::UnsafePath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn identity(path: &Path) -> Result<PathBuf, PersistenceError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path.file_name().ok_or(PersistenceError::UnsafePath)?;
    Ok(parent.canonicalize()?.join(name))
}

fn snapshot(path: PathBuf, bytes: &[u8]) -> Snapshot {
    Snapshot {
        path,
        digest: Sha256::digest(bytes).into(),
    }
}

/// Atomic rename permits lock-free readers; uncommitted sidecars are never read.
pub fn read(path: &Path) -> Result<Option<(Vec<u8>, Snapshot)>, PersistenceError> {
    if !regular_or_absent(path)? {
        return Ok(None);
    }
    let path = identity(path)?;
    let bytes = fs::read(&path)?;
    let token = snapshot(path, &bytes);
    Ok(Some((bytes, token)))
}

/// Sidecars append suffixes so distinct target extensions cannot share a lock.
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn open_sidecar(path: &Path) -> Result<File, PersistenceError> {
    regular_or_absent(path)?;
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?)
}

fn acquire(path: &Path) -> Result<File, PersistenceError> {
    let file = open_sidecar(&sidecar(path, ".lock"))?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(error) if error.kind() == fs2::lock_contended_error().kind() => {
                if Instant::now() >= deadline {
                    return Err(PersistenceError::Busy);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// Commit only the snapshot loaded from this exact target. A new object cannot
/// overwrite an existing record. Dropping the lock handle also handles unwind.
pub fn commit(
    path: &Path,
    expected: Option<&Snapshot>,
    bytes: &[u8],
) -> Result<Snapshot, PersistenceError> {
    commit_with_hook(path, expected, bytes, || Ok(()))
}

fn commit_with_hook(
    path: &Path,
    expected: Option<&Snapshot>,
    bytes: &[u8],
    before_rename: impl FnOnce() -> io::Result<()>,
) -> Result<Snapshot, PersistenceError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let path = identity(path)?;
    let _lock = acquire(&path)?;
    let current = read(&path)?.map(|(_, token)| token);
    if current.as_ref() != expected {
        return Err(PersistenceError::Conflict);
    }

    let pending = sidecar(&path, ".pending");
    let mut file = open_sidecar(&pending)?;
    // A pending file from a dead writer has no commit authority. Only the lock
    // owner may reuse it after CAS succeeds; malformed main files are not repaired.
    file.set_len(0)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    before_rename()?;
    fs::rename(&pending, &path)?;
    #[cfg(unix)]
    File::open(path.parent().expect("canonical target has parent"))?.sync_all()?;
    Ok(snapshot(path, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_precommit_preserves_old_bytes_and_next_commit_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let token = commit(&path, None, b"old").unwrap();
        let error = commit_with_hook(&path, Some(&token), b"uncommitted", || {
            Err(io::Error::other("injected pre-rename failure"))
        })
        .unwrap_err();
        assert!(matches!(error, PersistenceError::Io(_)));
        assert_eq!(read(&path).unwrap().unwrap().0, b"old");
        commit(&path, Some(&token), b"new").unwrap();
        assert_eq!(read(&path).unwrap().unwrap().0, b"new");
        assert!(!sidecar(&path, ".pending").exists());
    }

    #[test]
    fn contention_is_bounded_and_lock_inode_survives() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let lock = acquire(&path).unwrap();
        assert!(matches!(
            commit(&path, None, b"new"),
            Err(PersistenceError::Busy)
        ));
        drop(lock);
        commit(&path, None, b"new").unwrap();
        assert!(sidecar(&path, ".lock").is_file());
    }

    #[test]
    fn dead_writer_child() {
        let Ok(root) = std::env::var("ESI_STORAGE_CRASH_FIXTURE") else {
            return;
        };
        let path = Path::new(&root).join("state.json");
        let (_, token) = read(&path).unwrap().unwrap();
        let _ = commit_with_hook(&path, Some(&token), b"never committed", || {
            // Exit without destructors while holding the actual OS lock.
            std::process::exit(73);
        });
        panic!("crash fixture unexpectedly returned");
    }

    #[test]
    fn process_death_releases_lock_without_promoting_pending_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let token = commit(&path, None, b"committed").unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "storage::tests::dead_writer_child"])
            .env("ESI_STORAGE_CRASH_FIXTURE", dir.path())
            .status()
            .unwrap();
        assert_eq!(child.code(), Some(73));
        assert_eq!(read(&path).unwrap().unwrap().0, b"committed");
        assert!(sidecar(&path, ".pending").is_file());
        commit(&path, Some(&token), b"recovered").unwrap();
        assert_eq!(read(&path).unwrap().unwrap().0, b"recovered");
    }

    #[cfg(unix)]
    #[test]
    fn state_lock_and_pending_symlinks_are_rejected() {
        for suffix in ["", ".lock", ".pending"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("state.json");
            let outside = dir.path().join("untouched");
            fs::write(&outside, b"keep").unwrap();
            std::os::unix::fs::symlink(&outside, sidecar(&path, suffix)).unwrap();
            assert!(matches!(
                commit(&path, None, b"bad"),
                Err(PersistenceError::UnsafePath)
            ));
            assert_eq!(fs::read(outside).unwrap(), b"keep");
        }
    }
}
