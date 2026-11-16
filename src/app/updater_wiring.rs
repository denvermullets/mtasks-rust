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
use iced::futures::SinkExt;

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
            // TODO(JRST-516): verify and extract the zip, delete staging files.
            Effect::Stage(_) | Effect::RemoveStaged { .. } | Effect::DeleteStaging(_) => {
                Task::none()
            }
            // TODO(JRST-524): the ready toast and the "Restart to update?" confirm.
            Effect::ShowReadyNotice { .. } | Effect::ConfirmRestart { .. } => Task::none(),
            // TODO(JRST-519): swap the Staged update in, relaunch, exit.
            Effect::SwapStaged { .. } | Effect::Relaunch | Effect::Exit => Task::none(),
        }
    }
}
