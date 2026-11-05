//! Hidden Terminals keep parsing, build no frames and send no output wakes
//! (JRST-374). Floods are small: a debug build parses about 1,300 lines a second.
use super::*;

fn counting() -> (Terminals, Arc<AtomicUsize>) {
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    let t = Terminals::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    });
    (t, wakes)
}

fn hide_at_a_prompt(t: &Terminals, id: TerminalId) {
    wait_for(t, id, "a prompt", |f| {
        f.cursor.is_some() && f.lines().iter().any(|l| l.contains('$'))
    });
    t.set_visible(id, false);
    std::thread::sleep(Duration::from_millis(300));
    t.ack_wake();
}

#[test]
fn a_flooding_hidden_terminal_produces_no_wakes() {
    let (mut t, wakes) = counting();
    let id = t.spawn(sh_config("t1")).unwrap();
    hide_at_a_prompt(&t, id);
    let before = wakes.load(Ordering::SeqCst);
    type_line(&t, id, "seq 1 2000");
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(wakes.load(Ordering::SeqCst), before, "a hidden flood must not wake the UI");
    assert!(!t.frame(id).unwrap().contains_line("2000"), "and must not build frames");
    close_all(t);
}

#[test]
fn becoming_visible_gives_a_full_correct_frame_at_once() {
    let (mut t, wakes) = counting();
    let id = t.spawn(sh_config("t1")).unwrap();
    hide_at_a_prompt(&t, id);
    type_line(&t, id, "seq 1 2000");
    type_line(&t, id, "echo done$((6*7))");
    // Parsing is never throttled: give the hidden Terminal time to finish.
    std::thread::sleep(Duration::from_secs(5));
    let before = wakes.load(Ordering::SeqCst);
    t.set_visible(id, true);
    let f = wait_for(&t, id, "a full frame", |f| f.contains_line("done42"));
    assert_eq!(f.rows.len(), 40);
    assert!(f.contains_line("2000"), "the last output must be on screen");
    assert!(f.cursor.is_some());
    assert!(wakes.load(Ordering::SeqCst) > before, "showing it must wake the UI");
    close_all(t);
}

#[test]
fn a_hidden_terminals_shell_exit_is_still_reported() {
    let (mut t, wakes) = counting();
    let id = t.spawn(sh_config("t1")).unwrap();
    hide_at_a_prompt(&t, id);
    let before = wakes.load(Ordering::SeqCst);
    type_line(&t, id, "exit");
    let deadline = Instant::now() + Duration::from_secs(10);
    while wakes.load(Ordering::SeqCst) == before {
        assert!(Instant::now() < deadline, "no wake for a hidden exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(t.is_exited(id));
    assert_eq!(t.open_count("t1"), 0);
    t.set_visible(id, true);
    wait_for(&t, id, "an exited frame", |f| f.exited);
    close_all(t);
}
