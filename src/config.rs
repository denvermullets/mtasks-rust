use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::db::models::{Label, Lane, Member, Project};
use crate::display::{DisplayOptions, Filters};
use crate::updater::manifest::ReleaseNotes;
use crate::updater::machine::{Event, FailureKind, LastFailure, UpdaterState};
use semver::Version;

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

/// Whose display options (and filters) a board reads and writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardScope {
    /// Cross-team pages without a set of their own: the global set.
    Global,
    /// The My Issues page's own set.
    MyIssues,
    /// One team's board, by local team id.
    Team(String),
}

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
    /// The global set: what a team without its own entry in `team_display`
    /// starts from, and what cross-team pages use.
    #[serde(default)]
    pub display: DisplayOptions,
    /// Per-team display options, by local team id. A team gets an entry the
    /// first time its options change, copied from the global set.
    #[serde(default)]
    pub team_display: BTreeMap<String, DisplayOptions>,
    /// My Issues' own display options. None until they first change, and then
    /// the page reads the global set (seeded on first edit).
    #[serde(default)]
    pub my_issues_display: Option<DisplayOptions>,
    /// Global board issue filters: what a team without its own entry in
    /// `team_filters` starts from, and what cross-team pages use.
    #[serde(default)]
    pub filters: Filters,
    /// Per-team filters, by local team id. A team gets an entry the first time
    /// its filters change or are pruned, copied from the global set.
    #[serde(default)]
    pub team_filters: BTreeMap<String, Filters>,
    /// My Issues' own filters; None until first changed or pruned.
    #[serde(default)]
    pub my_issues_filters: Option<Filters>,
    /// Key of the selected color theme (see `theme::THEMES`).
    #[serde(default = "default_theme")]
    pub theme: String,
    /// UI font key (see `crate::fonts`). Local-only, never synced; read at
    /// launch, so changes apply after a restart.
    #[serde(default = "default_font")]
    pub font: String,
    /// Terminal font key (a monospace entry of `crate::fonts`). Device-wide,
    /// local-only, applied live and separate from the UI font.
    #[serde(default = "default_terminal_font")]
    pub terminal_font: String,
    /// Terminal font size in points. Device-wide, local-only, applied live.
    #[serde(default = "default_terminal_font_size")]
    pub terminal_font_size: f32,
    /// Post native macOS banners for new inbox notifications when the window
    /// is unfocused. Local-only, never synced.
    #[serde(default = "default_true")]
    pub native_notifications: bool,
    /// Whether the project page shows its properties sidebar (`/` toggles it).
    /// Device-wide and local-only, hidden by default.
    #[serde(default)]
    pub project_properties_open: bool,
    /// Whether the left teams sidebar is collapsed (Cmd+B toggles it).
    /// Device-wide and local-only, shown by default.
    #[serde(default)]
    pub sidebar_collapsed: bool,
    /// Whether the issue page shows its Properties (`/` toggles it).
    /// Device-wide and local-only, shown by default.
    #[serde(default = "default_true")]
    pub issue_properties_open: bool,
    /// Whether the project page always stacks Burndown above Issues, at any
    /// width. Device-wide and local-only; off means side by side when roomy.
    #[serde(default)]
    pub project_cards_stacked: bool,
    /// Where the Terminal panel docks: "bottom" or "right" (see
    /// `terminal::panel::Dock`). Device-wide and local-only.
    #[serde(default = "default_terminal_dock")]
    pub terminal_dock: String,
    /// Width of the right-docked Terminal panel; `None` is the default.
    #[serde(default)]
    pub terminal_panel_width: Option<f32>,
    /// Window background opacity in percent (`theme::MIN_WINDOW_OPACITY..=100`).
    /// Device-wide, local-only, applied live.
    #[serde(default = "default_window_opacity")]
    pub window_opacity: u8,
    /// Background blur radius behind a translucent window (0 is off, up to
    /// `window_effects::MAX_BLUR_RADIUS`). Device-wide, local-only, applied live.
    #[serde(default = "default_window_blur")]
    pub window_blur: u8,
    /// Self-update state that must survive a relaunch (`[updater]`).
    #[serde(default)]
    pub updater: UpdaterConfig,
}

