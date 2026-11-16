//! Downloading an Update's zip (JRST-514): the pure pieces plus the file sink.
//!
//! Zip path convention (JRST-516 picks the zip up from here):
//! `<staging_dir>/<version>.zip`. While downloading the bytes go to
//! `<staging_dir>/<version>.zip.part`, which is renamed only after the size and
//! sha256 check pass, so a `.zip` in the staging dir is always a verified
//! download. [`zip_path`] / [`part_path`] are the one place that says so.

use std::io::Write;
use std::path::{Path, PathBuf};

use semver::Version;
use sha2::{Digest, Sha256};

/// `<staging_dir>/<version>.zip`: a complete, sha256-checked download.
pub fn zip_path(staging_dir: &Path, version: &Version) -> PathBuf {
    staging_dir.join(format!("{version}.zip"))
}

/// `<staging_dir>/<version>.zip.part`: a download in flight.
pub fn part_path(staging_dir: &Path, version: &Version) -> PathBuf {
    staging_dir.join(format!("{version}.zip.part"))
}

/// Whole percent downloaded, capped at 100. A zero total reads as 0.
pub fn progress_percent(received: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    (received.min(total) as u128 * 100 / total as u128) as u8
}

/// Lowercase hex of a digest.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Does the computed digest match the manifest's (case-insensitive)?
pub fn sha_matches(actual_hex: &str, expected_hex: &str) -> bool {
    actual_hex.eq_ignore_ascii_case(expected_hex.trim())
}

/// Why a download stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum DownloadError {
    /// Network, HTTP or disk trouble: retry later, no strike.
    Failed(String),
    /// Bytes arrived but are wrong (too big, truncated, bad sha256).
    Broken(String),
    /// The app asked it to stop.
    Cancelled,
}

/// Receives the body chunk by chunk: writes it, hashes it, and refuses to take
/// more than the manifest's `zip_size`.
pub struct ZipSink<W: Write> {
    out: W,
    hasher: Sha256,
    received: u64,
    expected_size: u64,
    expected_sha: String,
}

impl<W: Write> ZipSink<W> {
    pub fn new(out: W, expected_size: u64, expected_sha: &str) -> Self {
        ZipSink {
            out,
            hasher: Sha256::new(),
            received: 0,
            expected_size,
            expected_sha: expected_sha.to_string(),
        }
    }

    pub fn received(&self) -> u64 {
        self.received
    }

    /// Take one chunk. Errors with `Broken` as soon as the total passes the
    /// expected size, so a bogus endless body is cut off early.
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), DownloadError> {
        self.received += chunk.len() as u64;
        if self.received > self.expected_size {
            return Err(DownloadError::Broken(format!(
                "the download is larger than the expected {} bytes",
                self.expected_size
            )));
        }
        self.hasher.update(chunk);
        self.out
            .write_all(chunk)
            .map_err(|e| DownloadError::Failed(format!("could not write the download: {e}")))
    }

    /// Flush and check the total size and sha256. Returns the writer.
    pub fn finish(mut self) -> Result<W, DownloadError> {
        self.out
            .flush()
            .map_err(|e| DownloadError::Failed(format!("could not write the download: {e}")))?;
        if self.received != self.expected_size {
            return Err(DownloadError::Broken(format!(
                "the download is truncated ({} of {} bytes)",
                self.received, self.expected_size
            )));
        }
        let actual = hex(&self.hasher.finalize());
        if !sha_matches(&actual, &self.expected_sha) {
            return Err(DownloadError::Broken(
                "the download's sha256 does not match the Release manifest".into(),
            ));
        }
        Ok(self.out)
    }
}

/// Move a finished `.part` into place; the `.part` is removed on failure.
pub fn promote(part: &Path, zip: &Path) -> Result<(), DownloadError> {
    std::fs::rename(part, zip).map_err(|e| {
        let _ = std::fs::remove_file(part);
        DownloadError::Failed(format!("could not save the download: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // sha256("hello")
    const HELLO_SHA: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    #[test]
    fn percent_is_floored_capped_and_zero_safe() {
        assert_eq!(progress_percent(0, 200), 0);
        assert_eq!(progress_percent(99, 200), 49);
        assert_eq!(progress_percent(200, 200), 100);
        assert_eq!(progress_percent(900, 200), 100);
        assert_eq!(progress_percent(5, 0), 0);
        assert_eq!(progress_percent(u64::MAX, u64::MAX), 100);
    }

    #[test]
    fn sha_check_ignores_case_and_padding() {
        assert!(sha_matches("abc", " ABC\n"));
        assert!(!sha_matches("abc", "abd"));
    }

    #[test]
    fn paths_follow_the_convention() {
        let v = Version::new(0, 13, 0);
        let dir = Path::new("/a/.jait-update");
        assert_eq!(zip_path(dir, &v), Path::new("/a/.jait-update/0.13.0.zip"));
        assert_eq!(part_path(dir, &v), Path::new("/a/.jait-update/0.13.0.zip.part"));
    }

    #[test]
    fn a_good_download_passes() {
        let mut s = ZipSink::new(Vec::new(), 5, HELLO_SHA);
        s.push(b"hel").unwrap();
        s.push(b"lo").unwrap();
        assert_eq!(s.received(), 5);
        assert_eq!(s.finish().unwrap(), b"hello");
    }

    #[test]
    fn a_wrong_sha_is_broken() {
        let mut s = ZipSink::new(Vec::new(), 5, HELLO_SHA);
        s.push(b"HELLO").unwrap();
        assert!(matches!(s.finish(), Err(DownloadError::Broken(m)) if m.contains("sha256")));
    }

    #[test]
    fn a_short_body_is_broken_as_truncated() {
        let mut s = ZipSink::new(Vec::new(), 5, HELLO_SHA);
        s.push(b"hel").unwrap();
        assert!(matches!(s.finish(), Err(DownloadError::Broken(m)) if m.contains("truncated")));
    }

    #[test]
    fn a_body_past_the_size_aborts_early() {
        let mut s = ZipSink::new(Vec::new(), 5, HELLO_SHA);
        s.push(b"hello").unwrap();
        assert!(matches!(s.push(b"!"), Err(DownloadError::Broken(m)) if m.contains("larger")));
    }

    #[test]
    fn promote_renames_and_reports_a_missing_part() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/download-promote");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let v = Version::new(1, 0, 0);
        std::fs::write(part_path(&dir, &v), b"x").unwrap();
        promote(&part_path(&dir, &v), &zip_path(&dir, &v)).unwrap();
        assert!(zip_path(&dir, &v).is_file());
        assert!(!part_path(&dir, &v).exists());
        assert!(matches!(
            promote(&part_path(&dir, &v), &zip_path(&dir, &v)),
            Err(DownloadError::Failed(_))
        ));
    }
}
