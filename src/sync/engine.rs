/// Sync engine: reads the sync queue and pushes changes to the remote API.
/// Currently stubbed — will become functional once the Rails JSON API exists.

use crate::db::models::SyncQueueEntry;

pub struct SyncEngine;

impl SyncEngine {
    pub fn new() -> Self {
        Self
    }

    /// Process all pending sync queue entries.
    /// Returns the number of entries successfully synced.
    pub async fn process_queue(&self, _entries: Vec<SyncQueueEntry>) -> Result<usize, String> {
        // TODO: iterate entries, call SyncClient for each, mark synced
        Ok(0)
    }
}
