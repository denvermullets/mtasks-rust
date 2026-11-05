//! Key routing while a Terminal has focus. Pure: no engine, no app state.
use iced::keyboard::key::Code;
use iced::keyboard::Modifiers;

use super::{KeyInput, KeyMods, TermKey};

#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    /// Cmd+J.
    ToggleFocus,
    /// Cmd+K.
    Palette,
    /// Cmd+V.
    Paste,
    /// Cmd+C: copy the selection. Ctrl+C is not this; it goes to the program.
    Copy,
    /// Cmd+1 to Cmd+9: the Tab at this zero-based position.
    Tab(usize),
    Send(KeyInput),
    Ignore,
}

/// What a key does while a Terminal has focus. Command keys never reach the
/// shell: Cmd+J, K, C, V and 1 to 9 are the app's, and Cmd+Q, Cmd+[ and Cmd+]
/// are reserved (Quit is the native menu item; the others do nothing here, so
/// no program ever sees a Command chord).
pub fn route(code: Code, mods: Modifiers, text: Option<&str>) -> Route {
    if let Some(key) = editing_key(code, mods) {
        return Route::Send(key);
    }
    if mods.logo() {
        return match code {
            Code::KeyJ => Route::ToggleFocus,
            Code::KeyK => Route::Palette,
            Code::KeyV => Route::Paste,
            Code::KeyC => Route::Copy,
            c => tab_position(c).map_or(Route::Ignore, Route::Tab),
        };
    }
    let Some(key) = term_key(code) else {
        return Route::Ignore;
    };
    // Control characters and macOS function-key text must not reach the
    // encoder as text.
    let text = text
        .filter(|t| {
            t.chars()
                .all(|c| !c.is_control() && !('\u{F700}'..='\u{F8FF}').contains(&c))
        })
        .map(str::to_string);
    Route::Send(KeyInput {
        key,
        mods: KeyMods {
            shift: mods.shift(),
            alt: mods.alt(),
            ctrl: mods.control(),
            logo: false,
        },
        text,
        option_as_alt: true,
    })
}

/// Mac line editing, as the shell's own keys: Cmd+Delete is `^U`, Cmd+Left
/// `^A`, Cmd+Right `^E`, Option+Left `ESC b` and Option+Right `ESC f`.
/// (Option is Alt everywhere, so `ESC b` is Alt+B.)
fn editing_key(code: Code, mods: Modifiers) -> Option<KeyInput> {
    let (ch, ctrl, alt) = if mods.logo() && !mods.alt() && !mods.control() {
        match code {
            Code::Backspace => ('u', true, false),
            Code::ArrowLeft => ('a', true, false),
            Code::ArrowRight => ('e', true, false),
            _ => return None,
        }
    } else if mods.alt() && !mods.logo() && !mods.control() && !mods.shift() {
        match code {
            Code::ArrowLeft => ('b', false, true),
            Code::ArrowRight => ('f', false, true),
            _ => return None,
        }
    } else {
        return None;
    };
    Some(KeyInput {
        key: TermKey::Char(ch),
        mods: KeyMods { shift: false, alt, ctrl, logo: false },
        text: None,
        option_as_alt: true,
    })
}

fn tab_position(code: Code) -> Option<usize> {
    let digits = [
        Code::Digit1,
        Code::Digit2,
        Code::Digit3,
        Code::Digit4,
        Code::Digit5,
        Code::Digit6,
        Code::Digit7,
        Code::Digit8,
        Code::Digit9,
    ];
    digits.iter().position(|d| *d == code)
}

