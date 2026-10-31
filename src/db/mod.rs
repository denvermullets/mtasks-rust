pub mod activity;
pub mod connection;
pub mod home_page;
pub mod migrations;
pub mod models;
pub mod notifications;
pub mod queries;
pub mod saved_views;
pub mod sync_queue;

/// Wire/storage timestamp format used for every `*_at` column.
pub const TS_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.3fZ";

/// Current UTC time in [`TS_FORMAT`].
pub fn now_iso() -> String {
    chrono::Utc::now().format(TS_FORMAT).to_string()
}
