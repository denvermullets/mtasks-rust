//! Adaptive Data sync scheduler: pure decision logic, no Iced, no I/O.
//!
//! The caller owns a [`Scheduler`], feeds it [`Event`]s with the current time,
//! and acts on the returned [`Decision`]: run the named cycle (if any) and
//! arm a timer for `next_delay`. Nothing here sleeps or spawns.
//!
//! A second cadence (the Notification poll) slots in later as another
//! [`Cadence`] value alongside `Cadence::Data`; the per-cadence timing lives in
//! [`CadenceTimer`], so a new field on `Scheduler` is all it needs.

use std::time::{Duration, Instant};

/// Focused window with recent input.
pub const FOCUSED_INTERVAL: Duration = Duration::from_secs(60);
/// Window not focused.
pub const UNFOCUSED_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// No input for [`IDLE_AFTER`].
pub const IDLE_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// Time without input before the app counts as idle.
pub const IDLE_AFTER: Duration = Duration::from_secs(10 * 60);
/// Ceiling for failure backoff.
pub const MAX_BACKOFF: Duration = Duration::from_secs(10 * 60);

/// Which cadence a decision belongs to. Only the Data sync exists today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cadence {
    Data,
}

/// What a cycle does. `Full` = push then pull; `PushOnly` = drain the queue.
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
    /// Any keyboard or mouse input. Returning from idle runs a full cycle.
    Input,
    /// The `s` shortcut.
    ManualSync,
    /// A local write was queued.
    #[allow(dead_code)] // sent by the local-write trigger (JRST-28) via App::drive_sync
    LocalWrite,
    /// The running cycle ended.
    CycleFinished {
        success: bool,
    },
    /// Auth pause turned on or off. Turning it off runs a full cycle.
    AuthPaused(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub cadence: Cadence,
    /// Cycle to start right now, if any.
    pub run: Option<CycleKind>,
    /// How long until the next timed check. `None` while a cycle is running
    /// or paused (a `CycleFinished`/`AuthPaused(false)` event re-arms it).
    pub next_delay: Option<Duration>,
}

/// Timing state for one cadence.
#[derive(Debug, Clone)]
struct CadenceTimer {
    /// When the last cycle ended (or the scheduler started).
    anchor: Instant,
    /// Consecutive failures since the last success.
    failures: u32,
}

#[derive(Debug, Clone)]
pub struct Scheduler {
    data: CadenceTimer,
    focused: bool,
    idle: bool,
    last_input: Instant,
    auth_paused: bool,
    in_flight: bool,
    follow_up: Option<CycleKind>,
}

impl Scheduler {
    /// New scheduler, focused and active. The first timed cycle is one
    /// focused interval after `now`; callers wanting a startup sync send
    /// `ManualSync`.
    pub fn new(now: Instant) -> Self {
        Self {
            data: CadenceTimer {
                anchor: now,
                failures: 0,
            },
            focused: true,
            idle: false,
            last_input: now,
            auth_paused: false,
            in_flight: false,
            follow_up: None,
        }
    }

    /// Interval for the current state, ignoring failures.
    fn base_interval(&self) -> Duration {
        if self.idle {
            IDLE_INTERVAL
        } else if !self.focused {
            UNFOCUSED_INTERVAL
        } else {
            FOCUSED_INTERVAL
        }
    }

    /// Delay between cycles: base interval, doubled per failure, capped.
    pub fn current_delay(&self) -> Duration {
        let base = self.base_interval();
        if self.data.failures == 0 {
            return base;
        }
        let factor = 1u32.checked_shl(self.data.failures).unwrap_or(u32::MAX);
        base.checked_mul(factor)
            .unwrap_or(MAX_BACKOFF)
            .min(MAX_BACKOFF)
    }

    pub fn handle(&mut self, event: Event, now: Instant) -> Decision {
        // Idle is derived from time since last input, checked on every event.
        if now.saturating_duration_since(self.last_input) >= IDLE_AFTER {
            self.idle = true;
        }

        let mut trigger: Option<CycleKind> = None;
        match event {
            Event::Tick => {
                if !self.in_flight
                    && !self.auth_paused
                    && now.saturating_duration_since(self.data.anchor) >= self.current_delay()
                {
                    trigger = Some(CycleKind::Full);
                }
            }
            Event::FocusGained => {
                let was_unfocused = !self.focused;
                self.focused = true;
                self.touch(now);
                if was_unfocused {
                    trigger = Some(CycleKind::Full);
                }
            }
            Event::FocusLost => self.focused = false,
            Event::Input => {
                if self.touch(now) {
                    trigger = Some(CycleKind::Full);
                }
            }
            Event::ManualSync => {
                self.touch(now);
                trigger = Some(CycleKind::Full);
            }
            Event::LocalWrite => {
                self.touch(now);
                trigger = Some(CycleKind::PushOnly);
            }
            Event::CycleFinished { success } => {
                self.in_flight = false;
                self.data.anchor = now;
                if success {
                    self.data.failures = 0;
                    // Coalesced triggers run as one follow-up.
                    trigger = self.follow_up.take();
                } else {
                    self.data.failures = self.data.failures.saturating_add(1);
                    // Don't hammer a failing server: the backed-off timed
                    // Full cycle covers whatever the follow-up wanted.
                    self.follow_up = None;
                }
            }
            Event::AuthPaused(paused) => {
                self.auth_paused = paused;
                if paused {
                    self.follow_up = None;
                } else {
                    trigger = Some(CycleKind::Full);
                }
            }
        }

        let run = self.admit(trigger);
        Decision {
            cadence: Cadence::Data,
            run,
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

    /// Apply the pause and in-flight rules to a requested cycle.
    fn admit(&mut self, trigger: Option<CycleKind>) -> Option<CycleKind> {
        let kind = trigger?;
        if self.auth_paused {
            return None;
        }
        if self.in_flight {
            self.follow_up = Some(match self.follow_up {
                Some(existing) => existing.merge(kind),
                None => kind,
            });
            return None;
        }
        self.in_flight = true;
        Some(kind)
    }

    fn next_delay(&self, now: Instant) -> Option<Duration> {
        if self.in_flight || self.auth_paused {
            return None;
        }
        let due = self.data.anchor + self.current_delay();
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
}
