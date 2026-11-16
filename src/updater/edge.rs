//! The updater's system edge: the I/O the state machine's effects ask for.
//!
//! The machine stays pure; the app maps each `Effect` to a Task that calls into
//! an [`UpdateEdge`] and feeds the result back as an `Event`. Follow-up tickets
//! add methods here (download: JRST-514, stage: JRST-516, swap: JRST-519).

use std::future::Future;
use std::pin::Pin;

use super::manifest::{self, ParsedManifest};

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
}

impl HttpEdge {
    /// Reads `JAIT_UPDATE_FEED_URL` once, at construction.
    pub fn from_env() -> Self {
        HttpEdge {
            url: feed_url(std::env::var(FEED_URL_ENV).ok()),
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
