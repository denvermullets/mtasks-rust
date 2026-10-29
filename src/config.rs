use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::display::{DisplayOptions, Filters};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_api_base_url")]
    pub api_base_url: String,
    #[serde(default)]
    pub api_token: Option<String>,
    #[serde(default)]
    pub last_synced_at: Option<DateTime<Utc>>,
    #[serde(default = "default_sync_interval_secs")]
    pub sync_interval_secs: u64,
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
}

fn default_api_base_url() -> String {
    "http://localhost:3005".to_string()
}

fn default_sync_interval_secs() -> u64 {
    60
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_base_url: default_api_base_url(),
            api_token: None,
            last_synced_at: None,
            sync_interval_secs: default_sync_interval_secs(),
            current_user_remote_id: None,
            current_user_name: None,
            display: DisplayOptions::default(),
            filters: Filters::default(),
        }
    }
}

pub fn config_path() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("mtasks-rusty");
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
    std::fs::write(&path, body).map_err(|e| format!("write {}: {}", path.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::{CardProperty, FilterField, GroupBy, OrderBy, SubGroup};

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
}