/// The updater's saved state. Versions are stored as strings; a value that
/// doesn't parse is dropped on restore rather than failing the whole config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdaterConfig {
    /// "Check for updates automatically". On by default.
    #[serde(default = "default_true")]
    pub auto_check: bool,
    #[serde(default)]
    pub last_checked: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_failure: Option<SavedFailure>,
    /// The version whose ready notice Later hid.
    #[serde(default)]
    pub dismissed_notice: Option<String>,
    /// Newest version that failed a security check.
    #[serde(default)]
    pub blocked_version: Option<String>,
    /// Broken-download strikes against one version.
    #[serde(default)]
    pub broken: Option<SavedStrikes>,
    /// Set just before Restart now exits and cleared at the next launch: the
    /// version it installed, so that launch can say "JAIT is now X" once. Not
    /// part of the machine's state (`from_state` leaves it empty; the app keeps
    /// the stored value when it saves).
    #[serde(default)]
    pub relaunch: Option<SavedRelaunch>,
}

/// What the "JAIT is now X" toast needs after a Restart now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedRelaunch {
    pub version: String,
    /// That version's own Release notes (Markdown), when they were known.
    #[serde(default)]
    pub notes: Option<String>,
}

impl SavedRelaunch {
    /// The marker for installing `version`: its own entry out of `notes`.
    pub fn for_install(version: &Version, notes: &ReleaseNotes) -> Self {
        let notes = match notes {
            ReleaseNotes::Available(entries) => entries
                .iter()
                .find(|e| &e.version == version)
                .map(|e| e.notes.trim().to_string())
                .filter(|n| !n.is_empty()),
            ReleaseNotes::Unavailable => None,
        };
        Self { version: version.to_string(), notes }
    }

