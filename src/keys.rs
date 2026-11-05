use iced::keyboard::{self, Key, Modifiers};

use crate::app::{Message, TabMessage, View};

pub fn handle_key_press(
    key: Key,
    modifiers: Modifiers,
    modal_open: bool,
    palette_open: bool,
    on_board: bool,
) -> Option<Message> {
    if modal_open {
        return match key {
            Key::Named(keyboard::key::Named::Escape) => Some(Message::CloseModal),
            Key::Named(keyboard::key::Named::ArrowDown) if palette_open => {
                Some(Message::PaletteMove(1))
            }
            Key::Named(keyboard::key::Named::ArrowUp) if palette_open => {
                Some(Message::PaletteMove(-1))
            }
            _ => None,
        };
    }

    match key {
        Key::Character(ref c) => match c.as_str() {
            // Global shortcuts — active on any view.
            "f" if modifiers.command() => Some(Message::FocusSearch),
            "k" if modifiers.command() => Some(Message::OpenCommandPalette),
            "j" if modifiers.command() => Some(Message::Terminal(
                crate::terminal::TerminalMessage::ToggleFocus,
            )),
            "L" | "l" if modifiers.shift() => Some(Message::OpenLabels),
            "I" | "i" if modifiers.shift() => Some(Message::NavigateToView(View::Inbox)),
            "p" if modifiers.is_empty() => Some(Message::CreateProject),
            "s" if modifiers.is_empty() => Some(Message::SyncNow),
            "," if modifiers.is_empty() => Some(Message::OpenSettings),
            "?" | "/" if modifiers.shift() || modifiers.is_empty() => Some(Message::ToggleHelp),

            // Cmd+1-9 focuses the Nth Tab (a missing one does nothing; the
            // modal case already returned above).
            d if modifiers.command() && tab_position(d).is_some() => {
                tab_position(d).map(|i| Message::Tab(TabMessage::Focus(i)))
            }

            // Board-only shortcuts — ignored on other views.
            "v" if on_board && modifiers.is_empty() => Some(Message::ToggleViewMode),
            "c" if on_board && modifiers.is_empty() => Some(Message::ToggleFocusedSection),
            "n" if on_board && modifiers.is_empty() => Some(Message::CreateIssue),
            "e" if on_board && modifiers.is_empty() => Some(Message::EditFocusedIssue),
            "d" if on_board && modifiers.is_empty() => Some(Message::ConfirmDeleteFocusedIssue),
            "j" if on_board && modifiers.is_empty() => Some(Message::FocusNextIssue),
            "k" if on_board && modifiers.is_empty() => Some(Message::FocusPrevIssue),
            "h" if on_board && modifiers.is_empty() => Some(Message::FocusPrevLane),
            "l" if on_board && modifiers.is_empty() => Some(Message::FocusNextLane),
            "m" if on_board && modifiers.is_empty() => Some(Message::MoveIssueNextLane),
            "M" | "m" if on_board && modifiers.shift() => Some(Message::MoveIssuePrevLane),
            "t" if on_board && modifiers.is_empty() => Some(Message::MoveFocusedIssueToTeam),
            "1" if on_board && modifiers.is_empty() => Some(Message::SetFocusedPriority(0)),
            "2" if on_board && modifiers.is_empty() => Some(Message::SetFocusedPriority(1)),
            "3" if on_board && modifiers.is_empty() => Some(Message::SetFocusedPriority(2)),
            "4" if on_board && modifiers.is_empty() => Some(Message::SetFocusedPriority(3)),
            _ => None,
        },
        Key::Named(named) => match named {
            keyboard::key::Named::Escape => Some(Message::CloseModal),
            keyboard::key::Named::Enter if on_board => Some(Message::EditFocusedIssue),
            keyboard::key::Named::ArrowDown if on_board => Some(Message::FocusNextIssue),
            keyboard::key::Named::ArrowUp if on_board => Some(Message::FocusPrevIssue),
            keyboard::key::Named::ArrowLeft if on_board => Some(Message::FocusPrevLane),
            keyboard::key::Named::ArrowRight if on_board => Some(Message::FocusNextLane),
            _ => None,
        },
        _ => None,
    }
}

