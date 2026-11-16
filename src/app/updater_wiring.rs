//! Wires the pure updater state machine into the app (project 320).
//!
//! Every updater input goes through [`App::apply_updater_event`]: it feeds the
//! `Event` to the machine, saves the persisted state, and maps the returned
//! `Effect`s to Tasks. A Task's result comes back as `Message::Updater(Event)`.
//! The I/O itself lives behind `updater::edge::UpdateEdge`.

use chrono::Utc;
use iced::Task;

use super::{App, Message};
use crate::config::UpdaterConfig;
use crate::updater::download::DownloadError;
use crate::updater::edge;
use crate::updater::install::InstallContext;
use crate::updater::machine::{CheckKind, Effect, Event, UpdateInfo};
use crate::updater::swap;
use crate::updater::verify::{StageError, StageRequest};
use iced::futures::SinkExt;

/// The updater event for a stage result. Pure.
fn stage_event(result: Result<(), StageError>, at: chrono::DateTime<Utc>) -> Event {
    match result {
        Ok(()) => Event::StagingVerified,
        Err(StageError::Broken(reason)) => Event::BrokenDownload { at, reason },
        Err(StageError::Security(reason)) => Event::SecurityFailure { at, reason },
    }
}

/// How often the Tick subscription fires. The machine decides whether 6 hours
/// have passed since the last check; this only bounds how late a check can be.
pub const TICK_EVERY: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Which background trigger fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Launched,
    Tick,
    Woke,
}

/// The event for a background trigger, or None when updates are disabled
/// (a dev build with no `.app`): nothing runs then. Pure.
fn background_event(
    install: &InstallContext,
    trigger: Trigger,
    at: chrono::DateTime<Utc>,
) -> Option<Event> {
    if matches!(install, InstallContext::Disabled) {
        return None;
    }
    Some(match trigger {
        Trigger::Launched => Event::Launched { at },
        Trigger::Tick => Event::Tick { at },
        Trigger::Woke => Event::Woke { at },
    })
}

/// Shown when the Check button is pressed in a build with no `.app` around it.
pub const DISABLED_NOTE: &str =
    "Updates are disabled: this build isn't running from a JAIT.app.";

impl App {
    /// The "Check for updates" button (Settings; the command palette sends the
    /// same message). Dev builds explain instead of fetching.
    pub(super) fn update_check_clicked(&mut self) -> Task<Message> {
        let disabled = matches!(self.install, InstallContext::Disabled);
        if let Some(form) = self.settings_form.as_mut() {
            form.update_note = disabled.then(|| DISABLED_NOTE.to_string());
        }
        if disabled {
            return Task::none();
        }
        self.apply_updater_event(Event::CheckStarted(CheckKind::Manual))
    }

    /// A background trigger (launch, periodic tick, wake from sleep). The
    /// machine decides whether a check is due; disabled builds do nothing.
    pub(super) fn updater_background(&mut self, trigger: Trigger) -> Task<Message> {
        match background_event(&self.install, trigger, Utc::now()) {
            Some(event) => self.apply_updater_event(event),
            None => Task::none(),
        }
    }

    /// Settings → Updates "Check for updates automatically". Saved through
    /// the updater's persisted state.
    pub(super) fn update_auto_check_toggled(&mut self, on: bool) -> Task<Message> {
        self.apply_updater_event(Event::SetAutoCheck(on))
    }

    /// Periodic tick and wake-from-sleep notices; none when disabled.
    pub(super) fn updater_subscription(&self) -> iced::Subscription<Message> {
        if matches!(self.install, InstallContext::Disabled) {
            return iced::Subscription::none();
        }
        iced::Subscription::batch([
            iced::time::every(TICK_EVERY).map(|_| Message::UpdaterTick),
            iced::Subscription::run(crate::wake::wakes).map(|_| Message::UpdaterWoke),
        ])
    }

    /// The "Download JAIT X" button: open the manifest's own `dmg_url`.
    pub(super) fn update_dmg_clicked(&mut self) -> Task<Message> {
        if let Some(offer) = self.updater.view().dmg_update {
            if let Err(e) = std::process::Command::new("open").arg(&offer.dmg_url).spawn() {
                if let Some(form) = self.settings_form.as_mut() {
                    form.update_note = Some(format!("Couldn't open the download: {e}"));
                }
            }
        }
        Task::none()
    }

    /// Feed one event to the updater, save its state, and start its effects.
    pub(super) fn apply_updater_event(&mut self, event: Event) -> Task<Message> {
        let effects = self.updater.update(event);
        self.persist_updater();
        let tasks: Vec<_> = effects
            .into_iter()
            .map(|effect| self.run_updater_effect(effect))
            .collect();
        Task::batch(tasks)
    }

    /// Save the updater's persisted fields to the config, only when they changed.
    fn persist_updater(&mut self) {
        let saved = UpdaterConfig::from_state(&self.updater);
        if saved == self.config.updater {
            return;
        }
        let mut cfg = self.config.clone();
        cfg.updater = saved;
        self.save_config_change(cfg);
    }

