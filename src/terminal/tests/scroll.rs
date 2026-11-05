//! Scrollback and the wheel (JRST-384).
use super::*;

fn wheel(lines: f32) -> WheelInput {
    WheelInput { rows: lines, col: 4, row: 2, mods: KeyMods::default() }
}

fn top_line_number(f: &Frame) -> Option<u32> {
    f.lines().first().and_then(|l| l.trim().parse().ok())
}

#[test]
fn after_20000_lines_the_last_10000_can_be_scrolled_to() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "seq 1 20000");
    // A debug build parses about 1,100 lines a second here, so give it time.
    wait_for_secs(60, &t, id, "the last line", |f| f.contains_line("20000"));
    // The engine counts the screen's rows in its 10,000, and prunes by page.
    let kept = t.frame(id).unwrap().scrollback;
    assert!((9_900..=10_000).contains(&kept), "history held {kept} rows");
    t.wheel(id, wheel(1.0e6));
    let f = wait_for(&t, id, "the top of history", |f| top_line_number(f).is_some_and(|n| n < 10_100));
    let top = top_line_number(&f).unwrap();
    // 10,000 history rows end above the 40 screen rows, so line 10,001 is the
    // first one still reachable (a prompt line or two shifts that a little).
    assert!((9_950..=10_001).contains(&top), "top of history was {top}");
    close_all(t);
}

#[test]
fn a_key_press_returns_to_the_bottom() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "seq 1 500");
    wait_for(&t, id, "the last line", |f| f.contains_line("500"));
    t.wheel(id, wheel(1.0e6));
    wait_for(&t, id, "scrolled up", |f| !f.contains_line("500"));
    t.send_key(id, key('x'));
    wait_for(&t, id, "back at the bottom", |f| f.contains_line("500"));
    close_all(t);
}

#[test]
fn scrolling_moves_through_history_a_row_at_a_time() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "seq 1 500");
    wait_for(&t, id, "the last line", |f| f.contains_line("500"));
    let before = top_line_number(&t.frame(id).unwrap()).unwrap();
    t.wheel(id, wheel(5.0));
    wait_for(&t, id, "5 rows up", |f| top_line_number(f) == Some(before - 5));
    t.wheel(id, wheel(-5.0));
    wait_for(&t, id, "back down", |f| top_line_number(f) == Some(before));
    close_all(t);
}

#[test]
fn the_wheel_goes_to_a_program_that_tracks_the_mouse_not_to_scrollback() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "seq 1 500");
    wait_for(&t, id, "the last line", |f| f.contains_line("500"));
    type_line(&t, id, "stty raw -echo; printf '\\033[?1000h\\033[?1006h'; cat -v");
    wait_for(&t, id, "tracking on", |f| f.tracking);
    t.wheel(id, wheel(1.0));
    // Button 64 is wheel up, in SGR form, press then release.
    wait_for(&t, id, "wheel reports", |f| {
        f.lines().iter().any(|l| l.contains("^[[<64;5;3M^[[<64;5;3m"))
    });
    close_all(t);
}

#[test]
fn the_wheel_sends_arrow_keys_to_a_full_screen_program_that_ignores_the_mouse() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "stty raw -echo; printf '\\033[?1049h'; cat -v");
    wait_for(&t, id, "alt screen", |f| f.alt);
    t.wheel(id, wheel(2.0));
    wait_for(&t, id, "two ups", |f| f.lines().iter().any(|l| l.contains("^[[A^[[A")));
    close_all(t);
}
