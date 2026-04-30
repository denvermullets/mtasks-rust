use iced::keyboard::{self, Key, Modifiers};

use crate::app::Message;

pub fn handle_key_press(key: Key, modifiers: Modifiers, modal_open: bool) -> Option<Message> {
    if modal_open {
        return match key {
            Key::Named(keyboard::key::Named::Escape) => Some(Message::CloseModal),
            _ => None,
        };
    }

    match key {
        Key::Character(ref c) => match c.as_str() {
            "n" if modifiers.is_empty() => Some(Message::CreateIssue),
            "e" if modifiers.is_empty() => Some(Message::EditFocusedIssue),
            "d" if modifiers.is_empty() => Some(Message::ConfirmDeleteFocusedIssue),
            "j" if modifiers.is_empty() => Some(Message::FocusNextIssue),
            "k" if modifiers.is_empty() => Some(Message::FocusPrevIssue),
            "h" if modifiers.is_empty() => Some(Message::FocusPrevLane),
            "l" if modifiers.is_empty() => Some(Message::FocusNextLane),
            "m" if modifiers.is_empty() => Some(Message::MoveIssueNextLane),
            "M" | "m" if modifiers.shift() => Some(Message::MoveIssuePrevLane),
            "p" if modifiers.is_empty() => Some(Message::CreateProject),
            "s" if modifiers.is_empty() => Some(Message::SyncNow),
            "," if modifiers.is_empty() => Some(Message::OpenSettings),
            "?" | "/" if modifiers.shift() || modifiers.is_empty() => Some(Message::ToggleHelp),
            "1" if modifiers.is_empty() => Some(Message::SetFocusedPriority(0)),
            "2" if modifiers.is_empty() => Some(Message::SetFocusedPriority(1)),
            "3" if modifiers.is_empty() => Some(Message::SetFocusedPriority(2)),
            "4" if modifiers.is_empty() => Some(Message::SetFocusedPriority(3)),
            _ => None,
        },
        Key::Named(named) => match named {
            keyboard::key::Named::Enter => Some(Message::EditFocusedIssue),
            keyboard::key::Named::Escape => Some(Message::CloseModal),
            keyboard::key::Named::ArrowDown => Some(Message::FocusNextIssue),
            keyboard::key::Named::ArrowUp => Some(Message::FocusPrevIssue),
            keyboard::key::Named::ArrowLeft => Some(Message::FocusPrevLane),
            keyboard::key::Named::ArrowRight => Some(Message::FocusNextLane),
            _ => None,
        },
        _ => None,
    }
}
