//! The updater's system edge: the I/O the state machine's effects ask for.
//!
//! The machine stays pure; the app maps each `Effect` to a Task that calls into
//! an [`UpdateEdge`] and feeds the result back as an `Event`. Follow-up tickets
//! add methods here (download: JRST-514, stage: JRST-516, swap: JRST-519).

use std::future::Future;
use std::pin::Pin;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use semver::Version;

use super::download::{self, DownloadError, ZipSink};
use super::manifest::{self, ParsedManifest};
use super::staging::{LockError, UpdateLock};
use super::verify::{self, StageError, StageRequest, SystemTools};

/// Where the Release manifest (`latest.json`) lives. GitHub redirects
/// `latest/download/` to the newest Release's asset. Never the REST API.
pub const DEFAULT_FEED_URL: &str =
    "https://github.com/denvermullets/jait-releases/releases/latest/download/latest.json";

/// Env var that overrides [`DEFAULT_FEED_URL`] (draft or local manifests).
pub const FEED_URL_ENV: &str = "JAIT_UPDATE_FEED_URL";

pub type EdgeFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// The I/O the updater needs. `Err` is a human-readable reason that ends up in
/// Settings.
pub trait UpdateEdge: Send + Sync {
    /// GET the Release manifest's bytes.
    fn fetch_manifest(&self) -> EdgeFuture<Result<Vec<u8>, String>>;

    /// Stream the Update's zip into the staging dir (see `download::zip_path`),
    /// calling `on_progress(received_bytes)` as whole percents tick over.
    fn download(
        &self,
        req: DownloadRequest,
        on_progress: Box<dyn Fn(u64) + Send + Sync>,
    ) -> EdgeFuture<Result<(), DownloadError>>;

    /// Stop the running download (it ends as `DownloadError::Cancelled`).
    fn cancel_download(&self);

    /// Unzip, verify and mark the downloaded Update (see `verify::stage`),
    /// under the update lock.
    fn stage(&self, req: StageRequest) -> EdgeFuture<Result<(), StageError>>;

    /// Delete a superseded `staged-<ver>/` (an older Staged update).
    fn remove_staged(&self, staging_dir: PathBuf, version: Version) -> EdgeFuture<()>;

    /// Delete every staging file of a failed Update (zip, partial, staged dir).
    fn delete_staging(&self, staging_dir: PathBuf, version: Version) -> EdgeFuture<()>;
}

/// What one download needs.
#[derive(Debug, Clone)]
pub struct DownloadRequest {
    pub url: String,
    pub version: Version,
    pub sha256: String,
    pub size: u64,
    pub staging_dir: PathBuf,
}

/// The manifest URL: the env override if set and non-blank, else the default.
pub fn feed_url(env_override: Option<String>) -> String {
    env_override
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_FEED_URL.to_string())
}

/// Real edge: reqwest, following redirects.
pub struct HttpEdge {
    url: String,
    cancel: Arc<AtomicBool>,
}

impl HttpEdge {
    /// Reads `JAIT_UPDATE_FEED_URL` once, at construction.
    pub fn from_env() -> Self {
        HttpEdge {
            url: feed_url(std::env::var(FEED_URL_ENV).ok()),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl UpdateEdge for HttpEdge {
    fn fetch_manifest(&self) -> EdgeFuture<Result<Vec<u8>, String>> {
        let url = self.url.clone();
        Box::pin(async move {
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::limited(10))
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(|e| format!("could not start the request: {e}"))?;
            let resp = client
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("could not reach the update server: {e}"))?;
            let status = resp.status();
            if !status.is_success() {
                return Err(format!("the update server answered {status}"));
            }
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| format!("the update server's reply was cut off: {e}"))?;
            Ok(bytes.to_vec())
        })
    }

    fn download(
        &self,
        req: DownloadRequest,
        on_progress: Box<dyn Fn(u64) + Send + Sync>,
    ) -> EdgeFuture<Result<(), DownloadError>> {
        let cancel = self.cancel.clone();
        cancel.store(false, Ordering::SeqCst);
        Box::pin(async move { download_zip(&req, &cancel, on_progress.as_ref()).await })
    }

    fn cancel_download(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    fn stage(&self, req: StageRequest) -> EdgeFuture<Result<(), StageError>> {
        Box::pin(async move {
            run_blocking(move || {
                let _lock = UpdateLock::acquire(&req.staging_dir).map_err(|e| {
                    StageError::Broken(format!("could not take the update lock: {e}"))
                })?;
                verify::stage(&SystemTools, &req)
            })
            .await
            .unwrap_or_else(|e| Err(StageError::Broken(e)))
        })
    }

    fn remove_staged(&self, staging_dir: PathBuf, version: Version) -> EdgeFuture<()> {
        Box::pin(async move {
            let _ = run_blocking(move || locked_delete(&staging_dir, &version)).await;
        })
    }

    fn delete_staging(&self, staging_dir: PathBuf, version: Version) -> EdgeFuture<()> {
        self.remove_staged(staging_dir, version)
    }
}

/// Delete one version's files under the update lock; skipped if another run
/// holds it (launch cleanup removes anything unverified later).
fn locked_delete(staging_dir: &std::path::Path, version: &Version) {
    if let Ok(_lock) = UpdateLock::acquire(staging_dir) {
        verify::delete_version(staging_dir, version);
    }
}

async fn run_blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("the update step crashed: {e}"))
}

