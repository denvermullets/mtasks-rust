//! Adaptive sync scheduler: pure decision logic, no Iced, no I/O.
//!
//! The caller owns a [`Scheduler`], feeds it [`Event`]s with the current time,
//! and acts on the returned [`Decision`]: run the named cycle (if any) and
//! arm a timer for `next_delay`. Nothing here sleeps or spawns.
//!
//! One scheduler owns two cadences: the Data sync ([`Cadence::Data`]) and the
//! Notification poll ([`Cadence::Notifications`]). Only one cycle of either
//! runs at a time; when both are due the Data sync goes first, and a cadence
//! that comes due while the other runs is deferred until `CycleFinished`.
//! Failure backoff and auth pause are shared by both cadences.
//!
//! The Notification cadence is opt-in: [`Scheduler::new`] schedules only the
//! Data sync (the driver predates the second cadence), and
//! [`Scheduler::with_notifications`] schedules both.

use std::time::{Duration, Instant};

/// Data sync, focused window with recent input.
pub const FOCUSED_INTERVAL: Duration = Duration::from_secs(60);
/// Data sync, window not focused.
pub const UNFOCUSED_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Data sync, no input for [`IDLE_AFTER`].
pub const IDLE_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// Notification poll, focused window with recent input.
pub const NOTIF_FOCUSED_INTERVAL: Duration = Duration::from_secs(15);
/// Notification poll, window not focused.
pub const NOTIF_UNFOCUSED_INTERVAL: Duration = Duration::from_secs(60);
/// Notification poll, no input for [`IDLE_AFTER`].
pub const NOTIF_IDLE_INTERVAL: Duration = Duration::from_secs(2 * 60);
/// Time without input before the app counts as idle.
pub const IDLE_AFTER: Duration = Duration::from_secs(10 * 60);
/// Ceiling for failure backoff (shared by both cadences).
pub const MAX_BACKOFF: Duration = Duration::from_secs(10 * 60);

/// Which cadence a decision belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cadence {
    Data,
    Notifications,
}

/// What a cycle does. `Full` = push then pull; `PushOnly` = drain the queue.
/// A Notification poll is always `Full` (the cadence says what that means).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleKind {
    Full,
    PushOnly,
}