    /// Stream the Update's zip into the staging dir. Progress arrives as
    /// `DownloadProgress`; the end as `DownloadFinished`, `BrokenDownload` or
    /// `DownloadFailed`. A cancelled download ends silently.
    fn start_download(&self, info: UpdateInfo) -> Task<Message> {
        let InstallContext::SelfInstall { staging_dir, .. } = &self.install else {
            return Task::none();
        };
        let req = edge::DownloadRequest {
            url: info.zip_url,
            version: info.version,
            sha256: info.zip_sha256,
            size: info.zip_size,
            staging_dir: staging_dir.clone(),
        };
        let edge = self.update_edge.clone();
        let stream = iced::stream::channel(64, move |mut out| async move {
            let progress_out = std::sync::Mutex::new(out.clone());
            let on_progress = Box::new(move |received| {
                // Progress is best-effort: a full channel just skips a tick.
                if let Ok(mut tx) = progress_out.lock() {
                    let _ = tx.try_send(Event::DownloadProgress { received });
                }
            });
            let result = edge.download(req, on_progress).await;
            let at = Utc::now();
            let event = match result {
                Ok(()) => Event::DownloadFinished,
                Err(DownloadError::Failed(reason)) => Event::DownloadFailed { at, reason },
                Err(DownloadError::Broken(reason)) => Event::BrokenDownload { at, reason },
                Err(DownloadError::Cancelled) => return,
            };
            let _ = out.send(event).await;
        });
        Task::run(stream, Message::Updater)
    }

    /// Unzip and verify the downloaded Update. Ends as `StagingVerified`,
    /// `BrokenDownload` (unzip) or `SecurityFailure` (codesign/spctl/version).
    fn start_stage(&self, info: UpdateInfo) -> Task<Message> {
        let InstallContext::SelfInstall { bundle, staging_dir } = &self.install else {
            return Task::none();
        };
        let req = StageRequest {
            version: info.version,
            staging_dir: staging_dir.clone(),
            running_bundle: bundle.clone(),
            running_version: self.updater.installed().clone(),
        };
        let edge = self.update_edge.clone();
        Task::perform(
            async move { stage_event(edge.stage(req).await, Utc::now()) },
            Message::Updater,
        )
    }

    /// Swap the Staged update `version` in for the running bundle. Shared by
    /// the Clean-quit install and Restart now. Never called at launch.
    pub(super) fn swap_staged(&self, version: &semver::Version) -> Result<(), String> {
        let Some((bundle, staging_dir, version)) = swap::quit_plan(&self.install, Some(version))
        else {
            return Ok(());
        };
        self.update_edge.swap(bundle, staging_dir, version)
    }

    /// Runs on every Clean quit: drop an in-flight download, then install the
    /// Staged update if there is one (independent of the auto-check toggle).
    pub(super) fn install_on_quit(&mut self) -> Task<Message> {
        let task = self.apply_updater_event(Event::QuitConfirmed);
        if let Some(ready) = self.updater.view().ready {
            if let Err(e) = self.swap_staged(&ready.version) {
                eprintln!("update swap failed: {e}");
            }
        }
        task
    }

    /// Carry out one effect. Exhaustive on purpose: a new `Effect` variant must
    /// be handled (or deliberately ignored) here.
    fn run_updater_effect(&self, effect: Effect) -> Task<Message> {
        match effect {
            Effect::FetchManifest => {
                let edge = self.update_edge.clone();
                Task::perform(
                    async move {
                        let result = edge::check(edge.as_ref()).await;
                        let at = Utc::now();
                        match result {
                            Ok(manifest) => Event::CheckFinished { at, manifest },
                            Err(reason) => Event::CheckFailed { at, reason },
                        }
                    },
                    Message::Updater,
                )
            }
            Effect::Download(info) => self.start_download(info),
            Effect::CancelDownload => {
                self.update_edge.cancel_download();
                Task::none()
            }
            Effect::Stage(info) => self.start_stage(info),
            Effect::RemoveStaged { version } | Effect::DeleteStaging(version) => {
                let InstallContext::SelfInstall { staging_dir, .. } = &self.install else {
                    return Task::none();
                };
                let edge = self.update_edge.clone();
                let dir = staging_dir.clone();
                Task::perform(async move { edge.delete_staging(dir, version).await }, |_| {
                    Message::Noop
                })
            }
            // TODO(JRST-524): the ready toast and the "Restart to update?" confirm.
            Effect::ShowReadyNotice { .. } | Effect::ConfirmRestart { .. } => Task::none(),
            Effect::SwapStaged { version } => {
                // Failure only means the old version keeps running.
                if let Err(e) = self.swap_staged(&version) {
                    eprintln!("update swap failed: {e}");
                }
                Task::none()
            }
            // TODO(JRST-524): relaunch, exit.
            Effect::Relaunch | Effect::Exit => Task::none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_triggers_map_to_events() {
        let at = Utc::now();
        let install = InstallContext::SelfInstall {
            bundle: "/Applications/JAIT.app".into(),
            staging_dir: "/Applications/.jait-update".into(),
        };
        assert_eq!(
            background_event(&install, Trigger::Launched, at),
            Some(Event::Launched { at })
        );
        assert_eq!(background_event(&install, Trigger::Tick, at), Some(Event::Tick { at }));
        assert_eq!(background_event(&install, Trigger::Woke, at), Some(Event::Woke { at }));
    }

    #[test]
    fn disabled_builds_run_no_background_checks() {
        let at = Utc::now();
        for t in [Trigger::Launched, Trigger::Tick, Trigger::Woke] {
            assert_eq!(background_event(&InstallContext::Disabled, t, at), None);
        }
    }

    #[test]
    fn stage_results_map_to_updater_events() {
        let at = Utc::now();
        assert_eq!(stage_event(Ok(()), at), Event::StagingVerified);
        assert_eq!(
            stage_event(Err(StageError::Broken("x".into())), at),
            Event::BrokenDownload { at, reason: "x".into() }
        );
        assert_eq!(
            stage_event(Err(StageError::Security("y".into())), at),
            Event::SecurityFailure { at, reason: "y".into() }
        );
    }
}
