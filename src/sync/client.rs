/// Stubbed HTTP client for future Rails API sync.
/// When the Rails app exposes a JSON API, this module will handle
/// authentication and HTTP requests.

pub struct SyncClient {
    pub base_url: String,
}

impl SyncClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
        }
    }

    /// Stub: push a sync queue entry to the remote API.
    pub async fn push_change(
        &self,
        _entity_type: &str,
        _entity_id: &str,
        _action: &str,
        _payload: Option<&str>,
    ) -> Result<(), String> {
        // TODO: implement when Rails API is available
        Err("Sync not yet implemented — Rails API not available".to_string())
    }

    /// Stub: pull latest changes from the remote API.
    pub async fn pull_changes(&self) -> Result<(), String> {
        Err("Sync not yet implemented — Rails API not available".to_string())
    }
}
