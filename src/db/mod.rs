pub mod activity;
pub mod attachments;
pub mod connection;
pub mod dashboards;
pub mod home_page;
pub mod migrations;
pub mod models;
pub mod notifications;
pub mod open_tabs;
pub mod panel_states;
pub mod queries;
pub mod recurring_issues;
pub mod saved_views;
pub mod sidebar_layout;
pub mod sync_queue;
pub mod team_purge;
pub mod terminal_dirs;
pub mod terminal_records;

/// Wire/storage timestamp format used for every `*_at` column.
pub const TS_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.3fZ";

/// Current UTC time in [`TS_FORMAT`].
pub fn now_iso() -> String {
    chrono::Utc::now().format(TS_FORMAT).to_string()
}
