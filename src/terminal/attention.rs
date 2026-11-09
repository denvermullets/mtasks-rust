//! Which Terminals need the person, and when to post a banner about it.
//!
//! A program asks for attention with escape sequences the engine turns into
//! `Signal`s: a program status report (OSC 7501, what Claude Code sends when
//! it waits on a permission prompt or finishes), a desktop notification
//! (OSC 9 or 777), or a plain bell. `apply` folds one signal into what a
//! Terminal already needs. Plain logic, no UI: `TerminalState` owns the map
//! and decides who is watching.

use super::{BlockKind, ProgramState, Signal};

/// What a Terminal is waiting on the person for. Its badge shows until the
/// person looks at the Terminal or the program says it moved on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// The program can't go on without the person (a permission prompt, say).
    Blocked(BlockKind),
    /// The program failed and stopped.
    Error,
    /// The program finished and the result is ready to read.
    Done,
    /// The program asked for a desktop notification.
    Notified,
    /// The program rang the bell. Badge only, never a banner.
    Bell,
}

impl Need {
    /// A weaker need never hides a stronger one: a bell after a permission
    /// prompt leaves the prompt's badge.
    fn rank(self) -> u8 {
        match self {
            Need::Blocked(_) => 4,
            Need::Error => 3,
            Need::Done | Need::Notified => 2,
            Need::Bell => 1,
        }
    }

    /// Set by a program status report, which says itself when it is over.
    fn set_by_status(self) -> bool {
        matches!(self, Need::Blocked(_) | Need::Error | Need::Done)
    }

    /// The badge's urgency: blocked and error stand out from the rest.
    pub fn urgent(self) -> bool {
        matches!(self, Need::Blocked(_) | Need::Error)
    }
}

/// The text of a banner to post. `title` already names the Terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub title: String,
    pub body: String,
}

/// Fold `signal` into `current`, for the Terminal called `name`. `watching`:
/// the person can see this Terminal now (it is in a pane on screen and the
/// window has focus), so nothing is flagged and no banner is posted.
///
/// A banner goes out when a status first reaches blocked, done or error (a
/// repeat of the same report posts nothing), and for each desktop
/// notification unless a status already flagged the Terminal: a program that
/// sends both for one event gets one banner.
pub fn apply(
    current: Option<Need>,
    signal: &Signal,
    watching: bool,
    name: &str,
) -> (Option<Need>, Option<Alert>) {
    if watching {
        return (None, None);
    }
    match signal {
        Signal::Status {
            state,
            title,
            message,
            ..
        } => {
            let next = match state {
                ProgramState::Working | ProgramState::Clear => return (None, None),
                // At its prompt again: whatever it needed still stands.
                ProgramState::Idle => return (current, None),
                ProgramState::Blocked(kind) => Need::Blocked(*kind),
                ProgramState::Done => Need::Done,
                ProgramState::Error => Need::Error,
            };
            let alert = (current != Some(next)).then(|| Alert {
                title: titled(name, title),
                body: if message.trim().is_empty() {
                    default_body(next).to_string()
                } else {
                    message.clone()
                },
            });
            (Some(next), alert)
        }
        Signal::Notification { title, body } => {
            if current.is_some_and(Need::set_by_status) {
                return (current, None);
            }
            let next = stronger(current, Need::Notified);
            let alert = Alert {
                title: titled(name, title),
                body: if body.trim().is_empty() {
                    default_body(Need::Notified).to_string()
                } else {
                    body.clone()
                },
            };
            (Some(next), Some(alert))
        }
        Signal::Bell => (Some(stronger(current, Need::Bell)), None),
    }
}

/// The need to show for several Terminals at once (a Tab, a team's row).
pub fn strongest(needs: impl IntoIterator<Item = Need>) -> Option<Need> {
    needs.into_iter().max_by_key(|n| n.rank())
}

fn stronger(current: Option<Need>, new: Need) -> Need {
    match current {
        Some(c) if c.rank() > new.rank() => c,
        _ => new,
    }
}

/// "api · Fix the login bug", or just the Terminal's name.
fn titled(name: &str, title: &str) -> String {
    let title = title.trim();
    if title.is_empty() || title == name {
        name.to_string()
    } else {
        format!("{name} \u{b7} {title}")
    }
}

