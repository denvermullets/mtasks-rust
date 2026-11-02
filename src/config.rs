use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::display::{DisplayOptions, Filters};

/// Default API server. Only dev builds know about localhost; release builds talk
/// to prod. `scripts/build-release.sh` checks the shipped binary for this.
#[cfg(debug_assertions)]
const DEFAULT_API_BASE_URL: &str = "http://localhost:3005";
#[cfg(not(debug_assertions))]
const DEFAULT_API_BASE_URL: &str = "https://justanotherissuetracker.com";

/// Folder under the config and data dirs. Dev and release builds keep separate
/// config, database and files: local rows are matched to the server by
/// `remote_id`, so one database must never sync against two servers.
pub const APP_DIR: &str = if cfg!(debug_assertions) { "mtasks-rusty" } else { "JAIT" };

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_api_base_url")]
    pub api_base_url: String,
    #[serde(default)]
    pub api_token: Option<String>,
    #[serde(default)]
    pub last_synced_at: Option<DateTime<Utc>>,
    /// Rails user id of the signed-in user, captured from `/me` on sync. Used to
    /// stamp `creator_id` on locally-created issues (resolved to the local member
    /// row via its `remote_id`).
    #[serde(default)]
    pub current_user_remote_id: Option<i64>,
    /// Display name of the signed-in user, captured alongside the id.
    #[serde(default)]
    pub current_user_name: Option<String>,
    /// Board display options (group/sub-group/order/completed/card properties).
    #[serde(default)]
    pub display: DisplayOptions,
    /// Board issue filters. Global, not per team (v1).
    #[serde(default)]
    pub filters: Filters,
    /// Key of the selected color theme (see `theme::THEMES`).
    #[serde(default = "default_theme")]
    pub theme: String,
    /// UI font key (see `crate::fonts`). Local-only, never synced; read at
    /// launch, so changes apply after a restart.
    #[serde(default = "default_font")]
    pub font: String,
    /// Post native macOS banners for new inbox notifications when the window
    /// is unfocused. Local-only, never synced.
    #[serde(default = "default_true")]
    pub native_notifications: bool,
}

fn default_true() -> bool {
    true
}

fn default_theme() -> String {
    crate::theme::DEFAULT_THEME_KEY.to_string()
}

fn default_font() -> String {
    crate::fonts::DEFAULT_FONT_KEY.to_string()
}

fn default_api_base_url() -> String {
    DEFAULT_API_BASE_URL.to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_base_url: default_api_base_url(),
            api_token: None,
            last_synced_at: None,
            current_user_remote_id: None,
            current_user_name: None,
            display: DisplayOptions::default(),
            filters: Filters::default(),
            theme: default_theme(),
            font: default_font(),
            native_notifications: true,
        }
    }
}

pub fn config_path() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_DIR);
    let _ = std::fs::create_dir_all(&dir);
    dir.join("config.toml")
}

/// Load config from disk. Returns the loaded (or default) config plus an
/// optional human-readable error to surface in the UI.
///
/// - missing file: write defaults to disk, no error
/// - malformed TOML: keep file intact, return defaults + error string
/// - I/O error: return defaults + error string
pub fn load() -> (Config, Option<String>) {
    let path = config_path();

    if !path.exists() {
        let cfg = Config::default();
        let err = save(&cfg).err();
        return (cfg, err);
    }

    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            return (
                Config::default(),
                Some(format!("read {}: {}", path.display(), e)),
            )
        }
    };

    match toml::from_str::<Config>(&raw) {
        Ok(cfg) => (cfg, None),
        Err(e) => (Config::default(), Some(format!("parse config.toml: {}", e))),
    }
}

pub fn save(cfg: &Config) -> Result<(), String> {
    let path = config_path();
    let body = toml::to_string_pretty(cfg).map_err(|e| format!("serialize config: {}", e))?;
    write_private(&path, &body).map_err(|e| format!("write {}: {}", path.display(), e))
}

/// Write owner-only (0600): the config holds the API token in plain text. Also
/// tightens a file that was created with looser permissions.
fn write_private(path: &Path, body: &str) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(body.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::{CardProperty, FilterField, GroupBy, OrderBy, SubGroup};

    #[test]
    fn write_private_is_owner_only_even_over_a_loose_file() {
        let path = std::env::temp_dir().join(format!("config-{}.toml", uuid::Uuid::new_v4()));
        std::fs::write(&path, "old body that is longer").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_private(&path, "new").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn display_and_filters_round_trip() {
        let mut cfg = Config::default();
        cfg.display.group_by = GroupBy::Assignee;
        cfg.display.sub_group = SubGroup::Priority;
        cfg.display.order_by = OrderBy::DueDate;
        cfg.display.show_completed = false;
        cfg.display.toggle_property(CardProperty::Assignee);
        cfg.filters.toggle(FilterField::Priority, "1");
        cfg.filters.toggle(FilterField::Assignee, "");
        let body = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&body).unwrap();
        assert_eq!(back.display, cfg.display);
        assert_eq!(back.filters, cfg.filters);
    }

    #[test]
    fn old_config_with_sync_interval_secs_loads_and_drops_it_on_save() {
        let cfg: Config =
            toml::from_str("api_base_url = \"http://x\"\nsync_interval_secs = 30\n").unwrap();
        assert_eq!(cfg.api_base_url, "http://x");
        assert!(!toml::to_string_pretty(&cfg)
            .unwrap()
            .contains("sync_interval"));
    }

    #[test]
    fn native_notifications_defaults_on_and_round_trips() {
        let old: Config = toml::from_str("api_base_url = \"http://x\"\n").unwrap();
        assert!(old.native_notifications);
        let mut cfg = Config::default();
        cfg.native_notifications = false;
        let back: Config = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert!(!back.native_notifications);
    }

    #[test]
    fn font_round_trips_and_old_config_defaults_to_inter() {
        let mut cfg = Config::default();
        assert_eq!(cfg.font, "inter");
        cfg.font = "jetbrains-mono".into();
        let back: Config = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert_eq!(back.font, "jetbrains-mono");
        let old: Config = toml::from_str("api_base_url = \"http://x\"\n").unwrap();
        assert_eq!(old.font, "inter");
    }

    #[test]
    fn old_config_without_display_uses_defaults() {
        let cfg: Config = toml::from_str("api_base_url = \"http://x\"\n").unwrap();
        assert_eq!(cfg.display, DisplayOptions::default());
        assert_eq!(cfg.filters.count(), 0);
    }

    #[test]
    fn old_display_table_without_view_mode_defaults_to_board_and_list_persists() {
        let cfg: Config = toml::from_str("[display]\ngroup_by = \"priority\"\n").unwrap();
        assert_eq!(cfg.display.view_mode, crate::display::ViewMode::Board);
        let mut cfg = Config::default();
        cfg.display.view_mode = crate::display::ViewMode::List;
        let back: Config = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.display.view_mode, crate::display::ViewMode::List);
    }

    #[test]
    fn old_config_without_theme_defaults_to_default_key() {
        let cfg: Config = toml::from_str("api_base_url = \"http://x\"\n").unwrap();
        assert_eq!(cfg.theme, "default");
    }

    #[test]
    fn theme_key_round_trips() {
        let mut cfg = Config::default();
        cfg.theme = "warm-paper".to_string();
        let back: Config = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert_eq!(back.theme, "warm-paper");
    }
}
