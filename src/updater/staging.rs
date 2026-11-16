//! Staging-area hygiene: a single-update lock and launch-time cleanup.
//!
//! Layout inside the staging folder: `update.lock`, plus one entry per
//! in-flight or staged Update. A directory holding a `verified` marker is a
//! verified Staged update and survives launches; everything else (partial
//! downloads, unverified extracts, a lock left by a crash) is stale.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const LOCK_FILE_NAME: &str = "update.lock";
pub const VERIFIED_MARKER: &str = "verified";

/// What the cleanup decision needs to know about one staging entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagingEntry {
    pub name: String,
    pub is_dir: bool,
    pub has_verified_marker: bool,
}

/// Names to delete at launch. Pure.
pub fn plan_cleanup(entries: &[StagingEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| !(e.is_dir && e.has_verified_marker))
        .map(|e| e.name.clone())
        .collect()
}

/// The system edge for staging cleanup.
pub trait StagingFs {
    fn list(&self, dir: &Path) -> io::Result<Vec<StagingEntry>>;
    fn remove(&self, dir: &Path, name: &str) -> io::Result<()>;
}

pub struct RealStagingFs;

impl StagingFs for RealStagingFs {
    fn list(&self, dir: &Path) -> io::Result<Vec<StagingEntry>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let is_dir = entry.file_type()?.is_dir();
            out.push(StagingEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir,
                has_verified_marker: is_dir && path.join(VERIFIED_MARKER).is_file(),
            });
        }
        Ok(out)
    }

    fn remove(&self, dir: &Path, name: &str) -> io::Result<()> {
        let path = dir.join(name);
        if path.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
    }
}

/// Remove stale staging entries; returns the names removed. A missing staging
/// folder is fine (nothing to clean). Call once at launch, before any updater
/// run could hold the lock.
pub fn cleanup_at_launch(fs: &dyn StagingFs, staging_dir: &Path) -> io::Result<Vec<String>> {
    let entries = match fs.list(staging_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let doomed = plan_cleanup(&entries);
    for name in &doomed {
        fs.remove(staging_dir, name)?;
    }
    Ok(doomed)
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("another update is already running")]
    Held,
    #[error("could not take the update lock: {0}")]
    Io(#[from] io::Error),
}

/// Held for the duration of one updater run; released on drop.
#[derive(Debug)]
pub struct UpdateLock {
    path: PathBuf,
}

impl UpdateLock {
    /// Atomically create the lock file (creating the staging folder if needed).
    pub fn acquire(staging_dir: &Path) -> Result<UpdateLock, LockError> {
        fs::create_dir_all(staging_dir)?;
        let path = staging_dir.join(LOCK_FILE_NAME);
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => Ok(UpdateLock { path }),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(LockError::Held),
            Err(e) => Err(LockError::Io(e)),
        }
    }
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool, verified: bool) -> StagingEntry {
        StagingEntry { name: name.into(), is_dir, has_verified_marker: verified }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp").join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn plan_removes_partials_and_unverified_keeps_verified() {
        let entries = [
            entry("0.9.0.zip.part", false, false),
            entry("extract-0.9.0", true, false),
            entry("staged-0.9.0", true, true),
            entry(LOCK_FILE_NAME, false, false),
        ];
        assert_eq!(
            plan_cleanup(&entries),
            vec!["0.9.0.zip.part", "extract-0.9.0", LOCK_FILE_NAME]
        );
    }

    #[test]
    fn plan_ignores_marker_on_a_plain_file() {
        assert_eq!(plan_cleanup(&[entry("x", false, true)]), vec!["x"]);
    }

    #[test]
    fn second_lock_is_refused_until_first_drops() {
        let dir = scratch("lock");
        let first = UpdateLock::acquire(&dir).unwrap();
        assert!(matches!(UpdateLock::acquire(&dir), Err(LockError::Held)));
        drop(first);
        assert!(UpdateLock::acquire(&dir).is_ok());
    }

    #[test]
    fn cleanup_on_disk_keeps_only_verified_staged_update() {
        let dir = scratch("cleanup");
        fs::write(dir.join("a.zip.part"), b"x").unwrap();
        fs::create_dir_all(dir.join("extract-a/JAIT.app")).unwrap();
        fs::create_dir_all(dir.join("staged-a/JAIT.app")).unwrap();
        fs::write(dir.join("staged-a").join(VERIFIED_MARKER), b"").unwrap();
        fs::write(dir.join(LOCK_FILE_NAME), b"").unwrap();

        let mut removed = cleanup_at_launch(&RealStagingFs, &dir).unwrap();
        removed.sort();
        assert_eq!(removed, vec!["a.zip.part", "extract-a", LOCK_FILE_NAME]);
        assert!(dir.join("staged-a/JAIT.app").is_dir());
        assert!(!dir.join("extract-a").exists());
    }

    #[test]
    fn cleanup_of_missing_staging_dir_is_a_noop() {
        let dir = scratch("missing").join("nope");
        assert!(cleanup_at_launch(&RealStagingFs, &dir).unwrap().is_empty());
    }
}