impl CycleKind {
    fn merge(self, other: CycleKind) -> CycleKind {
        if self == CycleKind::Full || other == CycleKind::Full {
            CycleKind::Full
        } else {
            CycleKind::PushOnly
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Timer fired (or the caller polls); runs a cycle if one is due.
    Tick,
    FocusGained,
    FocusLost,
    /// Any keyboard or mouse input. Returning from idle runs both cadences.
    Input,
    /// The `s` shortcut: both cadences.
    ManualSync,
    /// A local write was queued: drains the Data queue only.
    LocalWrite,
    /// The running cycle (whichever cadence) ended.
    CycleFinished {
        success: bool,
    },
    /// Auth pause turned on or off (pauses both cadences). Turning it off
    /// runs both.
    AuthPaused(bool),
}

/// What the driver should start for an admitted cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Run {
    /// The Data sync (push/pull) of the given kind.
    Data(CycleKind),
    /// A Notification poll.
    Poll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    /// The cadence of `run`; with no `run`, the cadence due soonest.
    pub cadence: Cadence,
    /// Cycle to start right now, if any.
    pub run: Option<CycleKind>,
    /// How long until the next timed check (the soonest due cadence). `None`
    /// while a cycle is running or paused (a `CycleFinished` /
    /// `AuthPaused(false)` event re-arms it).
    pub next_delay: Option<Duration>,
}

impl Decision {
    /// The cycle to start now, dispatched on `cadence`; `None` = just re-arm.
    pub fn action(&self) -> Option<Run> {
        match (self.run, self.cadence) {
            (None, _) => None,
            (Some(_), Cadence::Notifications) => Some(Run::Poll),
            (Some(kind), Cadence::Data) => Some(Run::Data(kind)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Scheduler {
    /// When the last Data cycle ended (or the scheduler started).
    data_anchor: Instant,
    /// Same for the Notification poll.
    notif_anchor: Instant,
    /// Whether the Notification cadence is scheduled at all.
    notifications: bool,
    /// Consecutive failures since the last success, shared by both cadences.
    failures: u32,
    focused: bool,
    idle: bool,
    last_input: Instant,
    auth_paused: bool,
    /// The cadence of the cycle in flight.
    running: Option<Cadence>,
    /// Data work waiting for the slot (coalesced).
    data_pending: Option<CycleKind>,
    /// Notification poll waiting for the slot.
    notif_pending: bool,
}

/// What the current event asks for.
#[derive(Default)]
struct Want {
    data: Option<CycleKind>,
    notif: bool,
}

impl Want {
    fn all() -> Self {
        Self {
            data: Some(CycleKind::Full),
            notif: true,
        }
    }
}

impl Scheduler {
    /// New Data-only scheduler, focused and active. The first timed cycle is
    /// one focused interval after `now`; callers wanting a startup sync send
    /// `ManualSync`.
    pub fn new(now: Instant) -> Self {
        Self {
            data_anchor: now,
            notif_anchor: now,
            notifications: false,
            failures: 0,
            focused: true,
            idle: false,
            last_input: now,
            auth_paused: false,
            running: None,
            data_pending: None,
            notif_pending: false,
        }
    }

    /// Like [`Scheduler::new`] but also scheduling the Notification poll.
    pub fn with_notifications(now: Instant) -> Self {
        Self {
            notifications: true,
            ..Self::new(now)
        }
    }

    /// Interval for the current state, ignoring failures.
    fn base_interval(&self, cadence: Cadence) -> Duration {
        let (focused, unfocused, idle) = match cadence {
            Cadence::Data => (FOCUSED_INTERVAL, UNFOCUSED_INTERVAL, IDLE_INTERVAL),
            Cadence::Notifications => (
                NOTIF_FOCUSED_INTERVAL,
                NOTIF_UNFOCUSED_INTERVAL,
                NOTIF_IDLE_INTERVAL,
            ),
        };
        if self.idle {
            idle
        } else if !self.focused {
            unfocused
        } else {
            focused
        }
    }

    /// Delay between cycles of a cadence: base interval, doubled per (shared)
    /// failure, capped.
    pub fn delay_for(&self, cadence: Cadence) -> Duration {
        let base = self.base_interval(cadence);
        if self.failures == 0 {
            return base;
        }
        let factor = 1u32.checked_shl(self.failures).unwrap_or(u32::MAX);
        base.checked_mul(factor)
            .unwrap_or(MAX_BACKOFF)
            .min(MAX_BACKOFF)
    }

    /// Data sync delay (see [`Scheduler::delay_for`]).
    #[cfg(test)]
    pub fn current_delay(&self) -> Duration {
        self.delay_for(Cadence::Data)
    }

    fn anchor(&self, cadence: Cadence) -> Instant {
        match cadence {
            Cadence::Data => self.data_anchor,
            Cadence::Notifications => self.notif_anchor,
        }
    }

    fn due_at(&self, cadence: Cadence) -> Instant {
        self.anchor(cadence) + self.delay_for(cadence)
    }

    fn is_due(&self, cadence: Cadence, now: Instant) -> bool {
        now.saturating_duration_since(self.anchor(cadence)) >= self.delay_for(cadence)
    }

    pub fn handle(&mut self, event: Event, now: Instant) -> Decision {
        // Idle is derived from time since last input, checked on every event.
        if now.saturating_duration_since(self.last_input) >= IDLE_AFTER {
            self.idle = true;
        }

        let mut want = Want::default();
        match event {
            Event::Tick => {
                if !self.auth_paused {
                    if self.running != Some(Cadence::Data) && self.is_due(Cadence::Data, now) {
                        want.data = Some(CycleKind::Full);
                    }
                    if self.notifications
                        && self.running != Some(Cadence::Notifications)
                        && self.is_due(Cadence::Notifications, now)
                    {
                        want.notif = true;
                    }
                }
            }
            Event::FocusGained => {
                let was_unfocused = !self.focused;
                self.focused = true;
                self.touch(now);
                if was_unfocused {
                    want = Want::all();
                }
            }
            Event::FocusLost => self.focused = false,
            Event::Input => {
                if self.touch(now) {
                    want = Want::all();
                }
            }
            Event::ManualSync => {
                self.touch(now);
                want = Want::all();
            }
            Event::LocalWrite => {
                self.touch(now);
                want.data = Some(CycleKind::PushOnly);
            }
            Event::CycleFinished { success } => {
                let finished = self.running.take().unwrap_or(Cadence::Data);
                match finished {
                    Cadence::Data => self.data_anchor = now,
                    Cadence::Notifications => self.notif_anchor = now,
                }
                if success {
                    // Deferred work (Data first) is admitted below.
                    self.failures = 0;
                } else {
                    self.failures = self.failures.saturating_add(1);
                    // Back off both cadences together, and don't hammer a
                    // failing server: the backed-off timed cycles cover
                    // whatever was deferred.
                    self.data_anchor = now;
                    self.notif_anchor = now;
                    self.data_pending = None;
                    self.notif_pending = false;
                }
            }
            Event::AuthPaused(paused) => {
                self.auth_paused = paused;
                if paused {
                    self.data_pending = None;
                    self.notif_pending = false;
                } else {
                    want = Want::all();
                }
            }
        }

        let run = self.admit(want);
        let cadence = match run {
            Some((cadence, _)) => cadence,
            None => self.soonest(),
        };
        Decision {
            cadence,
            run: run.map(|(_, kind)| kind),
            next_delay: self.next_delay(now),
        }
    }

    /// Record input. Returns true when this ends an idle period.
    fn touch(&mut self, now: Instant) -> bool {
        let was_idle = self.idle;
        self.last_input = now;
        self.idle = false;
        was_idle
    }

    /// Apply the pause and one-at-a-time rules to the requested work, then
    /// start the next pending cycle if the slot is free. Data goes first.
    fn admit(&mut self, want: Want) -> Option<(Cadence, CycleKind)> {
        if self.auth_paused {
            return None;
        }
        if let Some(kind) = want.data {
            self.data_pending = Some(match self.data_pending {
                Some(existing) => existing.merge(kind),
                None => kind,
            });
        }
        if want.notif && self.notifications {
            self.notif_pending = true;
        }
        if self.running.is_some() {
            return None;
        }
        if let Some(kind) = self.data_pending.take() {
            self.running = Some(Cadence::Data);
            return Some((Cadence::Data, kind));
        }
        if self.notif_pending {
            self.notif_pending = false;
            self.running = Some(Cadence::Notifications);
            return Some((Cadence::Notifications, CycleKind::Full));
        }
        None
    }

    /// The cadence due soonest; Data on a tie or when Notifications is off.
    fn soonest(&self) -> Cadence {
        if self.notifications && self.due_at(Cadence::Notifications) < self.due_at(Cadence::Data) {
            Cadence::Notifications
        } else {
            Cadence::Data
        }
    }

    fn next_delay(&self, now: Instant) -> Option<Duration> {
        if self.running.is_some() || self.auth_paused {
            return None;
        }
        let due = self.due_at(self.soonest());
        Some(due.saturating_duration_since(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn start() -> (Scheduler, Instant) {
        let t0 = Instant::now();
        (Scheduler::new(t0), t0)
    }

    fn finish(s: &mut Scheduler, at: Instant, success: bool) -> Decision {
        s.handle(Event::CycleFinished { success }, at)
    }

    #[test]
    fn decision_action_dispatches_on_cadence() {
        let d = |cadence, run| Decision {
            cadence,
            run,
            next_delay: None,
        };
        assert_eq!(d(Cadence::Data, None).action(), None);
        assert_eq!(d(Cadence::Notifications, None).action(), None);
        assert_eq!(
            d(Cadence::Data, Some(CycleKind::PushOnly)).action(),
            Some(Run::Data(CycleKind::PushOnly))
        );
        assert_eq!(
            d(Cadence::Notifications, Some(CycleKind::Full)).action(),
            Some(Run::Poll)
        );
    }

    #[test]
    fn manual_sync_runs_data_then_poll_never_together() {
        let t0 = Instant::now();
        let mut s = Scheduler::with_notifications(t0);
        let d = s.handle(Event::ManualSync, t0);
        assert_eq!(d.action(), Some(Run::Data(CycleKind::Full)));
        assert_eq!(d.next_delay, None);
        let d = finish(&mut s, t0, true);
        assert_eq!(d.action(), Some(Run::Poll));
        let d = finish(&mut s, t0, true);
        assert_eq!(d.action(), None);
        assert!(d.next_delay.is_some());
    }

    #[test]
    fn focused_active_interval_is_60s() {
        let (mut s, t0) = start();
        let d = s.handle(Event::Tick, t0 + secs(59));
        assert_eq!(d.run, None);
        assert_eq!(d.next_delay, Some(secs(1)));
        let d = s.handle(Event::Tick, t0 + secs(60));
        assert_eq!(d.run, Some(CycleKind::Full));
    }

    #[test]
    fn unfocused_interval_is_5_minutes() {
        let (mut s, t0) = start();
        s.handle(Event::FocusLost, t0);
        assert_eq!(s.handle(Event::Tick, t0 + secs(299)).run, None);
        assert_eq!(
            s.handle(Event::Tick, t0 + secs(300)).run,
            Some(CycleKind::Full)
        );
    }

    #[test]
    fn idle_interval_is_10_minutes_after_10_minutes_without_input() {
        let (mut s, t0) = start();
        // Cycle just before idle starts: the next one is a full idle interval out.
        assert_eq!(
            s.handle(Event::Tick, t0 + secs(599)).run,
            Some(CycleKind::Full)
        );
        finish(&mut s, t0 + secs(599), true);
        // Idle at t0+600; due 599+600.
        let d = s.handle(Event::Tick, t0 + secs(600));
        assert_eq!(d.run, None);
        assert_eq!(d.next_delay, Some(secs(599)));
        assert_eq!(s.handle(Event::Tick, t0 + secs(1198)).run, None);
        assert_eq!(
            s.handle(Event::Tick, t0 + secs(1199)).run,
            Some(CycleKind::Full)
        );
    }

    #[test]
    fn focus_gained_runs_full_cycle_immediately() {
        let (mut s, t0) = start();
        s.handle(Event::FocusLost, t0);
        let d = s.handle(Event::FocusGained, t0 + secs(5));
        assert_eq!(d.run, Some(CycleKind::Full));
    }

    #[test]
    fn focus_gained_when_already_focused_does_nothing() {
        let (mut s, t0) = start();
        assert_eq!(s.handle(Event::FocusGained, t0 + secs(5)).run, None);
    }

    #[test]
    fn return_from_idle_runs_full_cycle() {
        let (mut s, t0) = start();
        let d = s.handle(Event::Input, t0 + IDLE_AFTER + secs(1));
        assert_eq!(d.run, Some(CycleKind::Full));
        // Ordinary input does not trigger.
        finish(&mut s, t0 + IDLE_AFTER + secs(2), true);
        assert_eq!(s.handle(Event::Input, t0 + IDLE_AFTER + secs(3)).run, None);
    }

    #[test]
    fn manual_sync_runs_full_cycle() {
        let (mut s, t0) = start();
        assert_eq!(
            s.handle(Event::ManualSync, t0 + secs(1)).run,
            Some(CycleKind::Full)
        );
    }

    #[test]
    fn local_write_runs_push_only_cycle() {
        let (mut s, t0) = start();
        assert_eq!(
            s.handle(Event::LocalWrite, t0 + secs(1)).run,
            Some(CycleKind::PushOnly)
        );
    }

    #[test]
    fn triggers_during_a_cycle_coalesce_into_one_follow_up() {
        let (mut s, t0) = start();
        assert!(s.handle(Event::ManualSync, t0).run.is_some());
        assert_eq!(s.handle(Event::LocalWrite, t0 + secs(1)).run, None);
        assert_eq!(s.handle(Event::LocalWrite, t0 + secs(2)).run, None);
        assert_eq!(s.handle(Event::Tick, t0 + secs(100)).run, None);
        let d = finish(&mut s, t0 + secs(3), true);
        assert_eq!(d.run, Some(CycleKind::PushOnly));
        // Only one follow-up.
        let d = finish(&mut s, t0 + secs(4), true);
        assert_eq!(d.run, None);
    }

    #[test]
    fn full_trigger_upgrades_a_push_only_follow_up() {
        let (mut s, t0) = start();
        s.handle(Event::ManualSync, t0);
        s.handle(Event::LocalWrite, t0 + secs(1));
        s.handle(Event::ManualSync, t0 + secs(2));
        s.handle(Event::LocalWrite, t0 + secs(3));
        assert_eq!(
            finish(&mut s, t0 + secs(4), true).run,
            Some(CycleKind::Full)
        );
    }

    #[test]
    fn no_cycle_runs_while_auth_paused() {
        let (mut s, t0) = start();
        s.handle(Event::AuthPaused(true), t0);
        assert_eq!(s.handle(Event::Tick, t0 + secs(600)).run, None);
        assert_eq!(s.handle(Event::ManualSync, t0 + secs(601)).run, None);
        assert_eq!(s.handle(Event::LocalWrite, t0 + secs(602)).run, None);
        let d = s.handle(Event::FocusLost, t0 + secs(603));
        assert_eq!(d.run, None);
        assert_eq!(d.next_delay, None);
        // Resuming runs a full cycle.
        assert_eq!(
            s.handle(Event::AuthPaused(false), t0 + secs(700)).run,
            Some(CycleKind::Full)
        );
    }

    #[test]
    fn auth_pause_discards_pending_follow_up() {
        let (mut s, t0) = start();
        s.handle(Event::ManualSync, t0);
        s.handle(Event::LocalWrite, t0 + secs(1));
        s.handle(Event::AuthPaused(true), t0 + secs(2));
        assert_eq!(finish(&mut s, t0 + secs(3), true).run, None);
    }

    #[test]
    fn failure_backoff_grows_exponentially_and_caps_at_10_minutes() {
        let (mut s, t0) = start();
        let mut delays = vec![];
        for _ in 0..6 {
            s.handle(Event::ManualSync, t0);
            delays.push(finish(&mut s, t0, false).next_delay.unwrap());
        }
        assert_eq!(
            delays,
            vec![
                secs(120),
                secs(240),
                secs(480),
                secs(600),
                secs(600),
                secs(600)
            ]
        );
    }

    #[test]
    fn backoff_scales_from_the_current_base_and_never_exceeds_cap() {
        let (mut s, t0) = start();
        s.handle(Event::FocusLost, t0);
        for _ in 0..80 {
            s.handle(Event::ManualSync, t0);
            assert_eq!(finish(&mut s, t0, false).next_delay, Some(MAX_BACKOFF));
        }
    }

    #[test]
    fn first_success_resets_backoff() {
        let (mut s, t0) = start();
        for _ in 0..3 {
            s.handle(Event::ManualSync, t0);
            finish(&mut s, t0, false);
        }
        assert_eq!(s.current_delay(), secs(480));
        s.handle(Event::ManualSync, t0);
        let d = finish(&mut s, t0, true);
        assert_eq!(d.next_delay, Some(FOCUSED_INTERVAL));
    }

    #[test]
    fn failed_cycle_drops_follow_up_instead_of_retrying_immediately() {
        let (mut s, t0) = start();
        s.handle(Event::ManualSync, t0);
        s.handle(Event::LocalWrite, t0 + secs(1));
        let d = finish(&mut s, t0 + secs(2), false);
        assert_eq!(d.run, None);
        assert_eq!(d.next_delay, Some(secs(120)));
    }

    #[test]
    fn no_timer_armed_while_a_cycle_is_running() {
        let (mut s, t0) = start();
        let d = s.handle(Event::ManualSync, t0);
        assert_eq!(d.next_delay, None);
        assert_eq!(d.cadence, Cadence::Data);
    }

    // ---- Notification cadence ----

    fn start_both() -> (Scheduler, Instant) {
        let t0 = Instant::now();
        (Scheduler::with_notifications(t0), t0)
    }

    /// Send `event`, then finish every cycle it admits (successfully) until
    /// none is left; returns the cycles in the order they ran.
    fn run_and_finish(s: &mut Scheduler, event: Event, at: Instant) -> Vec<(Cadence, CycleKind)> {
        let mut runs = vec![];
        let mut d = s.handle(event, at);
        while let Some(kind) = d.run {
            runs.push((d.cadence, kind));
            d = s.handle(Event::CycleFinished { success: true }, at);
        }
        runs
    }

    const DATA_FULL: (Cadence, CycleKind) = (Cadence::Data, CycleKind::Full);
    const DATA_PUSH: (Cadence, CycleKind) = (Cadence::Data, CycleKind::PushOnly);
    const POLL: (Cadence, CycleKind) = (Cadence::Notifications, CycleKind::Full);

    /// First tick at which a poll runs, scanning whole seconds.
    fn first_poll_second(s: &mut Scheduler, t0: Instant) -> u64 {
        (1..=1200)
            .find(|n| {
                let d = s.handle(Event::Tick, t0 + secs(*n));
                d.run.is_some() && d.cadence == Cadence::Notifications
            })
            .expect("poll never ran")
    }

    #[test]
    fn default_scheduler_never_polls_notifications() {
        let (mut s, t0) = start();
        assert_eq!(s.handle(Event::ManualSync, t0).run, Some(CycleKind::Full));
        assert_eq!(finish(&mut s, t0, true).run, None);
        assert_eq!(s.handle(Event::Tick, t0 + secs(30)).run, None);
    }

    #[test]
    fn poll_interval_is_15s_when_focused() {
        let (mut s, t0) = start_both();
        let d = s.handle(Event::Tick, t0 + secs(14));
        assert_eq!((d.run, d.cadence), (None, Cadence::Notifications));
        assert_eq!(d.next_delay, Some(secs(1)));
        assert_eq!(first_poll_second(&mut s, t0), 15);
    }

    #[test]
    fn poll_interval_is_60s_when_unfocused() {
        let (mut s, t0) = start_both();
        s.handle(Event::FocusLost, t0);
        let d = s.handle(Event::Tick, t0 + secs(59));
        assert_eq!((d.run, d.cadence), (None, Cadence::Notifications));
        assert_eq!(d.next_delay, Some(secs(1)));
        assert_eq!(first_poll_second(&mut s, t0), 60);
    }

    #[test]
    fn poll_interval_is_2_minutes_when_idle() {
        let (mut s, t0) = start_both();
        // Keep the app alive with pre-idle polls, then let it go idle.
        run_and_finish(&mut s, Event::Tick, t0 + secs(599)); // idle starts at t0+600
        let d = s.handle(Event::Tick, t0 + secs(600));
        assert_eq!(d.run, None);
        assert_eq!(d.next_delay, Some(secs(119)));
        assert_eq!(s.handle(Event::Tick, t0 + secs(718)).run, None);
        assert_eq!(
            s.handle(Event::Tick, t0 + secs(719)).cadence,
            Cadence::Notifications
        );
    }

    #[test]
    fn data_intervals_are_unchanged_with_notifications_on() {
        let (mut s, t0) = start_both();
        for n in [15, 30, 45] {
            assert_eq!(run_and_finish(&mut s, Event::Tick, t0 + secs(n)), vec![POLL]);
        }
        // Data still waits for its own 60s, and goes before the poll due then.
        assert!(run_and_finish(&mut s, Event::Tick, t0 + secs(59)).is_empty());
        assert_eq!(
            run_and_finish(&mut s, Event::Tick, t0 + secs(60)),
            vec![DATA_FULL, POLL]
        );
    }

    #[test]
    fn data_goes_first_when_both_are_due() {
        let (mut s, t0) = start_both();
        assert_eq!(
            run_and_finish(&mut s, Event::Tick, t0 + secs(60)),
            vec![DATA_FULL, POLL]
        );
    }

    #[test]
    fn focus_return_from_idle_and_manual_sync_run_data_then_poll() {
        let (mut s, t0) = start_both();
        s.handle(Event::FocusLost, t0);
        assert_eq!(
            run_and_finish(&mut s, Event::FocusGained, t0 + secs(5)),
            vec![DATA_FULL, POLL]
        );
        assert_eq!(
            run_and_finish(&mut s, Event::Input, t0 + IDLE_AFTER + secs(10)),
            vec![DATA_FULL, POLL]
        );
        assert_eq!(
            run_and_finish(&mut s, Event::ManualSync, t0 + IDLE_AFTER + secs(11)),
            vec![DATA_FULL, POLL]
        );
    }

    #[test]
    fn local_write_fires_only_data() {
        let (mut s, t0) = start_both();
        assert_eq!(
            run_and_finish(&mut s, Event::LocalWrite, t0 + secs(1)),
            vec![DATA_PUSH]
        );
    }

    #[test]
    fn cycles_never_overlap_and_a_deferred_cadence_runs_after_finish() {
        let (mut s, t0) = start_both();
        let d = s.handle(Event::ManualSync, t0 + secs(1));
        assert_eq!((d.cadence, d.run), (Cadence::Data, Some(CycleKind::Full)));
        assert_eq!(d.next_delay, None);
        // Nothing else starts while Data runs; the poll is deferred, not dropped.
        assert_eq!(s.handle(Event::Tick, t0 + secs(100)).run, None);
        let d = finish(&mut s, t0 + secs(102), true);
        assert_eq!((d.cadence, d.run), (Cadence::Notifications, Some(CycleKind::Full)));
        // A local write during the poll waits for it.
        assert_eq!(s.handle(Event::LocalWrite, t0 + secs(103)).run, None);
        let d = finish(&mut s, t0 + secs(104), true);
        assert_eq!((d.cadence, d.run), (Cadence::Data, Some(CycleKind::PushOnly)));
        assert_eq!(finish(&mut s, t0 + secs(105), true).run, None);
    }

    #[test]
    fn data_due_during_a_poll_is_deferred_until_it_finishes() {
        let (mut s, t0) = start_both();
        let d = s.handle(Event::Tick, t0 + secs(15));
        assert_eq!((d.cadence, d.run), (Cadence::Notifications, Some(CycleKind::Full)));
        // Data comes due at 60s while the poll is still running.
        assert_eq!(s.handle(Event::Tick, t0 + secs(60)).run, None);
        let d = finish(&mut s, t0 + secs(61), true);
        assert_eq!((d.cadence, d.run), (Cadence::Data, Some(CycleKind::Full)));
    }

    #[test]
    fn a_failed_poll_backs_off_both_cadences() {
        let (mut s, t0) = start_both();
        s.handle(Event::Tick, t0 + secs(15));
        let d = finish(&mut s, t0 + secs(15), false);
        assert_eq!(s.delay_for(Cadence::Data), secs(120));
        assert_eq!(s.delay_for(Cadence::Notifications), secs(30));
        assert_eq!((d.cadence, d.next_delay), (Cadence::Notifications, Some(secs(30))));
    }

    #[test]
    fn a_failed_data_cycle_backs_off_the_poll_too() {
        let (mut s, t0) = start_both();
        s.handle(Event::ManualSync, t0 + secs(60));
        finish(&mut s, t0 + secs(60), false);
        assert_eq!(s.delay_for(Cadence::Notifications), secs(30));
        assert_eq!(s.handle(Event::Tick, t0 + secs(89)).run, None);
        assert_eq!(
            s.handle(Event::Tick, t0 + secs(90)).cadence,
            Cadence::Notifications
        );
    }

    #[test]
    fn shared_backoff_caps_at_10_minutes_and_success_clears_it() {
        let (mut s, t0) = start_both();
        for _ in 0..80 {
            s.handle(Event::ManualSync, t0);
            finish(&mut s, t0, false);
            assert!(s.delay_for(Cadence::Data) <= MAX_BACKOFF);
            assert!(s.delay_for(Cadence::Notifications) <= MAX_BACKOFF);
        }
        assert_eq!(s.delay_for(Cadence::Data), MAX_BACKOFF);
        assert_eq!(s.delay_for(Cadence::Notifications), MAX_BACKOFF);
        s.handle(Event::ManualSync, t0);
        finish(&mut s, t0, true);
        assert_eq!(s.delay_for(Cadence::Data), FOCUSED_INTERVAL);
        assert_eq!(s.delay_for(Cadence::Notifications), NOTIF_FOCUSED_INTERVAL);
    }

    #[test]
    fn failure_drops_the_deferred_poll() {
        let (mut s, t0) = start_both();
        s.handle(Event::ManualSync, t0);
        assert_eq!(finish(&mut s, t0 + secs(1), false).run, None);
    }

    #[test]
    fn auth_pause_stops_both_cadences_and_resuming_runs_both() {
        let (mut s, t0) = start_both();
        s.handle(Event::AuthPaused(true), t0);
        assert_eq!(s.handle(Event::Tick, t0 + secs(600)).run, None);
        assert_eq!(s.handle(Event::ManualSync, t0 + secs(601)).run, None);
        let d = s.handle(Event::FocusLost, t0 + secs(602));
        assert_eq!((d.run, d.next_delay), (None, None));
        assert_eq!(
            run_and_finish(&mut s, Event::AuthPaused(false), t0 + secs(700)),
            vec![DATA_FULL, POLL]
        );
    }

    #[test]
    fn auth_pause_discards_a_deferred_poll() {
        let (mut s, t0) = start_both();
        s.handle(Event::ManualSync, t0);
        s.handle(Event::AuthPaused(true), t0 + secs(1));
        assert_eq!(finish(&mut s, t0 + secs(2), true).run, None);
    }

    #[test]
    fn next_delay_points_at_the_soonest_cadence() {
        let (mut s, t0) = start_both();
        let d = s.handle(Event::Tick, t0 + secs(5));
        assert_eq!((d.cadence, d.next_delay), (Cadence::Notifications, Some(secs(10))));
        // Right after a poll at 15s the next poll (30s) still beats Data (60s).
        s.handle(Event::Tick, t0 + secs(15));
        let d = finish(&mut s, t0 + secs(15), true);
        assert_eq!((d.cadence, d.next_delay), (Cadence::Notifications, Some(secs(15))));
        // Unfocused: the 60s poll still beats the 5 min Data sync.
        let d = s.handle(Event::FocusLost, t0 + secs(16));
        assert_eq!((d.cadence, d.next_delay), (Cadence::Notifications, Some(secs(59))));
    }
}
