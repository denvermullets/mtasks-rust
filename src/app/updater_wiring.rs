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
use crate::updater::edge;
use crate::updater::install::InstallContext;
use crate::updater::machine::{CheckKind, Effect, Event};

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
            // TODO(JRST-514): download the zip with progress and check its sha256.
            // TODO(JRST-514): abort the running download.
            Effect::Download(_) | Effect::CancelDownload => Task::none(),
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
