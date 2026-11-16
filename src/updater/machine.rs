//! The updater state machine: the deep seam of the self-update feature.
//!
//! Pure. Events go in through [`UpdaterState::update`]; the state changes and a
//! list of [`Effect`]s comes out. The app maps effects to Tasks (HTTP, ditto,
//! codesign, the swap) and feeds their results back in as [`Event`]s. The UI
//! only reads [`UpdaterState::view`] and never reimplements a rule.
//!
//! Lifted from the prototype's `Updater` reducer (JRST-473). The clock is never
//! read here: events that need a time carry it (`at`).
//!
//! Extending: add a variant to [`Event`] / [`Effect`], a match arm in
//! `update`, and (if the UI needs it) a field on [`UpdaterView`]. Match arms for
//! unrelated events stay untouched, so tickets can extend this in parallel.

use chrono::{DateTime, Utc};
use semver::Version;

use super::manifest::{Manifest, ParsedManifest, ReleaseNotes};

/// Who asked for a check. Manual results are always shown; automatic ones stay
/// quiet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckKind {
    Manual,
    Automatic,
}

/// Everything the download and staging effects need about an Update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    pub version: Version,
    pub zip_url: String,
    pub zip_sha256: String,
    pub zip_size: u64,
    /// Release notes from the installed version (exclusive) to `version`.
    pub notes: ReleaseNotes,
}

impl UpdateInfo {
    fn from_manifest(m: &Manifest, installed: &Version) -> Self {
        UpdateInfo {
            version: m.version.clone(),
            zip_url: m.zip_url.clone(),
            zip_sha256: m.zip_sha256.clone(),
            zip_size: m.zip_size,
            notes: m.notes_since(installed),
        }
    }
}

/// Things that happened. The app feeds these in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A check was requested (by the user, or by the launch / timer / wake
    /// schedule).
    CheckStarted(CheckKind),
    /// The manifest was fetched and parsed. `at` is when the check finished.
    CheckFinished {
        at: DateTime<Utc>,
        manifest: ParsedManifest,
    },
    /// `received` bytes of the zip have arrived so far.
    DownloadProgress { received: u64 },
    /// The whole zip was downloaded.
    DownloadFinished,
    /// The zip passed every check and is staged next to the installed app.
    StagingVerified,
    /// Later or x on the ready notice: hide the toast for this version only
    /// (JRST-501). The chip stays.
    NoticeDismissed { version: Version },
}

/// Things the app must do. The machine returns these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Fetch and parse the Release manifest, then send `Event::CheckFinished`.
    FetchManifest,
    /// Download the Update's zip.
    Download(UpdateInfo),
    /// Verify and extract the downloaded zip, then send
    /// `Event::StagingVerified`.
    Stage(UpdateInfo),
    /// Show the one-time "ready" notice for this version (toast).
    ShowReadyNotice {
        version: Version,
        notes: ReleaseNotes,
    },
}

/// What the updater is busy with right now.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Activity {
    Idle,
    Checking,
    Downloading { info: UpdateInfo, received: u64 },
    Verifying(UpdateInfo),
}

/// What the UI should say about the updater. Derived from state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Nothing to show (idle, or an automatic step in progress).
    Idle,
    /// "Checking…" (manual checks only).
    Checking,
    /// "Up to date ✓" (after a manual check only).
    UpToDate,
    /// Download progress (manual checks only).
    Downloading { version: Version, percent: u8 },
    /// Verifying the download (manual checks only).
    Verifying { version: Version },
}

