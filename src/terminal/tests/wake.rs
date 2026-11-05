//! Wake behaviour the UI relies on for 0% idle CPU (JRST-372).
use super::*;

fn counting() -> (Terminals, Arc<AtomicUsize>) {
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    let t = Terminals::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    });
    (t, wakes)
}

fn at_a_prompt(t: &Terminals, id: TerminalId) {
    wait_for(t, id, "a prompt", |f| {
        f.cursor.is_some() && f.lines().iter().any(|l| l.contains('$'))
    });
    std::thread::sleep(Duration::from_millis(300));
    t.ack_wake();
}

#[test]
fn an_idle_terminal_sends_no_wakes() {
    let (mut t, wakes) = counting();
    let id = t.spawn(sh_config("t1")).unwrap();
    at_a_prompt(&t, id);
    let before = wakes.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(wakes.load(Ordering::SeqCst), before, "nothing changed, so nothing may wake the UI");
    close_all(t);
}

#[test]
fn a_change_wakes_once_the_app_has_taken_the_last_wake() {
    let (mut t, wakes) = counting();
    let id = t.spawn(sh_config("t1")).unwrap();
    at_a_prompt(&t, id);
    let before = wakes.load(Ordering::SeqCst);
    type_line(&t, id, "echo ping$((1+1))");
    wait_for(&t, id, "ping2", |f| f.contains_line("ping2"));
    assert_eq!(wakes.load(Ordering::SeqCst), before + 1);
    close_all(t);
}
