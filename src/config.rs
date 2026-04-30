use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
        Err(e) => return (Config::default(), Some(format!("read {}: {}", path.display(), e))),
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
