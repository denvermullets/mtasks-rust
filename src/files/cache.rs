//! File cache with LRU eviction, indexed by the `file_cache` table.

use std::fs;
use std::path::PathBuf;

use rusqlite::{params, Connection, OptionalExtension};

use crate::db::now_iso;

/// Cache size cap (~500 MB). Eviction runs until the total is at or under it.
pub const CACHE_CAP_BYTES: u64 = 500 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("db: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("invalid blob key: {0:?}")]
    InvalidKey(String),
}

pub type Result<T> = std::result::Result<T, CacheError>;

/// A cache rooted at an injectable folder (tests pass a temp dir).
pub struct FileCache {
    root: PathBuf,
    cap: u64,
}

impl FileCache {
    /// Cache under `root` with the default cap.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), cap: CACHE_CAP_BYTES }
    }

    /// Cache under the real `data_local_dir()` files root.
    pub fn at_default_root() -> Self {
        Self::new(super::default_root())
    }

    #[cfg(test)]
    pub fn with_cap(mut self, cap: u64) -> Self {
        self.cap = cap;
        self
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    #[cfg(test)]
    pub fn outbox_dir(&self) -> PathBuf {
        super::outbox_dir(&self.root)
    }

    /// Store bytes under `blob_key`, replacing any existing entry, then evict
    /// down to the cap. Returns the cached path.
    pub fn put(&self, conn: &Connection, blob_key: &str, bytes: &[u8]) -> Result<PathBuf> {
        self.put_at(conn, blob_key, bytes, &now_iso())
    }

    /// Cached path for `blob_key`, bumping its last-accessed time. A miss, or
    /// an index row whose file is gone (which is cleaned up), returns `None`.
    pub fn get(&self, conn: &Connection, blob_key: &str) -> Result<Option<PathBuf>> {
        self.get_at(conn, blob_key, &now_iso())
    }

    /// Remove least-recently-accessed entries until the cache is at or under
    /// the cap. Only files inside `cache/` are ever deleted. Returns the
    /// number of entries removed.
    pub fn evict(&self, conn: &Connection) -> Result<usize> {
        let mut total: u64 = conn.query_row(
            "SELECT COALESCE(SUM(byte_size), 0) FROM file_cache",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64;
        if total <= self.cap {
            return Ok(0);
        }
        let rows: Vec<(String, String, i64)> = {
            let mut stmt = conn.prepare(
                "SELECT blob_key, path, byte_size FROM file_cache
                 ORDER BY last_accessed ASC, blob_key ASC",
            )?;
            let mapped = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            mapped.collect::<std::result::Result<_, _>>()?
        };
        let cache_dir = self.cache_dir();
        let mut removed = 0;
        for (key, path, size) in rows {
            if total <= self.cap {
                break;
            }
            let path = PathBuf::from(path);
            if path.starts_with(&cache_dir) {
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
            conn.execute("DELETE FROM file_cache WHERE blob_key = ?1", params![key])?;
            total = total.saturating_sub(size.max(0) as u64);
            removed += 1;
        }
        Ok(removed)
    }

    /// Delete every cached file and the index (the outbox is never touched).
    /// Used when the token loses access: cached bytes must not stay reachable.
    /// Returns the number of entries removed.
    pub fn clear(&self, conn: &Connection) -> Result<usize> {
        let rows: Vec<String> = {
            let mut stmt = conn.prepare("SELECT path FROM file_cache")?;
            let mapped = stmt.query_map([], |r| r.get(0))?;
            mapped.collect::<std::result::Result<_, _>>()?
        };
        let cache_dir = self.cache_dir();
        for path in &rows {
            let path = PathBuf::from(path);
            if path.starts_with(&cache_dir) {
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        conn.execute("DELETE FROM file_cache", [])?;
        Ok(rows.len())
    }

    pub(crate) fn put_at(
        &self,
        conn: &Connection,
        blob_key: &str,
        bytes: &[u8],
        now: &str,
    ) -> Result<PathBuf> {
        let name = file_name(blob_key)?;
        let dir = self.cache_dir();
        fs::create_dir_all(&dir)?;
        let path = dir.join(name);
        fs::write(&path, bytes)?;
        conn.execute(
            "INSERT INTO file_cache (blob_key, path, byte_size, last_accessed)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(blob_key) DO UPDATE SET
                path = excluded.path,
                byte_size = excluded.byte_size,
                last_accessed = excluded.last_accessed",
            params![blob_key, path.to_string_lossy(), bytes.len() as i64, now],
        )?;
        self.evict(conn)?;
        Ok(path)
    }

    pub(crate) fn get_at(
        &self,
        conn: &Connection,
        blob_key: &str,
        now: &str,
    ) -> Result<Option<PathBuf>> {
        file_name(blob_key)?;
        let path: Option<String> = conn
            .query_row(
                "SELECT path FROM file_cache WHERE blob_key = ?1",
                params![blob_key],
                |r| r.get(0),
            )
            .optional()?;
        let Some(path) = path.map(PathBuf::from) else {
            return Ok(None);
        };
        if !path.is_file() {
            conn.execute("DELETE FROM file_cache WHERE blob_key = ?1", params![blob_key])?;
            return Ok(None);
        }
        conn.execute(
            "UPDATE file_cache SET last_accessed = ?2 WHERE blob_key = ?1",
            params![blob_key, now],
        )?;
        Ok(Some(path))
    }
}

/// Blob keys become file names, so reject anything that could escape `cache/`.
fn file_name(blob_key: &str) -> Result<&str> {
    let ok = !blob_key.is_empty()
        && blob_key.len() <= 255
        && blob_key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(blob_key)
    } else {
        Err(CacheError::InvalidKey(blob_key.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;

    fn setup(cap: u64) -> (tempdir::Tmp, Connection, FileCache) {
        let tmp = tempdir::Tmp::new();
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        let cache = FileCache::new(tmp.path()).with_cap(cap);
        (tmp, conn, cache)
    }

    fn row_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM file_cache", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn clear_removes_cached_files_but_not_the_outbox() {
        let (_t, conn, cache) = setup(1000);
        let p = cache.put(&conn, "abc", b"hello").unwrap();
        fs::create_dir_all(cache.outbox_dir()).unwrap();
        let queued = cache.outbox_dir().join("pending.bin");
        fs::write(&queued, b"x").unwrap();
        assert_eq!(cache.clear(&conn).unwrap(), 1);
        assert!(!p.exists());
        assert_eq!(row_count(&conn), 0);
        assert_eq!(cache.get(&conn, "abc").unwrap(), None);
        assert!(queued.exists());
    }

    #[test]
    fn put_then_get_returns_the_stored_bytes() {
        let (_t, conn, cache) = setup(1000);
        let p = cache.put(&conn, "abc", b"hello").unwrap();
        assert_eq!(cache.get(&conn, "abc").unwrap(), Some(p.clone()));
        assert_eq!(fs::read(p).unwrap(), b"hello");
    }

    #[test]
    fn miss_returns_none() {
        let (_t, conn, cache) = setup(1000);
        assert_eq!(cache.get(&conn, "nope").unwrap(), None);
    }

    #[test]
    fn get_updates_last_accessed() {
        let (_t, conn, cache) = setup(1000);
        cache.put_at(&conn, "a", b"x", "2026-01-01T00:00:00.000Z").unwrap();
        cache.get_at(&conn, "a", "2026-02-02T00:00:00.000Z").unwrap();
        let ts: String = conn
            .query_row("SELECT last_accessed FROM file_cache WHERE blob_key='a'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ts, "2026-02-02T00:00:00.000Z");
    }

    #[test]
    fn eviction_removes_least_recently_accessed_first_until_under_cap() {
        let (_t, conn, cache) = setup(25);
        cache.put_at(&conn, "a", &[0; 10], "2026-01-01T00:00:00.000Z").unwrap();
        cache.put_at(&conn, "b", &[0; 10], "2026-01-02T00:00:00.000Z").unwrap();
        // Touch a, so b becomes the least recently accessed.
        cache.get_at(&conn, "a", "2026-01-03T00:00:00.000Z").unwrap();
        let c = cache.put_at(&conn, "c", &[0; 10], "2026-01-04T00:00:00.000Z").unwrap();
        assert!(cache.get(&conn, "b").unwrap().is_none());
        assert!(!cache.cache_dir().join("b").exists());
        assert!(cache.get(&conn, "a").unwrap().is_some());
        assert!(c.exists());
        assert_eq!(row_count(&conn), 2);
    }

    #[test]
    fn eviction_never_touches_the_outbox_even_when_it_alone_exceeds_the_cap() {
        let (_t, conn, cache) = setup(10);
        fs::create_dir_all(cache.outbox_dir()).unwrap();
        let queued = cache.outbox_dir().join("pending.bin");
        fs::write(&queued, [0u8; 100]).unwrap();
        cache.put_at(&conn, "a", &[0; 8], "2026-01-01T00:00:00.000Z").unwrap();
        cache.put_at(&conn, "b", &[0; 8], "2026-01-02T00:00:00.000Z").unwrap();
        assert!(queued.exists());
        assert!(cache.get(&conn, "a").unwrap().is_none());
        assert!(cache.get(&conn, "b").unwrap().is_some());
    }

    #[test]
    fn evict_refuses_to_delete_an_indexed_path_outside_the_cache_dir() {
        let (_t, conn, cache) = setup(0);
        fs::create_dir_all(cache.outbox_dir()).unwrap();
        let queued = cache.outbox_dir().join("pending.bin");
        fs::write(&queued, b"data").unwrap();
        conn.execute(
            "INSERT INTO file_cache VALUES ('rogue', ?1, 4, '2026-01-01T00:00:00.000Z')",
            params![queued.to_string_lossy()],
        )
        .unwrap();
        cache.evict(&conn).unwrap();
        assert!(queued.exists());
        assert_eq!(row_count(&conn), 0);
    }

    #[test]
    fn index_row_with_missing_file_is_a_miss_and_is_cleaned_up() {
        let (_t, conn, cache) = setup(1000);
        let p = cache.put(&conn, "a", b"x").unwrap();
        fs::remove_file(p).unwrap();
        assert_eq!(cache.get(&conn, "a").unwrap(), None);
        assert_eq!(row_count(&conn), 0);
    }

    #[test]
    fn put_replaces_an_existing_entry() {
        let (_t, conn, cache) = setup(1000);
        cache.put(&conn, "a", b"old").unwrap();
        let p = cache.put(&conn, "a", b"newer").unwrap();
        assert_eq!(fs::read(p).unwrap(), b"newer");
        assert_eq!(row_count(&conn), 1);
    }

    #[test]
    fn keys_that_could_escape_the_cache_dir_are_rejected() {
        let (_t, conn, cache) = setup(1000);
        assert!(matches!(cache.put(&conn, "../x", b"x"), Err(CacheError::InvalidKey(_))));
        assert!(matches!(cache.get(&conn, ""), Err(CacheError::InvalidKey(_))));
    }

    #[test]
    fn default_cap_is_about_500_mb() {
        assert_eq!(CACHE_CAP_BYTES, 524_288_000);
    }

    /// Minimal self-deleting temp dir (no extra dependency).
    mod tempdir {
        use std::path::{Path, PathBuf};

        pub struct Tmp(PathBuf);

        impl Tmp {
            pub fn new() -> Self {
                let p = std::env::temp_dir()
                    .join(format!("mtasks-file-cache-{}", uuid::Uuid::new_v4()));
                std::fs::create_dir_all(&p).unwrap();
                Tmp(p)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for Tmp {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
