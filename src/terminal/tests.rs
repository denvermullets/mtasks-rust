//! Tests drive the public Terminal API with real shells. No window, no database.
use super::*;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

fn theme() -> TermTheme {
    TermTheme {
        fg: [200; 3],
        bg: [20; 3],
        cursor: [200; 3],
        ansi: [[128; 3]; 16],
    }
}

fn sh_config(team: &str) -> SpawnConfig {
    SpawnConfig {
        team: team.into(),
        cols: 200,
        rows: 40,
        theme: theme(),
        cwd: None,
        // /bin/sh keeps the tests clear of the developer's zsh startup files.
        program: Some(("/bin/sh".into(), vec![])),
    }
}

fn terminals() -> Terminals {
    Terminals::new(|| {})
}

fn key(c: char) -> KeyInput {
    KeyInput {
        key: TermKey::Char(c),
        mods: KeyMods::default(),
        text: Some(c.to_string()),
        option_as_alt: true,
    }
}

fn enter() -> KeyInput {
    KeyInput {
        key: TermKey::Enter,
        mods: KeyMods::default(),
        text: None,
        option_as_alt: true,
    }
}

fn type_line(t: &Terminals, id: TerminalId, line: &str) {
    for c in line.chars() {
        t.send_key(id, key(c));
    }
    t.send_key(id, enter());
}

/// Poll until `check` holds for the latest frame, or panic after `secs`.
fn wait_for(t: &Terminals, id: TerminalId, what: &str, check: impl Fn(&Frame) -> bool) -> Arc<Frame> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(f) = t.frame(id) {
            if check(&f) {
                return f;
            }
        }
        if Instant::now() > deadline {
            let screen = t.frame(id).map(|f| f.lines().join("\n")).unwrap_or_default();
            panic!("timed out waiting for {what}; screen was:\n{screen}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn close_all(mut t: Terminals) {
    for id in t.ids() {
        if let Some(c) = t.close(id) {
            c.wait();
        }
    }
}

#[test]
fn a_command_typed_with_send_key_shows_its_output_in_a_frame() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "echo $((1234*5))");
    let f = wait_for(&t, id, "6170", |f| f.contains_line("6170"));
    assert_eq!(f.cols, 200);
    assert_eq!(f.rows.len(), 40);
    assert!(!f.exited);
    close_all(t);
}

#[test]
fn the_shell_sees_the_terminal_environment() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "echo env=$TERM/$COLORTERM");
    wait_for(&t, id, "env line", |f| f.contains_line("env=xterm-256color/truecolor"));
    // The PTY is in UTF-8 mode so the line editor erases whole characters.
    type_line(&t, id, "stty -a | tr ' ' '\\n' | grep -c '^iutf8'");
    wait_for(&t, id, "iutf8 count of 1", |f| f.contains_line("1"));
    close_all(t);
}

#[test]
fn an_exiting_shell_is_reported_and_keeps_its_last_screen() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "echo bye$((40+2))");
    wait_for(&t, id, "bye42", |f| f.contains_line("bye42"));
    type_line(&t, id, "exit");
    let f = wait_for(&t, id, "exited frame", |f| f.exited);
    assert!(f.contains_line("bye42"));
    assert!(t.is_exited(id));
    close_all(t);
}

#[test]
fn open_count_counts_a_teams_live_shells() {
    let mut t = terminals();
    let a = t.spawn(sh_config("team-a")).unwrap();
    let _b = t.spawn(sh_config("team-a")).unwrap();
    let _c = t.spawn(sh_config("team-b")).unwrap();
    assert_eq!(t.open_count("team-a"), 2);
    assert_eq!(t.open_count("team-b"), 1);
    assert_eq!(t.open_count("nobody"), 0);
    type_line(&t, a, "exit");
    wait_for(&t, a, "exit", |f| f.exited);
    assert_eq!(t.open_count("team-a"), 1);
    close_all(t);
}

#[test]
fn a_panic_on_an_owner_thread_marks_only_that_terminal_dead() {
    let mut t = terminals();
    let doomed = t.spawn(sh_config("t1")).unwrap();
    let healthy = t.spawn(sh_config("t1")).unwrap();
    t.crash_owner(doomed);
    let f = wait_for(&t, doomed, "dead frame", |f| f.dead);
    assert!(f.exited);
    // The test process and the other Terminal are fine.
    type_line(&t, healthy, "echo alive$((20+1))");
    wait_for(&t, healthy, "alive21", |f| f.contains_line("alive21"));
    assert!(!t.frame(healthy).unwrap().dead);
    close_all(t);
}

#[test]
fn one_wake_is_pending_until_the_app_takes_it() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    let mut t = Terminals::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    });
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "echo first$((1+1))");
    wait_for(&t, id, "first2", |f| f.contains_line("first2"));
    assert_eq!(wakes.load(Ordering::SeqCst), 1, "frames after the first must not wake again");
    t.ack_wake();
    type_line(&t, id, "echo second$((2+2))");
    wait_for(&t, id, "second4", |f| f.contains_line("second4"));
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
    close_all(t);
}

#[test]
fn a_hidden_terminal_builds_no_frames_until_shown() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    let mut t = Terminals::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    });
    let id = t.spawn(sh_config("t1")).unwrap();
    wait_for(&t, id, "a prompt", |f| f.cursor.is_some() && f.lines().iter().any(|l| l.contains('$')));
    t.set_visible(id, false);
    std::thread::sleep(Duration::from_millis(200));
    t.ack_wake();
    let before = wakes.load(Ordering::SeqCst);
    type_line(&t, id, "echo hidden$((3+4))");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(wakes.load(Ordering::SeqCst), before, "a hidden Terminal must not wake the UI");
    assert!(!t.frame(id).unwrap().contains_line("hidden7"));
    // Showing it builds a full frame at once, including what arrived while hidden.
    t.set_visible(id, true);
    wait_for(&t, id, "hidden7", |f| f.contains_line("hidden7"));
    close_all(t);
}

/// Only `engine.rs` may name the engine crates, so an unstable API stays in one file.
#[test]
fn only_the_engine_file_touches_the_engine_crates() {
    fn scan(dir: &std::path::Path, hits: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path, hits);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                // Split so this test file doesn't match itself.
                let needles = [["libghostty", "_vt"].concat(), ["portable", "_pty"].concat()];
                if needles.iter().any(|n| text.contains(n.as_str())) {
                    hits.push(path.to_string_lossy().into_owned());
                }
            }
        }
    }
    let mut hits = vec![];
    scan(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut hits);
    assert_eq!(hits.len(), 1, "files naming the engine crates: {hits:?}");
    assert!(hits[0].ends_with("src/terminal/engine.rs"), "{hits:?}");
}
