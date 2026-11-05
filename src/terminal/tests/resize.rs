//! A PTY takes the size of the pane showing it (JRST-373).
use super::*;

#[test]
fn stty_size_reports_the_size_the_pane_asked_for() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "stty size");
    wait_for(&t, id, "the spawn size", |f| f.contains_line("40 200"));
    t.resize(id, 100, 30, 8.0, 16.0);
    type_line(&t, id, "stty size");
    let f = wait_for(&t, id, "the new size", |f| f.contains_line("30 100"));
    assert_eq!((f.cols, f.rows.len()), (100, 30), "the grid follows the PTY");
    close_all(t);
}

#[test]
fn the_shell_reflows_to_the_new_width() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    t.resize(id, 20, 10, 8.0, 16.0);
    type_line(&t, id, "echo aaaaaaaaaabbbbbbbbbbcccccccccc");
    let f = wait_for(&t, id, "a wrapped line", |f| f.lines().iter().any(|l| l == "cccccccccc"));
    assert!(f.lines().iter().all(|l| l.chars().count() <= 20));
    close_all(t);
}

#[test]
fn resizing_during_a_flood_leaves_the_terminal_working() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    // Small on purpose: a debug build parses about 1,300 lines a second.
    type_line(&t, id, "seq 1 2000");
    for i in 0..30u16 {
        let (cols, rows) = if i % 2 == 0 { (60 + i, 20) } else { (90, 15 + i % 7) };
        t.resize(id, cols, rows, 8.0, 16.0);
        std::thread::sleep(Duration::from_millis(5));
    }
    t.resize(id, 120, 30, 8.0, 16.0);
    type_line(&t, id, "echo after$((6*7))");
    let f = wait_for(&t, id, "output after the flood", |f| f.contains_line("after42"));
    assert!(!f.dead && !f.exited);
    assert_eq!((f.cols, f.rows.len()), (120, 30));
    close_all(t);
}
