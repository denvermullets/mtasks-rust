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
    Send(KeyInput),
    Ignore,
}

pub fn route(code: Code, mods: Modifiers, text: Option<&str>) -> Route {
    if mods.logo() {
        return match code {
            Code::KeyJ => Route::ToggleFocus,
            Code::KeyK => Route::Palette,
            Code::KeyV => Route::Paste,
            _ => Route::Ignore,
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
    fn macos_function_key_text_is_dropped() {
        let Route::Send(k) = route(Code::ArrowUp, Modifiers::empty(), Some("\u{F700}")) else {
            panic!("arrow must be sent");
        };
        assert_eq!(k.text, None);
        assert_eq!(k.key, TermKey::ArrowUp);
    }
}