fn default_body(need: Need) -> &'static str {
    match need {
        Need::Blocked(BlockKind::Permission) => "Needs your permission",
        Need::Blocked(BlockKind::Question) => "Has a question for you",
        Need::Blocked(BlockKind::Auth) => "Needs you to sign in",
        Need::Blocked(BlockKind::Unspecified) => "Needs your input",
        Need::Error => "Stopped with an error",
        Need::Done => "Finished",
        Need::Notified | Need::Bell => "Wants your attention",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: ProgramState) -> Signal {
        Signal::Status {
            state,
            app: "claude-code".into(),
            title: String::new(),
            message: String::new(),
        }
    }

    fn notif(body: &str) -> Signal {
        Signal::Notification {
            title: String::new(),
            body: body.into(),
        }
    }

    const PERMISSION: ProgramState = ProgramState::Blocked(BlockKind::Permission);

    #[test]
    fn a_permission_prompt_flags_the_terminal_and_posts_one_banner() {
        let (need, alert) = apply(None, &status(PERMISSION), false, "api");
        assert_eq!(need, Some(Need::Blocked(BlockKind::Permission)));
        let alert = alert.expect("a banner");
        assert_eq!(alert.title, "api");
        assert_eq!(alert.body, "Needs your permission");
        // The same report again (a progress update, say) posts nothing more.
        let (again, alert) = apply(need, &status(PERMISSION), false, "api");
        assert_eq!(again, need);
        assert!(alert.is_none());
    }

    #[test]
    fn the_programs_title_and_message_go_in_the_banner() {
        let signal = Signal::Status {
            state: PERMISSION,
            app: "claude-code".into(),
            title: "Fix the login bug".into(),
            message: "Allow Bash: cargo test?".into(),
        };
        let (_, alert) = apply(None, &signal, false, "api");
        let alert = alert.unwrap();
        assert_eq!(alert.title, "api \u{b7} Fix the login bug");
        assert_eq!(alert.body, "Allow Bash: cargo test?");
    }

    #[test]
    fn done_flags_and_banners_too() {
        let blocked = Some(Need::Blocked(BlockKind::Permission));
        let (need, alert) = apply(blocked, &status(ProgramState::Done), false, "api");
        assert_eq!(need, Some(Need::Done));
        assert_eq!(alert.unwrap().body, "Finished");
    }

    #[test]
    fn working_again_or_clear_takes_the_flag_away() {
        for state in [ProgramState::Working, ProgramState::Clear] {
            let (need, alert) = apply(Some(Need::Done), &status(state), false, "api");
            assert_eq!((need, alert), (None, None));
        }
    }

    #[test]
    fn going_idle_keeps_what_the_terminal_needed() {
        let (need, alert) = apply(Some(Need::Done), &status(ProgramState::Idle), false, "api");
        assert_eq!((need, alert), (Some(Need::Done), None));
    }

    #[test]
    fn nothing_is_flagged_while_the_person_is_watching() {
        assert_eq!(apply(None, &status(PERMISSION), true, "api"), (None, None));
        assert_eq!(apply(None, &notif("hi"), true, "api"), (None, None));
        assert_eq!(apply(None, &Signal::Bell, true, "api"), (None, None));
    }

    #[test]
    fn a_bell_badges_without_a_banner_and_never_hides_a_prompt() {
        assert_eq!(
            apply(None, &Signal::Bell, false, "api"),
            (Some(Need::Bell), None)
        );
        let blocked = Some(Need::Blocked(BlockKind::Question));
        assert_eq!(apply(blocked, &Signal::Bell, false, "api"), (blocked, None));
    }

    #[test]
    fn a_desktop_notification_banners_each_time() {
        let (need, alert) = apply(None, &notif("Build finished"), false, "api");
        assert_eq!(need, Some(Need::Notified));
        assert_eq!(alert.unwrap().body, "Build finished");
        let (_, alert) = apply(need, &notif("Tests passed"), false, "api");
        assert_eq!(alert.unwrap().body, "Tests passed");
    }

    #[test]
    fn a_notification_for_what_a_status_already_flagged_posts_no_second_banner() {
        let blocked = Some(Need::Blocked(BlockKind::Permission));
        assert_eq!(
            apply(
                blocked,
                &notif("Claude needs your permission"),
                false,
                "api"
            ),
            (blocked, None)
        );
    }

    #[test]
    fn a_group_shows_its_most_pressing_need() {
        let needs = [Need::Bell, Need::Blocked(BlockKind::Auth), Need::Done];
        assert_eq!(strongest(needs), Some(Need::Blocked(BlockKind::Auth)));
        assert_eq!(strongest([]), None);
    }

    #[test]
    fn a_notification_after_a_bell_takes_over_the_badge() {
        let (need, _) = apply(Some(Need::Bell), &notif("x"), false, "api");
        assert_eq!(need, Some(Need::Notified));
    }
}
