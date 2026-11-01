//! Pure hover-card state machine.
//!
//! Events go in, the machine returns the timer to schedule (if any) and
//! exposes the card that should currently be drawn. It never reads the clock:
//! the app turns a `StartTimer` effect into a delayed task and feeds
//! `Event::TimerFired(generation)` back. Every state change bumps the
//! generation, so a timer scheduled before the change is ignored when it
//! fires. Timing matches the web: 300ms to show, 150ms to hide.
//!
//! `A` identifies the anchor (issue reference, mention, row...) and decides
//! when a card is "the same card" and when a new anchor replaces it.

use std::time::Duration;

pub const SHOW_DELAY: Duration = Duration::from_millis(300);
pub const HIDE_DELAY: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, PartialEq)]
pub enum Event<A> {
    EnteredAnchor(A),
    LeftAnchor,
    EnteredCard,
    LeftCard,
    /// A timer from an earlier `Effect` elapsed.
    TimerFired(u64),
    Clicked,
    Scrolled,
    Escape,
    ViewChanged,
}

/// Ask the app to deliver `Event::TimerFired(generation)` after `delay`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Effect {
    pub generation: u64,
    pub delay: Duration,
}

#[derive(Debug, Clone, PartialEq)]
enum Phase<A> {
    Idle,
    /// Pointer is on an anchor; the card is not shown yet.
    Waiting(A),
    Open(A),
    /// Pointer left; the card stays visible until the hide timer fires.
    Closing(A),
}

#[derive(Debug, Clone, PartialEq)]
pub struct HoverCard<A> {
    phase: Phase<A>,
    generation: u64,
}

impl<A> Default for HoverCard<A> {
    fn default() -> Self {
        Self { phase: Phase::Idle, generation: 0 }
    }
}

impl<A: Clone + PartialEq> HoverCard<A> {
    pub fn new() -> Self {
        Self::default()
    }

    /// The anchor whose card should be drawn now, if any. At most one.
    pub fn current(&self) -> Option<&A> {
        match &self.phase {
            Phase::Open(a) | Phase::Closing(a) => Some(a),
            _ => None,
        }
    }

    pub fn update(&mut self, event: Event<A>) -> Option<Effect> {
        use Phase::*;
        match (event, &self.phase) {
            (Event::EnteredAnchor(a), Open(cur) | Closing(cur)) if *cur == a => {
                self.set(Open(a), None)
            }
            (Event::EnteredAnchor(a), Waiting(cur)) if *cur == a => None,
            (Event::EnteredAnchor(a), _) => self.set(Waiting(a), Some(SHOW_DELAY)),
            (Event::LeftAnchor, Waiting(_)) => self.set(Idle, None),
            (Event::LeftAnchor | Event::LeftCard, Open(a)) => {
                let a = a.clone();
                self.set(Closing(a), Some(HIDE_DELAY))
            }
            (Event::EnteredCard, Closing(a)) => {
                let a = a.clone();
                self.set(Open(a), None)
            }
            (Event::TimerFired(g), _) if g != self.generation => None,
            (Event::TimerFired(_), Waiting(a)) => {
                let a = a.clone();
                self.set(Open(a), None)
            }
            (Event::TimerFired(_), Closing(_)) => self.set(Idle, None),
            (Event::Clicked | Event::Scrolled | Event::Escape | Event::ViewChanged, Idle) => None,
            (Event::Clicked | Event::Scrolled | Event::Escape | Event::ViewChanged, _) => {
                self.set(Idle, None)
            }
            _ => None,
        }
    }

