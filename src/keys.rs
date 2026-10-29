use iced::keyboard::{self, Key, Modifiers};

use crate::app::Message;

pub fn handle_key_press(
    key: Key,
    modifiers: Modifiers,
    modal_open: bool,
    on_board: bool,
) -> Option<Message> {
    if modal_open {
        return match key {
            Key::Named(keyboard::key::Named::Escape) => Some(Message::CloseModal),
            _ => None,
        };
    }

    match key {
        Key::Character(ref c) => match c.as_str() {
            // Global shortcuts — active on any view.
            "f" if modifiers.command() => Some(Message::FocusSearch),
            "L" | "l" if modifiers.shift() => Some(Message::OpenLabels),
            "p" if modifiers.is_empty() => Some(Message::CreateProject),
            "s" if modifiers.is_empty() => Some(Message::SyncNow),
            "," if modifiers.is_empty() => Some(Message::OpenSettings),
            "?" | "/" if modifiers.shift() || modifiers.is_empty() => Some(Message::ToggleHelp),

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