pub fn term_key(code: Code) -> Option<TermKey> {
    use TermKey as K;
    let ch = |c: char| Some(K::Char(c));
    match code {
        Code::KeyA => ch('a'),
        Code::KeyB => ch('b'),
        Code::KeyC => ch('c'),
        Code::KeyD => ch('d'),
        Code::KeyE => ch('e'),
        Code::KeyF => ch('f'),
        Code::KeyG => ch('g'),
        Code::KeyH => ch('h'),
        Code::KeyI => ch('i'),
        Code::KeyJ => ch('j'),
        Code::KeyK => ch('k'),
        Code::KeyL => ch('l'),
        Code::KeyM => ch('m'),
        Code::KeyN => ch('n'),
        Code::KeyO => ch('o'),
        Code::KeyP => ch('p'),
        Code::KeyQ => ch('q'),
        Code::KeyR => ch('r'),
        Code::KeyS => ch('s'),
        Code::KeyT => ch('t'),
        Code::KeyU => ch('u'),
        Code::KeyV => ch('v'),
        Code::KeyW => ch('w'),
        Code::KeyX => ch('x'),
        Code::KeyY => ch('y'),
        Code::KeyZ => ch('z'),
        Code::Digit0 => ch('0'),
        Code::Digit1 => ch('1'),
        Code::Digit2 => ch('2'),
        Code::Digit3 => ch('3'),
        Code::Digit4 => ch('4'),
        Code::Digit5 => ch('5'),
        Code::Digit6 => ch('6'),
        Code::Digit7 => ch('7'),
        Code::Digit8 => ch('8'),
        Code::Digit9 => ch('9'),
        Code::Backquote => ch('`'),
        Code::Backslash => ch('\\'),
        Code::BracketLeft => ch('['),
        Code::BracketRight => ch(']'),
        Code::Comma => ch(','),
        Code::Equal => ch('='),
        Code::Minus => ch('-'),
        Code::Period => ch('.'),
        Code::Quote => ch('\''),
        Code::Semicolon => ch(';'),
        Code::Slash => ch('/'),
        Code::Space => Some(K::Space),
        Code::Enter | Code::NumpadEnter => Some(K::Enter),
        Code::Escape => Some(K::Escape),
        Code::Backspace => Some(K::Backspace),
        Code::Tab => Some(K::Tab),
        Code::Delete => Some(K::Delete),
        Code::Insert => Some(K::Insert),
        Code::Home => Some(K::Home),
        Code::End => Some(K::End),
        Code::PageUp => Some(K::PageUp),
        Code::PageDown => Some(K::PageDown),
        Code::ArrowUp => Some(K::ArrowUp),
        Code::ArrowDown => Some(K::ArrowDown),
        Code::ArrowLeft => Some(K::ArrowLeft),
        Code::ArrowRight => Some(K::ArrowRight),
        Code::F1 => Some(K::F(1)),
        Code::F2 => Some(K::F(2)),
        Code::F3 => Some(K::F(3)),
        Code::F4 => Some(K::F(4)),
        Code::F5 => Some(K::F(5)),
        Code::F6 => Some(K::F(6)),
        Code::F7 => Some(K::F(7)),
        Code::F8 => Some(K::F(8)),
        Code::F9 => Some(K::F(9)),
        Code::F10 => Some(K::F(10)),
        Code::F11 => Some(K::F(11)),
        Code::F12 => Some(K::F(12)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_keys_go_to_the_terminal_with_their_text() {
        let Route::Send(k) = route(Code::KeyJ, Modifiers::empty(), Some("j")) else {
            panic!("j must reach the shell, not the board");
        };
        assert_eq!(k.key, TermKey::Char('j'));
        assert_eq!(k.text.as_deref(), Some("j"));
        assert!(k.option_as_alt);
    }

    #[test]
    fn ctrl_c_reaches_the_program() {
        let Route::Send(k) = route(Code::KeyC, Modifiers::CTRL, Some("\u{3}")) else {
            panic!("Ctrl+C must be sent");
        };
        assert!(k.mods.ctrl);
        assert_eq!(k.text, None, "control characters are never sent as text");
    }

    #[test]
    fn reserved_command_keys_stay_with_the_app() {
        assert_eq!(route(Code::KeyJ, Modifiers::LOGO, None), Route::ToggleFocus);
        assert_eq!(route(Code::KeyK, Modifiers::LOGO, None), Route::Palette);
        assert_eq!(route(Code::KeyV, Modifiers::LOGO, None), Route::Paste);
        assert_eq!(route(Code::KeyX, Modifiers::LOGO, Some("x")), Route::Ignore);
    }

    #[test]
    fn cmd_c_copies_and_is_never_sent_while_ctrl_c_still_is() {
        assert_eq!(route(Code::KeyC, Modifiers::LOGO, Some("c")), Route::Copy);
        assert!(matches!(route(Code::KeyC, Modifiers::CTRL, None), Route::Send(_)));
        let other = route(Code::KeyC, Modifiers::LOGO | Modifiers::SHIFT, None);
        assert!(!matches!(other, Route::Send(_)), "no Command chord reaches the shell");
    }

    #[test]
    fn cmd_digits_pick_a_tab_and_other_reserved_keys_are_never_sent() {
        assert_eq!(route(Code::Digit1, Modifiers::LOGO, Some("1")), Route::Tab(0));
        assert_eq!(route(Code::Digit9, Modifiers::LOGO, Some("9")), Route::Tab(8));
        assert_eq!(route(Code::Digit0, Modifiers::LOGO, Some("0")), Route::Ignore);
        for code in [Code::KeyQ, Code::BracketLeft, Code::BracketRight] {
            assert_eq!(route(code, Modifiers::LOGO, None), Route::Ignore);
        }
    }

    fn sent(code: Code, mods: Modifiers) -> (char, bool, bool) {
        let Route::Send(k) = route(code, mods, None) else {
            panic!("{code:?} must be sent");
        };
        let TermKey::Char(c) = k.key else {
            panic!("expected a letter chord");
        };
        assert!(!k.mods.logo && k.text.is_none() && k.option_as_alt);
        (c, k.mods.ctrl, k.mods.alt)
    }

    #[test]
    fn mac_editing_keys_become_shell_editing_chords() {
        assert_eq!(sent(Code::Backspace, Modifiers::LOGO), ('u', true, false));
        assert_eq!(sent(Code::ArrowLeft, Modifiers::LOGO), ('a', true, false));
        assert_eq!(sent(Code::ArrowRight, Modifiers::LOGO), ('e', true, false));
        assert_eq!(sent(Code::ArrowLeft, Modifiers::ALT), ('b', false, true));
        assert_eq!(sent(Code::ArrowRight, Modifiers::ALT), ('f', false, true));
    }

    #[test]
    fn option_letters_are_alt_not_composed_text() {
        let Route::Send(k) = route(Code::KeyB, Modifiers::ALT, Some("\u{222b}")) else {
            panic!("Option+B must be sent");
        };
        assert!(k.mods.alt && k.option_as_alt);
        assert_eq!(k.key, TermKey::Char('b'));
    }

    #[test]
    fn plain_arrows_and_option_backspace_are_untouched() {
        let Route::Send(k) = route(Code::ArrowLeft, Modifiers::empty(), None) else {
            panic!();
        };
        assert_eq!(k.key, TermKey::ArrowLeft);
        let Route::Send(k) = route(Code::Backspace, Modifiers::ALT, None) else {
            panic!();
        };
        assert_eq!((k.key, k.mods.alt), (TermKey::Backspace, true));
    }

    #[test]
    fn macos_function_key_text_is_dropped() {
        let Route::Send(k) = route(Code::ArrowUp, Modifiers::empty(), Some("\u{F700}")) else {
            panic!("arrow must be sent");
        };
        assert_eq!(k.text, None);
        assert_eq!(k.key, TermKey::ArrowUp);
    }
}