/// "1".."9" as a zero-based Tab position.
fn tab_position(c: &str) -> Option<usize> {
    let n: usize = c.parse().ok()?;
    (1..=9).contains(&n).then(|| n - 1)
}

/// Keys bound only while a dashboard is showing: j/k and the arrows walk rows
/// across groups, h/l jump between groups, Enter opens the focused issue.
pub fn handle_dashboard_key(key: &Key, modifiers: Modifiers) -> Option<Message> {
    if !modifiers.is_empty() {
        return None;
    }
    match key {
        Key::Character(c) => match c.as_str() {
            "j" => Some(Message::FocusNextIssue),
            "k" => Some(Message::FocusPrevIssue),
            "h" => Some(Message::FocusPrevLane),
            "l" => Some(Message::FocusNextLane),
            _ => None,
        },
        Key::Named(keyboard::key::Named::Enter) => Some(Message::EditFocusedIssue),
        Key::Named(keyboard::key::Named::ArrowDown) => Some(Message::FocusNextIssue),
        Key::Named(keyboard::key::Named::ArrowUp) => Some(Message::FocusPrevIssue),
        Key::Named(keyboard::key::Named::ArrowLeft) => Some(Message::FocusPrevLane),
        Key::Named(keyboard::key::Named::ArrowRight) => Some(Message::FocusNextLane),
        _ => None,
    }
}

/// Keys bound only while the Inbox is showing. Checked before the global
/// handler; `None` falls through to it.
pub fn handle_inbox_key(key: &Key, modifiers: Modifiers) -> Option<Message> {
    if modifiers.shift() && !modifiers.command() && !modifiers.control() && !modifiers.alt() {
        if let Key::Character(c) = key {
            if c.as_str() == "R" || c.as_str() == "r" {
                return Some(Message::InboxMarkAllRead);
            }
        }
        return None;
    }
    if !modifiers.is_empty() {
        return None;
    }
    match key {
        Key::Character(c) => match c.as_str() {
            "j" => Some(Message::InboxMove(1)),
            "k" => Some(Message::InboxMove(-1)),
            "r" => Some(Message::InboxMarkRead),
            "f" => Some(Message::InboxToggleFilter),
            _ => None,
        },
        Key::Named(keyboard::key::Named::ArrowDown) => Some(Message::InboxMove(1)),
        Key::Named(keyboard::key::Named::ArrowUp) => Some(Message::InboxMove(-1)),
        Key::Named(keyboard::key::Named::Enter) => Some(Message::InboxOpen),
        _ => None,
    }
}

/// The description keys both detail views share: Escape leaves the editor
/// while editing (once rendered it falls through to the global handler), and
/// plain `e` starts editing.
fn description_key(
    key: &Key,
    modifiers: Modifiers,
    editing_description: bool,
    stop: Message,
    edit: Message,
) -> Option<Message> {
    match key {
        Key::Named(keyboard::key::Named::Escape) if editing_description => Some(stop),
        Key::Character(c) if modifiers.is_empty() && c.as_str() == "e" => Some(edit),
        _ => None,
    }
}

/// Keys bound only while an issue detail is showing. Checked before the
/// global handler; `None` falls through to it.
pub fn handle_issue_detail_key(
    key: &Key,
    modifiers: Modifiers,
    editing_description: bool,
) -> Option<Message> {
    description_key(
        key,
        modifiers,
        editing_description,
        Message::StopEditingIssueDescription,
        Message::EditIssueDescription,
    )
    .or_else(|| match key {
        Key::Character(c) if modifiers.is_empty() && c.as_str() == "a" => {
            Some(Message::PickAttachmentFiles)
        }
        _ => None,
    })
}

/// Keys bound only while a project detail is showing: the same description keys
/// as the issue detail handler, plus `/` to toggle the properties sidebar. It
/// only sees keys no text field or Terminal took.
pub fn handle_project_detail_key(
    key: &Key,
    modifiers: Modifiers,
    editing_description: bool,
) -> Option<Message> {
    description_key(
        key,
        modifiers,
        editing_description,
        Message::StopEditingProjectDescription,
        Message::EditProjectDescription,
    )
    .or_else(|| match key {
        Key::Character(c) if modifiers.is_empty() && c.as_str() == "/" => {
            Some(Message::ToggleProjectProperties)
        }
        _ => None,
    })
}

