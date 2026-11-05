//! Mouse reporting (JRST-383): clicks reach a program only while it tracks the mouse.
use super::*;

/// A shell that prints mouse input as visible text, with the terminal in raw mode.
fn echo_mouse(t: &Terminals, id: TerminalId, tracking: bool) {
    let on = if tracking { "printf '\\033[?1000h\\033[?1006h'; " } else { "" };
    type_line(t, id, &format!("stty raw -echo; {on}cat -v"));
}

fn click(col: i32, row: i32) -> [MouseInput; 2] {
    let at = |action| MouseInput { action, col, row, mods: KeyMods::default() };
    [at(MouseAction::Press(MouseButton::Left)), at(MouseAction::Release(MouseButton::Left))]
}

#[test]
fn a_click_reaches_a_program_that_tracks_the_mouse() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    echo_mouse(&t, id, true);
    wait_for(&t, id, "tracking on", |f| f.tracking);
    for e in click(4, 2) {
        t.mouse(id, e);
    }
    // SGR encoding is 1-based: column 5, row 3.
    wait_for(&t, id, "press and release reports", |f| {
        f.lines().iter().any(|l| l.contains("^[[<0;5;3M^[[<0;5;3m"))
    });
    close_all(t);
}

#[test]
fn a_click_sends_nothing_to_a_program_that_is_not_tracking() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    echo_mouse(&t, id, false);
    wait_for(&t, id, "raw cat running", |f| f.lines().iter().any(|l| l.contains("cat -v")));
    for e in click(4, 2) {
        t.mouse(id, e);
    }
    // Input is handled in order, so once the later key shows, an earlier click would have.
    t.send_key(id, key('z'));
    let f = wait_for(&t, id, "the later key", |f| f.lines().iter().any(|l| l.ends_with('z')));
    assert!(!f.lines().iter().any(|l| l.contains("^[")), "{:?}", f.lines());
    close_all(t);
}