/// How long one chunk may take to arrive before the download counts as dead.
const CHUNK_TIMEOUT: Duration = Duration::from_secs(60);

/// Download under the update lock; a failed or cancelled run leaves no `.part`.
async fn download_zip(
    req: &DownloadRequest,
    cancel: &AtomicBool,
    on_progress: &(dyn Fn(u64) + Send + Sync),
) -> Result<(), DownloadError> {
    let _lock = UpdateLock::acquire(&req.staging_dir).map_err(|e| match e {
        LockError::Held => DownloadError::Failed("another update is already running".into()),
        LockError::Io(e) => {
            DownloadError::Failed(format!("could not prepare the update folder: {e}"))
        }
    })?;
    let part = download::part_path(&req.staging_dir, &req.version);
    match stream_to_part(req, &part, cancel, on_progress).await {
        Ok(()) => download::promote(&part, &download::zip_path(&req.staging_dir, &req.version)),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

async fn stream_to_part(
    req: &DownloadRequest,
    part: &std::path::Path,
    cancel: &AtomicBool,
    on_progress: &(dyn Fn(u64) + Send + Sync),
) -> Result<(), DownloadError> {
    let net = |what: &str, e: &dyn std::fmt::Display| DownloadError::Failed(format!("{what}: {e}"));
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .connect_timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| net("could not start the download", &e))?;
    let mut resp = client
        .get(&req.url)
        .send()
        .await
        .map_err(|e| net("could not reach the update server", &e))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(DownloadError::Failed(format!(
            "the update server answered {status}"
        )));
    }
    let file =
        std::fs::File::create(part).map_err(|e| net("could not write the download", &e))?;
    let mut sink = ZipSink::new(std::io::BufWriter::new(file), req.size, &req.sha256);
    let mut last_percent = 0u8;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(DownloadError::Cancelled);
        }
        let chunk = tokio::time::timeout(CHUNK_TIMEOUT, resp.chunk())
            .await
            .map_err(|_| DownloadError::Failed("the download stalled".into()))?
            .map_err(|e| net("the download was interrupted", &e))?;
        let Some(chunk) = chunk else { break };
        sink.push(&chunk)?;
        let pct = download::progress_percent(sink.received(), req.size);
        if pct != last_percent {
            last_percent = pct;
            on_progress(sink.received());
        }
    }
    sink.finish()?;
    Ok(())
}

/// Fetch and parse the Release manifest. Any failure is a check failure.
pub async fn check(edge: &dyn UpdateEdge) -> Result<ParsedManifest, String> {
    let bytes = edge.fetch_manifest().await?;
    manifest::parse(&bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(Result<Vec<u8>, String>);

    impl UpdateEdge for Fake {
        fn fetch_manifest(&self) -> EdgeFuture<Result<Vec<u8>, String>> {
            let r = self.0.clone();
            Box::pin(async move { r })
        }

        fn download(
            &self,
            _req: DownloadRequest,
            _on_progress: Box<dyn Fn(u64) + Send + Sync>,
        ) -> EdgeFuture<Result<(), DownloadError>> {
            Box::pin(async { Ok(()) })
        }

        fn cancel_download(&self) {}

        fn stage(&self, _req: StageRequest) -> EdgeFuture<Result<(), StageError>> {
            Box::pin(async { Ok(()) })
        }

        fn remove_staged(&self, _d: PathBuf, _v: Version) -> EdgeFuture<()> {
            Box::pin(async {})
        }

        fn delete_staging(&self, _d: PathBuf, _v: Version) -> EdgeFuture<()> {
            Box::pin(async {})
        }
    }

    fn block_on<T>(f: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn feed_url_defaults_and_honours_a_non_blank_override() {
        assert_eq!(feed_url(None), DEFAULT_FEED_URL);
        assert_eq!(feed_url(Some("  ".into())), DEFAULT_FEED_URL);
        assert_eq!(
            feed_url(Some(" http://localhost:9/latest.json ".into())),
            "http://localhost:9/latest.json"
        );
    }

    #[test]
    fn a_transport_failure_is_a_check_failure() {
        let r = block_on(check(&Fake(Err("offline".into()))));
        assert_eq!(r.unwrap_err(), "offline");
    }

    #[test]
    fn a_malformed_manifest_is_a_check_failure() {
        let r = block_on(check(&Fake(Ok(b"not json".to_vec()))));
        assert!(r.unwrap_err().contains("not valid JSON"));
    }
}