/// Keys bound only while the lightbox modal is open. Checked before the
/// modal fallthrough in `handle_key_press`; `None` falls through to it
/// (Esc closes). ←/→ step through the issue's images.
pub fn handle_lightbox_key(key: &Key, modifiers: Modifiers) -> Option<Message> {
    match key {
        Key::Character(c) if modifiers.is_empty() && c.as_str() == "o" => {
            Some(Message::OpenLightboxExternally)
        }
        Key::Named(keyboard::key::Named::ArrowRight) if modifiers.is_empty() => Some(Message::LightboxStep(1)),
        Key::Named(keyboard::key::Named::ArrowLeft) if modifiers.is_empty() => Some(Message::LightboxStep(-1)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn o_opens_lightbox_image_in_preview() {
        let o = Key::Character("o".into());
        assert!(matches!(
            handle_lightbox_key(&o, Modifiers::empty()),
            Some(Message::OpenLightboxExternally)
        ));
        assert!(handle_lightbox_key(&o, Modifiers::COMMAND).is_none());
        assert!(handle_lightbox_key(&Key::Character("x".into()), Modifiers::empty()).is_none());
        // Inert in the global handler (no lightbox open).
        assert!(press("o", false, true).is_none());
    }

    #[test]
    fn arrows_step_the_lightbox() {
        let right = Key::Named(keyboard::key::Named::ArrowRight);
        let left = Key::Named(keyboard::key::Named::ArrowLeft);
        assert!(matches!(
            handle_lightbox_key(&right, Modifiers::empty()),
            Some(Message::LightboxStep(1))
        ));
        assert!(matches!(
            handle_lightbox_key(&left, Modifiers::empty()),
            Some(Message::LightboxStep(-1))
        ));
        assert!(handle_lightbox_key(&right, Modifiers::SHIFT).is_none());
    }

    #[test]
    fn a_attaches_a_file_in_issue_detail_only() {
        let a = Key::Character("a".into());
        assert!(matches!(
            handle_issue_detail_key(&a, Modifiers::empty(), false),
            Some(Message::PickAttachmentFiles)
        ));
        assert!(handle_issue_detail_key(&a, Modifiers::COMMAND, false).is_none());
        assert!(handle_issue_detail_key(&Key::Character("b".into()), Modifiers::empty(), false).is_none());
        // The global handler never binds it, so it's inert off the detail view.
        assert!(press("a", false, true).is_none());
        assert!(press("a", false, false).is_none());
    }

    #[test]
    fn e_edits_the_description_in_issue_detail_only() {
        let e = Key::Character("e".into());
        assert!(matches!(
            handle_issue_detail_key(&e, Modifiers::empty(), false),
            Some(Message::EditIssueDescription)
        ));
        assert!(handle_issue_detail_key(&e, Modifiers::COMMAND, false).is_none());
        assert!(handle_issue_detail_key(&e, Modifiers::SHIFT, false).is_none());
        // The board keeps its own `e`; elsewhere the global handler is inert.
        assert!(matches!(press("e", false, true), Some(Message::EditFocusedIssue)));
        assert!(press("e", false, false).is_none());
    }

    #[test]
    fn escape_leaves_the_description_editor_only_while_editing() {
        let esc = Key::Named(keyboard::key::Named::Escape);
        assert!(matches!(
            handle_issue_detail_key(&esc, Modifiers::empty(), true),
            Some(Message::StopEditingIssueDescription)
        ));
        // Rendered: Escape falls through to today's behavior.
        assert!(handle_issue_detail_key(&esc, Modifiers::empty(), false).is_none());
    }

    #[test]
    fn project_escape_leaves_the_description_editor_only_while_editing() {
        let esc = Key::Named(keyboard::key::Named::Escape);
        assert!(matches!(
            handle_project_detail_key(&esc, Modifiers::empty(), true),
            Some(Message::StopEditingProjectDescription)
        ));
        assert!(handle_project_detail_key(&esc, Modifiers::empty(), false).is_none());
    }

    #[test]
    fn slash_toggles_properties_on_the_project_page_and_opens_help_elsewhere() {
        let slash = Key::Character("/".into());
        assert!(matches!(
            handle_project_detail_key(&slash, Modifiers::empty(), false),
            Some(Message::ToggleProjectProperties)
        ));
        // `?` is shift+/ and stays help: the project handler leaves it alone.
        let q = Key::Character("?".into());
        assert!(handle_project_detail_key(&q, Modifiers::SHIFT, false).is_none());
        assert!(matches!(press_with("?", Modifiers::SHIFT, false), Some(Message::ToggleHelp)));
        // With a command modifier it is not the shortcut.
        assert!(handle_project_detail_key(&slash, Modifiers::COMMAND, false).is_none());
        // Off the project page the global handler still opens help on `/`.
        assert!(matches!(press("/", false, false), Some(Message::ToggleHelp)));
    }

    #[test]
    fn e_edits_the_project_description_with_no_modifiers() {
        let e = Key::Character("e".into());
        assert!(matches!(
            handle_project_detail_key(&e, Modifiers::empty(), false),
            Some(Message::EditProjectDescription)
        ));
        assert!(handle_project_detail_key(&e, Modifiers::COMMAND, false).is_none());
        assert!(handle_project_detail_key(&e, Modifiers::SHIFT, false).is_none());
    }

    #[test]
    fn inbox_jk_move_selection() {
        let j = Key::Character("j".into());
        let k = Key::Character("k".into());
        assert!(matches!(
            handle_inbox_key(&j, Modifiers::empty()),
            Some(Message::InboxMove(1))
        ));
        assert!(matches!(
            handle_inbox_key(&k, Modifiers::empty()),
            Some(Message::InboxMove(-1))
        ));
        assert!(handle_inbox_key(&k, Modifiers::COMMAND).is_none());
    }

    #[test]
    fn inbox_r_marks_read_only_in_inbox_handler() {
        let r = Key::Character("r".into());
        assert!(matches!(
            handle_inbox_key(&r, Modifiers::empty()),
            Some(Message::InboxMarkRead)
        ));
        assert!(!matches!(press("r", false, false), Some(Message::InboxMarkRead)));
    }

    #[test]
    fn inbox_enter_opens_the_issue() {
        let enter = Key::Named(keyboard::key::Named::Enter);
        assert!(matches!(
            handle_inbox_key(&enter, Modifiers::empty()),
            Some(Message::InboxOpen)
        ));
    }

    #[test]
    fn inbox_shift_r_marks_all_read() {
        let big = Key::Character("R".into());
        assert!(matches!(
            handle_inbox_key(&big, Modifiers::SHIFT),
            Some(Message::InboxMarkAllRead)
        ));
        assert!(!matches!(press("R", false, false), Some(Message::InboxMarkAllRead)));
    }

    #[test]
    fn inbox_f_toggles_filter() {
        let f = Key::Character("f".into());
        assert!(matches!(
            handle_inbox_key(&f, Modifiers::empty()),
            Some(Message::InboxToggleFilter)
        ));
    }

    #[test]
    fn jk_do_nothing_outside_the_inbox() {
        // The global handler never binds j/k off the board.
        assert!(press("j", false, false).is_none());
        assert!(press("k", false, false).is_none());
    }

    fn press(c: &str, modal_open: bool, on_board: bool) -> Option<Message> {
        handle_key_press(
            Key::Character(c.into()),
            Modifiers::empty(),
            modal_open,
            false,
            on_board,
        )
    }

    fn press_with(c: &str, modifiers: Modifiers, modal_open: bool) -> Option<Message> {
        handle_key_press(Key::Character(c.into()), modifiers, modal_open, false, true)
    }

    #[test]
    fn cmd_digit_focuses_that_tab() {
        for (c, i) in [("1", 0), ("5", 4), ("9", 8)] {
            assert!(matches!(
                press_with(c, Modifiers::COMMAND, false),
                Some(Message::Tab(TabMessage::Focus(n))) if n == i
            ));
        }
        assert!(press_with("0", Modifiers::COMMAND, false).is_none());
    }

    #[test]
    fn cmd_digit_does_nothing_while_a_modal_is_open() {
        assert!(press_with("1", Modifiers::COMMAND, true).is_none());
        assert!(press_with("9", Modifiers::COMMAND, true).is_none());
    }

    #[test]
    fn bare_digits_still_set_priority() {
        assert!(matches!(press("1", false, true), Some(Message::SetFocusedPriority(0))));
        assert!(matches!(press("4", false, true), Some(Message::SetFocusedPriority(3))));
        assert!(press("5", false, true).is_none());
        // Off the board a bare digit is still inert.
        assert!(press("1", false, false).is_none());
    }

    #[test]
    fn t_opens_move_team_on_board_only() {
        assert!(matches!(
            press("t", false, true),
            Some(Message::MoveFocusedIssueToTeam)
        ));
        assert!(press("t", false, false).is_none());
    }

    #[test]
    fn shift_i_opens_inbox_on_any_view() {
        for on_board in [true, false] {
            let m = handle_key_press(
                Key::Character("I".into()),
                Modifiers::SHIFT,
                false,
                false,
                on_board,
            );
            assert!(matches!(m, Some(Message::NavigateToView(View::Inbox))));
        }
    }

    #[test]
    fn shift_i_ignored_while_modal_open() {
        let m = handle_key_press(Key::Character("I".into()), Modifiers::SHIFT, true, false, true);
        assert!(m.is_none());
    }

    #[test]
    fn plain_i_does_nothing() {
        assert!(press("i", false, true).is_none());
    }

    #[test]
    fn t_ignored_while_modal_open() {
        assert!(press("t", true, true).is_none());
    }

    fn cmd(c: &str, modal_open: bool, on_board: bool) -> Option<Message> {
        handle_key_press(
            Key::Character(c.into()),
            Modifiers::COMMAND,
            modal_open,
            false,
            on_board,
        )
    }

    #[test]
    fn cmd_k_opens_palette_on_any_view() {
        assert!(matches!(
            cmd("k", false, true),
            Some(Message::OpenCommandPalette)
        ));
        assert!(matches!(
            cmd("k", false, false),
            Some(Message::OpenCommandPalette)
        ));
    }

    #[test]
    fn cmd_k_ignored_while_modal_open() {
        assert!(cmd("k", true, true).is_none());
        assert!(cmd("k", true, false).is_none());
    }

    #[test]
    fn cmd_f_still_focuses_search() {
        assert!(matches!(cmd("f", false, true), Some(Message::FocusSearch)));
        assert!(matches!(cmd("f", false, false), Some(Message::FocusSearch)));
    }

    #[test]
    fn plain_k_still_moves_focus_on_board() {
        assert!(matches!(
            press("k", false, true),
            Some(Message::FocusPrevIssue)
        ));
    }

    fn arrow(named: keyboard::key::Named, palette_open: bool, modal_open: bool) -> Option<Message> {
        handle_key_press(
            Key::Named(named),
            Modifiers::empty(),
            modal_open,
            palette_open,
            true,
        )
    }

    #[test]
    fn arrows_move_the_palette_highlight_while_it_is_open() {
        use keyboard::key::Named;
        assert!(matches!(
            arrow(Named::ArrowDown, true, true),
            Some(Message::PaletteMove(1))
        ));
        assert!(matches!(
            arrow(Named::ArrowUp, true, true),
            Some(Message::PaletteMove(-1))
        ));
    }

    #[test]
    fn arrows_are_ignored_in_other_modals() {
        use keyboard::key::Named;
        assert!(arrow(Named::ArrowDown, false, true).is_none());
        assert!(arrow(Named::ArrowUp, false, true).is_none());
    }
}

#[cfg(test)]
mod dashboard_key_tests {
    use super::*;

    #[test]
    fn dashboard_keys_map_to_focus_and_open() {
        let none = Modifiers::empty();
        let ch = |c: &str| Key::Character(c.into());
        assert!(matches!(handle_dashboard_key(&ch("j"), none), Some(Message::FocusNextIssue)));
        assert!(matches!(handle_dashboard_key(&ch("k"), none), Some(Message::FocusPrevIssue)));
        assert!(matches!(handle_dashboard_key(&ch("h"), none), Some(Message::FocusPrevLane)));
        assert!(matches!(handle_dashboard_key(&ch("l"), none), Some(Message::FocusNextLane)));
        assert!(matches!(
            handle_dashboard_key(&Key::Named(keyboard::key::Named::Enter), none),
            Some(Message::EditFocusedIssue)
        ));
        assert!(handle_dashboard_key(&ch("j"), Modifiers::SHIFT).is_none());
    }
}
