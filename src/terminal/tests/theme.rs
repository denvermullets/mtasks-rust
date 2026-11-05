//! Live theme changes (JRST-390): `set_theme` recolours a Terminal at once,
//! hidden or not, and so does the scrollback behind it.
use super::*;

const RED_A: Rgb = [250, 10, 10];
const RED_B: Rgb = [10, 10, 250];

fn themed(red: Rgb, bg: Rgb) -> TermTheme {
    let mut t = theme();
    t.ansi[1] = red;
    t.bg = bg;
    t
}

/// The foreground of the first run that is exactly `needle` (not the echoed command).
fn fg_of(f: &Frame, needle: &str) -> Option<Rgb> {
    f.rows.iter().flat_map(|r| r.runs.iter()).find(|r| r.text == needle).map(|r| r.fg)
}

fn print_red(t: &Terminals, id: TerminalId) {
    type_line(t, id, "printf '\\033[31mREDTXT\\033[0m \\033[38;2;1;2;3mTRUETXT\\033[0m\\n'");
}

#[test]
fn a_theme_change_recolours_a_visible_terminal_at_once() {
    let mut t = terminals();
    let mut cfg = sh_config("t1");
    cfg.theme = themed(RED_A, [20; 3]);
    let id = t.spawn(cfg).unwrap();
    print_red(&t, id);
    wait_for(&t, id, "red in theme A", |f| fg_of(f, "REDTXT") == Some(RED_A));
    t.set_theme(id, themed(RED_B, [90; 3]));
    let f = wait_for(&t, id, "red in theme B", |f| fg_of(f, "REDTXT") == Some(RED_B));
    assert_eq!(f.bg, [90; 3]);
    assert_eq!(fg_of(&f, "TRUETXT"), Some([1, 2, 3]), "a truecolor choice is the program's own");
    close_all(t);
}

#[test]
fn a_hidden_terminal_has_the_new_colours_when_shown() {
    let mut t = terminals();
    let mut cfg = sh_config("t1");
    cfg.theme = themed(RED_A, [20; 3]);
    let id = t.spawn(cfg).unwrap();
    print_red(&t, id);
    wait_for(&t, id, "red in theme A", |f| fg_of(f, "REDTXT") == Some(RED_A));
    t.set_visible(id, false);
    std::thread::sleep(Duration::from_millis(300));
    t.set_theme(id, themed(RED_B, [90; 3]));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(fg_of(&t.frame(id).unwrap(), "REDTXT"), Some(RED_A), "hidden: no frame is built");
    t.set_visible(id, true);
    let f = wait_for(&t, id, "red in theme B", |f| fg_of(f, "REDTXT") == Some(RED_B));
    assert_eq!(f.bg, [90; 3]);
    close_all(t);
}

#[test]
fn scrollback_recolours_for_palette_colours_but_not_truecolor() {
    let mut t = terminals();
    let mut cfg = sh_config("t1");
    cfg.rows = 10;
    cfg.theme = themed(RED_A, [20; 3]);
    let id = t.spawn(cfg).unwrap();
    print_red(&t, id);
    type_line(&t, id, "seq 1 60");
    wait_for(&t, id, "the red line scrolled off", |f| {
        f.contains_line("60") && f.scrollback > 0 && fg_of(f, "REDTXT").is_none()
    });
    t.set_theme(id, themed(RED_B, [90; 3]));
    t.scroll_top(id);
    let f = wait_for(&t, id, "the scrollback top in view", |f| fg_of(f, "REDTXT").is_some());
    assert_eq!(fg_of(&f, "REDTXT"), Some(RED_B), "scrollback follows the palette");
    assert_eq!(fg_of(&f, "TRUETXT"), Some([1, 2, 3]));
    close_all(t);
}