/// A Staged update: downloaded, verified, waiting to install at the next
/// Clean quit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyUpdate {
    pub version: Version,
    pub notes: ReleaseNotes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdaterView {
    pub status: Status,
    /// The Staged update, if any. Independent of `status`.
    pub ready: Option<ReadyUpdate>,
    pub last_checked: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct UpdaterState {
    installed: Version,
    activity: Activity,
    /// Did the user ask for the step that's running?
    manual: bool,
    last_checked: Option<DateTime<Utc>>,
    up_to_date_shown: bool,
    staged: Option<UpdateInfo>,
    /// The one version whose ready toast the user dismissed with Later.
    notice_dismissed: Option<Version>,
}

impl UpdaterState {
    pub fn new(installed: Version) -> Self {
        UpdaterState {
            installed,
            activity: Activity::Idle,
            manual: false,
            last_checked: None,
            up_to_date_shown: false,
            staged: None,
            notice_dismissed: None,
        }
    }

    /// The version whose ready notice was dismissed, for persisting.
    pub fn dismissed_notice(&self) -> Option<&Version> {
        self.notice_dismissed.as_ref()
    }

    /// Restore a persisted dismissal at launch.
    pub fn set_dismissed_notice(&mut self, version: Option<Version>) {
        self.notice_dismissed = version;
    }

    pub fn installed(&self) -> &Version {
        &self.installed
    }

    /// Is a check or download running?
    pub fn is_busy(&self) -> bool {
        !matches!(self.activity, Activity::Idle)
    }

    pub fn view(&self) -> UpdaterView {
        let status = match &self.activity {
            Activity::Checking if self.manual => Status::Checking,
            Activity::Downloading { info, received } if self.manual => Status::Downloading {
                version: info.version.clone(),
                percent: percent(*received, info.zip_size),
            },
            Activity::Verifying(info) if self.manual => Status::Verifying {
                version: info.version.clone(),
            },
            Activity::Idle if self.up_to_date_shown => Status::UpToDate,
            _ => Status::Idle,
        };
        UpdaterView {
            status,
            ready: self.staged.as_ref().map(|i| ReadyUpdate {
                version: i.version.clone(),
                notes: i.notes.clone(),
            }),
            last_checked: self.last_checked,
        }
    }

    /// Apply an event; returns the effects the app must carry out.
    pub fn update(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::CheckStarted(kind) => self.check_started(kind),
            Event::CheckFinished { at, manifest } => self.check_finished(at, manifest),
            Event::DownloadProgress { received } => self.download_progress(received),
            Event::DownloadFinished => self.download_finished(),
            Event::StagingVerified => self.staging_verified(),
            Event::NoticeDismissed { version } => {
                self.notice_dismissed = Some(version);
                vec![]
            }
        }
    }

    fn check_started(&mut self, kind: CheckKind) -> Vec<Effect> {
        if self.is_busy() {
            // A step is already running. A manual request adopts it, so its
            // result is shown. No second check starts.
            if kind == CheckKind::Manual {
                self.manual = true;
            }
            return vec![];
        }
        self.activity = Activity::Checking;
        self.manual = kind == CheckKind::Manual;
        self.up_to_date_shown = false;
        vec![Effect::FetchManifest]
    }

    /// Is this version already staged (or older than what is)?
    fn has_staged(&self, version: &Version) -> bool {
        self.staged.as_ref().is_some_and(|s| &s.version >= version)
    }

    fn download_progress(&mut self, received: u64) -> Vec<Effect> {
        if let Activity::Downloading { received: r, .. } = &mut self.activity {
            *r = received;
        }
        vec![]
    }

    fn download_finished(&mut self) -> Vec<Effect> {
        let Activity::Downloading { info, .. } = &self.activity else {
            return vec![];
        };
        let info = info.clone();
        self.activity = Activity::Verifying(info.clone());
        vec![Effect::Stage(info)]
    }

    fn staging_verified(&mut self) -> Vec<Effect> {
        let Activity::Verifying(info) = &self.activity else {
            return vec![];
        };
        let info = info.clone();
        self.activity = Activity::Idle;
        self.manual = false;
        self.staged = Some(info.clone());
        if self.notice_dismissed.as_ref() == Some(&info.version) {
            return vec![];
        }
        vec![Effect::ShowReadyNotice {
            version: info.version,
            notes: info.notes,
        }]
    }

    fn check_finished(&mut self, at: DateTime<Utc>, manifest: ParsedManifest) -> Vec<Effect> {
        if self.activity != Activity::Checking {
            return vec![]; // stale answer, no check is running
        }
        self.last_checked = Some(at);
        match manifest {
            ParsedManifest::SelfInstall(m)
                if m.is_update(&self.installed) && !self.has_staged(&m.version) =>
            {
                let info = UpdateInfo::from_manifest(&m, &self.installed);
                self.activity = Activity::Downloading {
                    info: info.clone(),
                    received: 0,
                };
                vec![Effect::Download(info)]
            }
            // A DMG-only offer is handled by a later ticket (JRST-515); until
            // then it is treated as nothing to self-install.
            _ => {
                self.activity = Activity::Idle;
                self.up_to_date_shown = self.manual;
                self.manual = false;
                vec![]
            }
        }
    }
}

