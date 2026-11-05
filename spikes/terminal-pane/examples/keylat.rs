//! PROTOTYPE (JRST-328): headless key-to-frame timing. No window, so it cannot take keyboard focus.
//! Sends Up arrows to a real zsh; never sends Enter. Logs timings only, never terminal content.
#![allow(dead_code)]
#[path = "../src/term.rs"]
mod term;
use libghostty_vt::key;
use std::time::Duration;
use term::*;

fn main() {
    std::env::set_var("SPIKE_KEYTEST", "1");
    let (tx, _rx) = iced::futures::channel::mpsc::unbounded();
    let theme = TermTheme { fg: [200; 3], bg: [20; 3], cursor: [200; 3], ansi: [[128; 3]; 16] };
    let t = term::spawn(153, 24, theme, tx);
    let up = |gap: u64, n: usize| for _ in 0..n {
        let _ = t.tx.send(Cmd::Key(KeyInput { key: key::Key::ArrowUp, mods: key::Mods::empty(), unshifted: '\0', text: None, option_as_alt: true }));
        std::thread::sleep(Duration::from_millis(gap));
    };
    std::thread::sleep(Duration::from_secs(3));
    eprintln!("--- tapping, one press every 300ms");
    up(300, 12);
    eprintln!("--- holding, 30 presses/s");
    up(33, 30);
    std::thread::sleep(Duration::from_millis(300));
    std::process::exit(0);
}