    fn set(&mut self, phase: Phase<A>, delay: Option<Duration>) -> Option<Effect> {
        self.phase = phase;
        self.generation += 1;
        delay.map(|delay| Effect { generation: self.generation, delay })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Event::*;

    type M = HoverCard<&'static str>;

    fn open(m: &mut M, a: &'static str) {
        let e = m.update(EnteredAnchor(a)).unwrap();
        m.update(TimerFired(e.generation));
        assert_eq!(m.current(), Some(&a));
    }

    #[test]
    fn shows_after_delay_not_before() {
        let mut m = M::new();
        let e = m.update(EnteredAnchor("a")).unwrap();
        assert_eq!(e.delay, Duration::from_millis(300));
        assert_eq!(m.current(), None);
        m.update(TimerFired(e.generation));
        assert_eq!(m.current(), Some(&"a"));
    }

    #[test]
    fn leaving_before_delay_cancels() {
        let mut m = M::new();
        let e = m.update(EnteredAnchor("a")).unwrap();
        assert_eq!(m.update(LeftAnchor), None);
        m.update(TimerFired(e.generation));
        assert_eq!(m.current(), None);
    }

    #[test]
    fn hides_after_delay() {
        let mut m = M::new();
        open(&mut m, "a");
        let e = m.update(LeftAnchor).unwrap();
        assert_eq!(e.delay, Duration::from_millis(150));
        assert_eq!(m.current(), Some(&"a"), "still visible during hide delay");
        m.update(TimerFired(e.generation));
        assert_eq!(m.current(), None);
    }

    #[test]
    fn moving_onto_card_keeps_it_open() {
        let mut m = M::new();
        open(&mut m, "a");
        let e = m.update(LeftAnchor).unwrap();
        m.update(EnteredCard);
        m.update(TimerFired(e.generation));
        assert_eq!(m.current(), Some(&"a"));
        let e = m.update(LeftCard).unwrap();
        m.update(TimerFired(e.generation));
        assert_eq!(m.current(), None);
    }

    #[test]
    fn returning_to_same_anchor_cancels_hide() {
        let mut m = M::new();
        open(&mut m, "a");
        let e = m.update(LeftAnchor).unwrap();
        assert_eq!(m.update(EnteredAnchor("a")), None);
        m.update(TimerFired(e.generation));
        assert_eq!(m.current(), Some(&"a"));
    }

    #[test]
    fn new_anchor_replaces_old_card() {
        let mut m = M::new();
        open(&mut m, "a");
        let e = m.update(EnteredAnchor("b")).unwrap();
        assert_eq!(e.delay, SHOW_DELAY);
        assert_eq!(m.current(), None, "old card gone, new one not yet shown");
        m.update(TimerFired(e.generation));
        assert_eq!(m.current(), Some(&"b"));
    }

    #[test]
    fn new_anchor_during_hide_delay_replaces_and_old_timer_is_stale() {
        let mut m = M::new();
        open(&mut m, "a");
        let hide = m.update(LeftAnchor).unwrap();
        let show = m.update(EnteredAnchor("b")).unwrap();
        m.update(TimerFired(hide.generation));
        assert_eq!(m.current(), None);
        m.update(TimerFired(show.generation));
        assert_eq!(m.current(), Some(&"b"));
    }

    #[test]
    fn stale_timers_do_nothing() {
        let mut m = M::new();
        let first = m.update(EnteredAnchor("a")).unwrap();
        m.update(LeftAnchor);
        let second = m.update(EnteredAnchor("a")).unwrap();
        m.update(TimerFired(first.generation));
        assert_eq!(m.current(), None);
        m.update(TimerFired(second.generation));
        assert_eq!(m.current(), Some(&"a"));
        m.update(TimerFired(second.generation + 99));
        assert_eq!(m.current(), Some(&"a"));
    }

    #[test]
    fn dismissing_events_close_card_and_cancel_timers() {
        for dismiss in [Clicked, Scrolled, Escape, ViewChanged] {
            let mut m = M::new();
            open(&mut m, "a");
            m.update(dismiss.clone());
            assert_eq!(m.current(), None, "{dismiss:?} while open");

            let mut m = M::new();
            open(&mut m, "a");
            m.update(LeftAnchor);
            m.update(dismiss.clone());
            assert_eq!(m.current(), None, "{dismiss:?} while closing");

            let mut m = M::new();
            let e = m.update(EnteredAnchor("a")).unwrap();
            m.update(dismiss.clone());
            m.update(TimerFired(e.generation));
            assert_eq!(m.current(), None, "{dismiss:?} while waiting");
        }
    }

    #[test]
    fn dismissing_when_idle_schedules_nothing() {
        let mut m = M::new();
        assert_eq!(m.update(Escape), None);
        assert_eq!(m.current(), None);
    }
}