fn percent(received: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    (received.min(total) * 100 / total) as u8
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::updater::manifest::parse;

    pub fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    pub fn at(h: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2026-11-17T{h:02}:00:00Z"))
            .unwrap()
            .with_timezone(&Utc)
    }

    /// A schema 1 manifest for `version`, with notes for each of `notes`.
    pub fn manifest(version: &str, notes: &[&str]) -> ParsedManifest {
        let releases: Vec<_> = notes
            .iter()
            .map(|n| {
                serde_json::json!({
                    "version": n, "published_at": "2026-11-17T10:00:00Z", "notes": format!("notes {n}")
                })
            })
            .collect();
        let base = "https://github.com/denvermullets/jait-releases/releases/download";
        let j = serde_json::json!({
            "schema": 1, "version": version, "published_at": "2026-11-17T10:00:00Z",
            "zip_url": format!("{base}/v{version}/JAIT-{version}-arm64.zip"),
            "zip_sha256": "0".repeat(64), "zip_size": 1000u64,
            "dmg_url": format!("{base}/v{version}/JAIT-{version}.dmg"),
            "releases": releases,
        });
        parse(j.to_string().as_bytes()).unwrap()
    }

    pub fn finished(h: u32, version: &str) -> Event {
        Event::CheckFinished {
            at: at(h),
            manifest: manifest(version, &[version]),
        }
    }

    fn state() -> UpdaterState {
        UpdaterState::new(v("0.12.0"))
    }

    #[test]
    fn manual_check_shows_checking_then_up_to_date() {
        let mut s = state();
        assert_eq!(
            s.update(Event::CheckStarted(CheckKind::Manual)),
            vec![Effect::FetchManifest]
        );
        assert_eq!(s.view().status, Status::Checking);
        assert_eq!(s.update(finished(9, "0.12.0")), vec![]);
        assert_eq!(s.view().status, Status::UpToDate);
    }

    #[test]
    fn automatic_check_that_finds_nothing_shows_nothing() {
        let mut s = state();
        assert_eq!(
            s.update(Event::CheckStarted(CheckKind::Automatic)),
            vec![Effect::FetchManifest]
        );
        assert_eq!(s.view().status, Status::Idle);
        s.update(finished(9, "0.12.0"));
        assert_eq!(s.view().status, Status::Idle);
    }

    #[test]
    fn older_manifest_is_not_an_update() {
        let mut s = state();
        s.update(Event::CheckStarted(CheckKind::Manual));
        assert_eq!(s.update(finished(9, "0.11.0")), vec![]);
        assert_eq!(s.view().status, Status::UpToDate);
    }

    #[test]
    fn finding_an_update_emits_a_download_with_notes_since_installed() {
        let mut s = state();
        s.update(Event::CheckStarted(CheckKind::Automatic));
        let ev = Event::CheckFinished {
            at: at(9),
            manifest: manifest("0.14.0", &["0.12.0", "0.13.0", "0.14.0"]),
        };
        let fx = s.update(ev);
        let [Effect::Download(info)] = fx.as_slice() else {
            panic!("expected one download effect, got {fx:?}");
        };
        assert_eq!(info.version, v("0.14.0"));
        let ReleaseNotes::Available(n) = &info.notes else {
            panic!()
        };
        assert_eq!(n.len(), 2);
        assert!(s.is_busy());
    }

    #[test]
    fn last_check_time_is_recorded_on_every_finished_check() {
        let mut s = state();
        assert_eq!(s.view().last_checked, None);
        s.update(Event::CheckStarted(CheckKind::Automatic));
        s.update(finished(9, "0.12.0"));
        assert_eq!(s.view().last_checked, Some(at(9)));
        s.update(Event::CheckStarted(CheckKind::Manual));
        s.update(finished(11, "0.13.0")); // finds an Update
        assert_eq!(s.view().last_checked, Some(at(11)));
    }

    #[test]
    fn a_new_check_clears_up_to_date() {
        let mut s = state();
        s.update(Event::CheckStarted(CheckKind::Manual));
        s.update(finished(9, "0.12.0"));
        s.update(Event::CheckStarted(CheckKind::Automatic));
        assert_eq!(s.view().status, Status::Idle);
    }

    #[test]
    fn manual_request_during_an_automatic_check_adopts_it() {
        let mut s = state();
        s.update(Event::CheckStarted(CheckKind::Automatic));
        assert_eq!(s.update(Event::CheckStarted(CheckKind::Manual)), vec![]);
        assert_eq!(s.view().status, Status::Checking);
        s.update(finished(9, "0.12.0"));
        assert_eq!(s.view().status, Status::UpToDate);
    }

    #[test]
    fn automatic_request_while_busy_is_ignored() {
        let mut s = state();
        s.update(Event::CheckStarted(CheckKind::Manual));
        assert_eq!(s.update(Event::CheckStarted(CheckKind::Automatic)), vec![]);
        assert_eq!(s.view().status, Status::Checking);
    }

    fn update_of(fx: Vec<Effect>) -> UpdateInfo {
        match fx.as_slice() {
            [Effect::Download(i)] => i.clone(),
            other => panic!("expected one download, got {other:?}"),
        }
    }

    #[test]
    fn automatic_update_goes_to_ready_with_no_visible_progress() {
        let mut s = state();
        s.update(Event::CheckStarted(CheckKind::Automatic));
        let info = update_of(s.update(finished(9, "0.13.0")));
        assert_eq!(s.view().status, Status::Idle);
        s.update(Event::DownloadProgress { received: 500 });
        assert_eq!(s.view().status, Status::Idle);
        assert_eq!(
            s.update(Event::DownloadFinished),
            vec![Effect::Stage(info.clone())]
        );
        assert_eq!(s.view().status, Status::Idle);
        assert_eq!(s.view().ready, None);
        let fx = s.update(Event::StagingVerified);
        assert_eq!(
            fx,
            vec![Effect::ShowReadyNotice {
                version: v("0.13.0"),
                notes: info.notes.clone()
            }]
        );
        let ready = s.view().ready.unwrap();
        assert_eq!(ready.version, v("0.13.0"));
        assert_eq!(s.view().status, Status::Idle);
        assert!(!s.is_busy());
    }

    #[test]
    fn manual_check_exposes_download_progress_then_verifying() {
        let mut s = state();
        s.update(Event::CheckStarted(CheckKind::Manual));
        update_of(s.update(finished(9, "0.13.0")));
        let downloading = |percent| Status::Downloading {
            version: v("0.13.0"),
            percent,
        };
        assert_eq!(s.view().status, downloading(0));
        s.update(Event::DownloadProgress { received: 250 });
        assert_eq!(s.view().status, downloading(25));
        s.update(Event::DownloadProgress { received: 5000 }); // past the size
        assert_eq!(s.view().status, downloading(100));
        s.update(Event::DownloadFinished);
        assert_eq!(s.view().status, Status::Verifying { version: v("0.13.0") });
        s.update(Event::StagingVerified);
        assert_eq!(s.view().status, Status::Idle);
        assert!(s.view().ready.is_some());
    }

    #[test]
    fn ready_notice_is_emitted_once_per_staging() {
        let mut s = state();
        s.update(Event::CheckStarted(CheckKind::Automatic));
        update_of(s.update(finished(9, "0.13.0")));
        s.update(Event::DownloadFinished);
        let fx = s.update(Event::StagingVerified);
        assert_eq!(fx.len(), 1);
        // A repeated or stray verified event does nothing.
        assert_eq!(s.update(Event::StagingVerified), vec![]);
        // A later check that finds the same version doesn't re-download it.
        s.update(Event::CheckStarted(CheckKind::Automatic));
        assert_eq!(s.update(finished(15, "0.13.0")), vec![]);
        assert!(s.view().ready.is_some());
    }

    #[test]
    fn download_events_outside_a_download_are_ignored() {
        let mut s = state();
        assert_eq!(s.update(Event::DownloadProgress { received: 1 }), vec![]);
        assert_eq!(s.update(Event::DownloadFinished), vec![]);
        assert_eq!(s.update(Event::StagingVerified), vec![]);
        assert_eq!(s.view().ready, None);
    }

    #[test]
    fn a_stale_check_result_is_ignored() {
        let mut s = state();
        assert_eq!(s.update(finished(9, "0.13.0")), vec![]);
        assert_eq!(s.view().last_checked, None);
    }

    // --- JRST-501: Later hides the notice for one version ---

    fn stage(s: &mut UpdaterState, h: u32, version: &str) -> Vec<Effect> {
        s.update(Event::CheckStarted(CheckKind::Automatic));
        update_of(s.update(finished(h, version)));
        s.update(Event::DownloadFinished);
        s.update(Event::StagingVerified)
    }

    #[test]
    fn later_hides_the_notice_on_a_re_ready_of_the_same_version() {
        let mut s = state();
        assert_eq!(stage(&mut s, 9, "0.13.0").len(), 1);
        s.update(Event::NoticeDismissed { version: v("0.13.0") });
        assert_eq!(s.dismissed_notice(), Some(&v("0.13.0")));
        // The same version is staged again (e.g. after a relaunch re-verify).
        s.staged = None;
        assert_eq!(stage(&mut s, 15, "0.13.0"), vec![]);
    }

    #[test]
    fn later_keeps_the_chip_ready() {
        let mut s = state();
        stage(&mut s, 9, "0.13.0");
        s.update(Event::NoticeDismissed { version: v("0.13.0") });
        assert_eq!(s.view().ready.unwrap().version, v("0.13.0"));
    }

    #[test]
    fn a_newer_version_shows_its_notice_after_later() {
        let mut s = state();
        stage(&mut s, 9, "0.13.0");
        s.update(Event::NoticeDismissed { version: v("0.13.0") });
        let fx = stage(&mut s, 15, "0.14.0");
        assert!(matches!(fx.as_slice(), [Effect::ShowReadyNotice { version, .. }] if *version == v("0.14.0")));
    }
}