    /// The toast text to show on launch, if this marker is for the version now
    /// running (a stale marker shows nothing). Pure.
    pub fn toast_message(&self, running: &Version) -> Option<String> {
        if Version::parse(&self.version).ok().as_ref() != Some(running) {
            return None;
        }
        let mut msg = format!("JAIT is now {}", self.version);
        if let Some(notes) = &self.notes {
            let lines: Vec<&str> = notes.lines().map(str::trim).filter(|l| !l.is_empty()).take(4).collect();
            if !lines.is_empty() {
                msg.push('\n');
                msg.push_str(&lines.join("\n"));
            }
        }
        Some(msg)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedFailure {
    pub at: DateTime<Utc>,
    pub kind: FailureKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedStrikes {
    pub version: String,
    pub strikes: u8,
}

impl Default for UpdaterConfig {
    fn default() -> Self {
        Self {
            auto_check: true,
            last_checked: None,
            last_failure: None,
            dismissed_notice: None,
            blocked_version: None,
            broken: None,
            relaunch: None,
        }
    }
}

impl UpdaterConfig {
    /// Snapshot the state's persisted fields.
    pub fn from_state(state: &UpdaterState) -> Self {
        Self {
            auto_check: state.auto_check(),
            last_checked: state.view().last_checked,
            last_failure: state.last_failure().map(|f| SavedFailure {
                at: f.at,
                kind: f.kind,
                message: f.message.clone(),
            }),
            dismissed_notice: state.dismissed_notice().map(|v| v.to_string()),
            blocked_version: state.blocked_version().map(|v| v.to_string()),
            broken: state.broken_strikes().map(|(v, n)| SavedStrikes {
                version: v.to_string(),
                strikes: n,
            }),
            relaunch: None,
        }
    }

    /// Restore a freshly created state from the saved fields. The auto-check
    /// setting goes in through `Event::SetAutoCheck`, as the machine documents.
    pub fn restore(&self, state: &mut UpdaterState) {
        state.update(Event::SetAutoCheck(self.auto_check));
        state.set_last_checked(self.last_checked);
        state.set_last_failure(self.last_failure.as_ref().map(|f| LastFailure {
            at: f.at,
            kind: f.kind,
            message: f.message.clone(),
        }));
        state.set_dismissed_notice(parse_version(&self.dismissed_notice));
        state.set_blocked_version(parse_version(&self.blocked_version));
        state.set_broken_strikes(
            self.broken
                .as_ref()
                .and_then(|b| Version::parse(&b.version).ok().map(|v| (v, b.strikes))),
        );
    }
}

fn parse_version(s: &Option<String>) -> Option<Version> {
    s.as_deref().and_then(|s| Version::parse(s).ok())
}

fn default_window_opacity() -> u8 {
    100
}

fn default_window_blur() -> u8 {
    20
}

fn default_terminal_dock() -> String {
    "bottom".to_string()
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

fn default_terminal_font() -> String {
    crate::fonts::DEFAULT_TERMINAL_FONT_KEY.to_string()
}

fn default_terminal_font_size() -> f32 {
    crate::fonts::DEFAULT_TERMINAL_SIZE
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
            team_display: BTreeMap::new(),
            my_issues_display: None,
            filters: Filters::default(),
            team_filters: BTreeMap::new(),
            my_issues_filters: None,
            theme: default_theme(),
            font: default_font(),
            terminal_font: default_terminal_font(),
            terminal_font_size: default_terminal_font_size(),
            native_notifications: true,
            project_properties_open: false,
            sidebar_collapsed: false,
            issue_properties_open: true,
            project_cards_stacked: false,
            terminal_dock: default_terminal_dock(),
            terminal_panel_width: None,
            window_opacity: default_window_opacity(),
            window_blur: default_window_blur(),
            updater: UpdaterConfig::default(),
        }
    }
}

impl Config {
    /// The display options for a board. A team with no entry reads the global
    /// set, so an upgraded config shows nothing different.
    pub fn display_for(&self, scope: &BoardScope) -> &DisplayOptions {
        match scope {
            BoardScope::Global => &self.display,
            BoardScope::MyIssues => self.my_issues_display.as_ref().unwrap_or(&self.display),
            BoardScope::Team(id) => self.team_display.get(id).unwrap_or(&self.display),
        }
    }

    /// Mutable display options for a board. A team with no entry gets one,
    /// seeded from the global set, so the edit touches only that team.
    pub fn display_for_mut(&mut self, scope: &BoardScope) -> &mut DisplayOptions {
        match scope {
            BoardScope::Global => &mut self.display,
            BoardScope::MyIssues => {
                let seed = &self.display;
                self.my_issues_display.get_or_insert_with(|| seed.clone())
            }
            BoardScope::Team(id) => {
                let seed = &self.display;
                self.team_display
                    .entry(id.clone())
                    .or_insert_with(|| seed.clone())
            }
        }
    }
}

impl Config {
    /// The filters for a board. A team with no entry reads the global set.
    pub fn filters_for(&self, scope: &BoardScope) -> &Filters {
        match scope {
            BoardScope::Global => &self.filters,
            BoardScope::MyIssues => self.my_issues_filters.as_ref().unwrap_or(&self.filters),
            BoardScope::Team(id) => self.team_filters.get(id).unwrap_or(&self.filters),
        }
    }

    /// Mutable filters for a board. A team with no entry gets one, seeded from
    /// the global set.
    pub fn filters_for_mut(&mut self, scope: &BoardScope) -> &mut Filters {
        match scope {
            BoardScope::Global => &mut self.filters,
            BoardScope::MyIssues => {
                let seed = &self.filters;
                self.my_issues_filters.get_or_insert_with(|| seed.clone())
            }
            BoardScope::Team(id) => {
                let seed = &self.filters;
                self.team_filters
                    .entry(id.clone())
                    .or_insert_with(|| seed.clone())
            }
        }
    }

    /// Drop filter ids the loaded data doesn't contain from one board's
    /// filters only. A team with no entry keeps reading the global set unless
    /// pruning changes its copy. True if anything changed.
    pub fn prune_filters_for(
        &mut self,
        scope: &BoardScope,
        members: &[Member],
        labels: &[Label],
        lanes: &[Lane],
        projects: &[Project],
    ) -> bool {
        let mut working = self.filters_for(scope).clone();
        if !working.prune(members, labels, lanes, projects) {
            return false;
        }
        *self.filters_for_mut(scope) = working;
        true
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
        let cfg = Config { native_notifications: false, ..Default::default() };
        let back: Config = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert!(!back.native_notifications);
    }

    #[test]
    fn layout_options_default_off_and_round_trip() {
        let old: Config = toml::from_str("api_base_url = \"http://x\"\n").unwrap();
        assert!(!old.project_cards_stacked);
        assert_eq!(old.terminal_dock, "bottom");
        assert_eq!(old.terminal_panel_width, None);
        let cfg = Config {
            project_cards_stacked: true,
            terminal_dock: "right".into(),
            terminal_panel_width: Some(512.0),
            ..Default::default()
        };
        let back: Config = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert!(back.project_cards_stacked);
        assert_eq!(back.terminal_dock, "right");
        assert_eq!(back.terminal_panel_width, Some(512.0));
    }

    #[test]
    fn issue_properties_default_shown_and_round_trip() {
        let old: Config = toml::from_str("api_base_url = \"http://x\"\n").unwrap();
        assert!(old.issue_properties_open);
        let mut cfg = Config::default();
        assert!(cfg.issue_properties_open);
        cfg.issue_properties_open = false;
        let back: Config = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert!(!back.issue_properties_open);
    }

    #[test]
    fn project_properties_default_hidden_and_round_trip() {
        let old: Config = toml::from_str("api_base_url = \"http://x\"\n").unwrap();
        assert!(!old.project_properties_open);
        let mut cfg = Config::default();
        assert!(!cfg.project_properties_open);
        cfg.project_properties_open = true;
        let back: Config = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert!(back.project_properties_open);
    }

    #[test]
    fn terminal_font_round_trips_and_old_config_defaults_to_jetbrains_mono() {
        let mut cfg = Config::default();
        assert_eq!(cfg.terminal_font, "jetbrains-mono");
        cfg.terminal_font = "fira-code".into();
        let back: Config = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert_eq!(back.terminal_font, "fira-code");
        let old: Config = toml::from_str("api_base_url = \"http://x\"\n").unwrap();
        assert_eq!(old.terminal_font, "jetbrains-mono");
    }

    #[test]
    fn terminal_size_round_trips_and_old_config_defaults_to_13() {
        let mut cfg = Config::default();
        assert_eq!(cfg.terminal_font_size, 13.0);
        cfg.terminal_font_size = 16.0;
        let back: Config = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert_eq!(back.terminal_font_size, 16.0);
        let old: Config = toml::from_str("api_base_url = \"http://x\"\n").unwrap();
        assert_eq!(old.terminal_font_size, 13.0);
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
        let cfg = Config { theme: "warm-paper".to_string(), ..Default::default() };
        let back: Config = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert_eq!(back.theme, "warm-paper");
    }

    #[test]
    fn team_without_entry_reads_the_global_display() {
        let mut cfg = Config::default();
        cfg.display.group_by = GroupBy::Assignee;
        let a = BoardScope::Team("a".into());
        assert_eq!(cfg.display_for(&a), &cfg.display);
        assert!(cfg.team_display.is_empty());
    }

    #[test]
    fn editing_one_team_seeds_from_global_and_leaves_others_alone() {
        let mut cfg = Config::default();
        cfg.display.order_by = OrderBy::DueDate;
        let a = BoardScope::Team("a".into());
        let b = BoardScope::Team("b".into());
        cfg.display_for_mut(&a).group_by = GroupBy::Priority;
        assert_eq!(cfg.display_for(&a).group_by, GroupBy::Priority);
        assert_eq!(cfg.display_for(&a).order_by, OrderBy::DueDate);
        assert_eq!(cfg.display_for(&b), &cfg.display);
        assert_ne!(cfg.display.group_by, GroupBy::Priority);
    }

    #[test]
    fn team_display_round_trips_and_old_config_has_none() {
        let mut cfg = Config::default();
        cfg.display_for_mut(&BoardScope::Team("3f2a-uuid".into())).group_by = GroupBy::Assignee;
        let back: Config = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert_eq!(back.team_display, cfg.team_display);
        let old: Config = toml::from_str("[display]\ngroup_by = \"priority\"\n").unwrap();
        assert!(old.team_display.is_empty());
    }

    fn lane(id: &str) -> Lane {
        Lane {
            id: id.into(),
            name: String::new(),
            color: None,
            position: 0,
            team_id: "t".into(),
            remote_id: None,
            category: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn team_filters_seed_from_global_and_are_independent() {
        let mut cfg = Config::default();
        cfg.filters.toggle(FilterField::Priority, "1");
        let a = BoardScope::Team("a".into());
        let b = BoardScope::Team("b".into());
        assert_eq!(cfg.filters_for(&a), &cfg.filters);
        cfg.filters_for_mut(&a).toggle(FilterField::Priority, "2");
        assert_eq!(cfg.filters_for(&a).priorities, vec!["1", "2"]);
        assert_eq!(cfg.filters_for(&b).priorities, vec!["1"]);
        assert_eq!(cfg.filters.priorities, vec!["1"]);
    }

    #[test]
    fn pruning_only_touches_the_team_it_belongs_to() {
        let mut cfg = Config::default();
        let a = BoardScope::Team("a".into());
        let b = BoardScope::Team("b".into());
        cfg.filters_for_mut(&a).toggle(FilterField::Status, "lane-a");
        cfg.filters_for_mut(&b).toggle(FilterField::Status, "lane-b");
        cfg.filters.toggle(FilterField::Status, "lane-b");
        // Team b's data: only lane-b exists.
        let lanes = [lane("lane-b")];
        assert!(!cfg.prune_filters_for(&b, &[], &[], &lanes, &[]));
        assert_eq!(cfg.filters_for(&b).statuses, vec!["lane-b"]);
        assert_eq!(cfg.filters_for(&a).statuses, vec!["lane-a"]);
        // Team a's data lacks lane-a: only a's set drops it; global is untouched.
        assert!(cfg.prune_filters_for(&a, &[], &[], &lanes, &[]));
        assert!(cfg.filters_for(&a).statuses.is_empty());
        assert_eq!(cfg.filters_for(&b).statuses, vec!["lane-b"]);
        assert_eq!(cfg.filters.statuses, vec!["lane-b"]);
    }

    #[test]
    fn pruning_a_seeded_team_copies_instead_of_editing_global() {
        let mut cfg = Config::default();
        cfg.filters.toggle(FilterField::Status, "other-team-lane");
        let c = BoardScope::Team("c".into());
        assert!(cfg.prune_filters_for(&c, &[], &[], &[], &[]));
        assert!(cfg.filters_for(&c).statuses.is_empty());
        assert_eq!(cfg.filters.statuses, vec!["other-team-lane"]);
    }

    #[test]
    fn team_filters_round_trip_and_old_config_has_none() {
        let mut cfg = Config::default();
        cfg.filters_for_mut(&BoardScope::Team("t-1".into()))
            .toggle(FilterField::Priority, "3");
        let back: Config = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert_eq!(back.team_filters, cfg.team_filters);
        let old: Config = toml::from_str("[filters]\npriorities = [\"1\"]\n").unwrap();
        assert!(old.team_filters.is_empty());
        assert_eq!(old.filters.priorities, vec!["1"]);
    }

    #[test]
    fn my_issues_is_separate_from_teams_and_seeds_from_global() {
        let mut cfg = Config::default();
        cfg.display.order_by = OrderBy::DueDate;
        cfg.filters.toggle(FilterField::Priority, "1");
        let team = BoardScope::Team("a".into());
        assert_eq!(cfg.display_for(&BoardScope::MyIssues), &cfg.display);
        assert_eq!(cfg.filters_for(&BoardScope::MyIssues), &cfg.filters);

        cfg.display_for_mut(&BoardScope::MyIssues).group_by = GroupBy::Priority;
        cfg.filters_for_mut(&BoardScope::MyIssues)
            .toggle(FilterField::Priority, "2");
        assert_eq!(cfg.display_for(&BoardScope::MyIssues).order_by, OrderBy::DueDate);
        assert_eq!(cfg.filters_for(&BoardScope::MyIssues).priorities, vec!["1", "2"]);
        assert_eq!(cfg.display_for(&team), &cfg.display);
        assert_eq!(cfg.filters_for(&team).priorities, vec!["1"]);

        cfg.display_for_mut(&team).group_by = GroupBy::Assignee;
        cfg.filters_for_mut(&team).clear();
        assert_eq!(cfg.display_for(&BoardScope::MyIssues).group_by, GroupBy::Priority);
        assert_eq!(cfg.filters_for(&BoardScope::MyIssues).priorities, vec!["1", "2"]);
    }

    fn at(h: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2026-11-17T{h:02}:00:00Z"))
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn missing_updater_section_loads_as_defaults() {
        let cfg: Config = toml::from_str("api_base_url = \"http://x\"\n").unwrap();
        assert!(cfg.updater.auto_check);
        assert_eq!(cfg.updater, UpdaterConfig::default());
        let state = UpdaterState::new(Version::new(0, 12, 0));
        assert_eq!(UpdaterConfig::from_state(&state), UpdaterConfig::default());
    }

    #[test]
    fn updater_state_survives_a_relaunch_through_the_config() {
        let mut s = UpdaterState::new(Version::new(0, 12, 0));
        s.update(Event::SetAutoCheck(false));
        s.set_last_checked(Some(at(9)));
        s.set_last_failure(Some(LastFailure {
            at: at(10),
            kind: FailureKind::Security,
            message: "codesign failed".into(),
        }));
        s.set_dismissed_notice(Some(Version::new(0, 13, 0)));
        s.set_blocked_version(Some(Version::new(0, 14, 0)));
        s.set_broken_strikes(Some((Version::new(0, 15, 0), 2)));

        let cfg = Config { updater: UpdaterConfig::from_state(&s), ..Default::default() };
        let back: Config = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();

        let mut r = UpdaterState::new(Version::new(0, 12, 0));
        back.updater.restore(&mut r);
        assert!(!r.auto_check());
        assert_eq!(r.view().last_checked, Some(at(9)));
        assert_eq!(r.view().last_failure, s.view().last_failure);
        assert_eq!(r.dismissed_notice(), Some(&Version::new(0, 13, 0)));
        assert_eq!(r.blocked_version(), Some(&Version::new(0, 14, 0)));
        assert_eq!(r.broken_downloads(&Version::new(0, 15, 0)), 2);
    }

    fn entry(v: Version, notes: &str) -> crate::updater::manifest::ReleaseEntry {
        crate::updater::manifest::ReleaseEntry {
            version: v,
            published_at: chrono::DateTime::parse_from_rfc3339("2026-11-16T00:00:00Z").unwrap(),
            notes: notes.into(),
        }
    }

    #[test]
    fn relaunch_marker_takes_only_the_installed_versions_notes() {
        let notes = ReleaseNotes::Available(vec![
            entry(Version::new(0, 13, 0), "- Faster boards\n- Fix\n"),
            entry(Version::new(0, 12, 1), "- Older"),
        ]);
        let m = SavedRelaunch::for_install(&Version::new(0, 13, 0), &notes);
        assert_eq!(m.notes.as_deref(), Some("- Faster boards\n- Fix"));
        let none = SavedRelaunch::for_install(&Version::new(0, 13, 0), &ReleaseNotes::Unavailable);
        assert_eq!(none.notes, None);
    }

    #[test]
    fn relaunch_toast_shows_only_for_the_running_version() {
        let m = SavedRelaunch { version: "0.13.0".into(), notes: Some("- Faster boards".into()) };
        assert_eq!(
            m.toast_message(&Version::new(0, 13, 0)).as_deref(),
            Some("JAIT is now 0.13.0\n- Faster boards")
        );
        assert_eq!(m.toast_message(&Version::new(0, 12, 0)), None);
        let bare = SavedRelaunch { version: "0.13.0".into(), notes: None };
        assert_eq!(bare.toast_message(&Version::new(0, 13, 0)).as_deref(), Some("JAIT is now 0.13.0"));
    }

    #[test]
    fn relaunch_marker_round_trips_and_is_absent_by_default() {
        assert_eq!(UpdaterConfig::default().relaunch, None);
        let cfg = Config {
            updater: UpdaterConfig {
                relaunch: Some(SavedRelaunch { version: "0.13.0".into(), notes: None }),
                ..UpdaterConfig::default()
            },
            ..Default::default()
        };
        let back: Config = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert_eq!(back.updater.relaunch, cfg.updater.relaunch);
    }

    #[test]
    fn restored_failure_is_recorded_but_not_shown() {
        let mut s = UpdaterState::new(Version::new(0, 12, 0));
        s.set_last_failure(Some(LastFailure {
            at: at(10),
            kind: FailureKind::Check,
            message: "offline".into(),
        }));
        assert_eq!(s.view().status, crate::updater::machine::Status::Idle);
        assert!(s.view().last_failure.is_some());
    }

    #[test]
    fn unparseable_saved_versions_are_dropped_not_fatal() {
        let cfg: Config = toml::from_str(
            "[updater]\nblocked_version = \"nope\"\n[updater.broken]\nversion = \"x\"\nstrikes = 2\n",
        )
        .unwrap();
        let mut s = UpdaterState::new(Version::new(0, 12, 0));
        cfg.updater.restore(&mut s);
        assert_eq!(s.blocked_version(), None);
        assert_eq!(s.broken_strikes(), None);
    }

    #[test]
    fn my_issues_round_trips_and_old_config_has_none() {
        let mut cfg = Config::default();
        cfg.display_for_mut(&BoardScope::MyIssues).group_by = GroupBy::Assignee;
        cfg.filters_for_mut(&BoardScope::MyIssues)
            .toggle(FilterField::Priority, "3");
        let back: Config = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert_eq!(back.my_issues_display, cfg.my_issues_display);
        assert_eq!(back.my_issues_filters, cfg.my_issues_filters);
        let old: Config = toml::from_str("api_base_url = \"http://x\"\n").unwrap();
        assert!(old.my_issues_display.is_none() && old.my_issues_filters.is_none());
        assert_eq!(old.display_for(&BoardScope::MyIssues), &old.display);
    }
}
