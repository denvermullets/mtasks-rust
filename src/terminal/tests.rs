//! Tests drive the public Terminal API with real shells. No window, no database.
use super::*;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

fn theme() -> TermTheme {
    TermTheme {
        fg: [200; 3],
        bg: [20; 3],
        cursor: [200; 3],
        selection: [60; 3],
        light: false,
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

fn process_exists(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn group_exists(pgid: u32) -> bool {
    unsafe { libc::killpg(pgid as libc::pid_t, 0) == 0 }
}

#[test]
fn closing_a_normal_shell_reaps_it_and_ends_all_three_threads() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "echo up$((5+6))");
    wait_for(&t, id, "up11", |f| f.contains_line("up11"));
    let pid = t.pid(id).unwrap();
    assert!(process_exists(pid));

    let started = Instant::now();
    let report = t.close(id).unwrap().wait();

    assert!(started.elapsed() < Duration::from_millis(1500), "a shell that obeys SIGHUP closes at once");
    assert!(!report.escalated_to_sigkill);
    assert_eq!(report.threads_exited, 3);
    assert!(!process_exists(pid), "the child must be reaped, not left as a zombie");
    assert!(t.frame(id).is_none(), "a closed Terminal leaves the set");
    assert_eq!(t.open_count("t1"), 0);
}

#[test]
fn a_program_ignoring_sighup_is_killed_after_two_seconds() {
    let mut t = terminals();
    let mut config = sh_config("t1");
    config.program = Some((
        "/bin/sh".into(),
        vec!["-c".into(), "trap '' HUP; echo ready$((6*7)); sleep 100".into()],
    ));
    let id = t.spawn(config).unwrap();
    wait_for(&t, id, "ready42", |f| f.contains_line("ready42"));
    let pid = t.pid(id).unwrap();

    let started = Instant::now();
    let closing = t.close(id).unwrap();
    std::thread::sleep(Duration::from_millis(1000));
    assert!(!closing.is_done(), "SIGHUP is ignored, so close must still be waiting");
    let report = closing.wait();
    let took = started.elapsed();

    assert!(took >= Duration::from_millis(1900), "killed too early: {took:?}");
    assert!(took < Duration::from_secs(5), "took too long: {took:?}");
    assert!(report.escalated_to_sigkill);
    assert_eq!(report.threads_exited, 3);
    assert!(!process_exists(pid));
    assert!(!group_exists(pid), "the whole process group must be gone, sleep included");
}

#[test]
fn closing_an_exited_terminal_works() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "exit");
    wait_for(&t, id, "exit", |f| f.exited);
    let pid = t.pid(id).unwrap();
    let report = t.close(id).unwrap().wait();
    assert!(!report.escalated_to_sigkill);
    assert_eq!(report.threads_exited, 3);
    assert!(!process_exists(pid));
}

#[test]
fn closing_a_dead_terminal_works() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    t.crash_owner(id);
    wait_for(&t, id, "dead", |f| f.dead);
    let pid = t.pid(id).unwrap();
    let report = t.close(id).unwrap().wait();
    assert_eq!(report.threads_exited, 3);
    assert!(!process_exists(pid));
}

/// A shell that ignores SIGHUP and writes after the owner died. macOS holds a process in
/// exit until the PTY's output queue drains into the master, so a close that stops
/// reading the master hangs in `wait`, even after SIGKILL.
#[test]
fn closing_after_the_owner_died_still_drains_the_pty() {
    let mut t = terminals();
    let mut cfg = sh_config("t1");
    cfg.program = Some((
        "/bin/sh".into(),
        vec!["-c".into(), "trap '' HUP TERM; printf a; sleep 0.5; printf b; sleep 0.5; printf c; sleep 30".into()],
    ));
    let id = t.spawn(cfg).unwrap();
    t.crash_owner(id);
    wait_for(&t, id, "dead", |f| f.dead);
    // The first byte after the crash makes the reader stop; later bytes sit unread.
    std::thread::sleep(Duration::from_millis(1800));
    let pid = t.pid(id).unwrap();
    let closing = t.close(id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while !closing.is_done() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(closing.is_done(), "close hung with unread PTY output");
    let report = closing.wait();
    assert!(report.escalated_to_sigkill);
    assert!(report.reaped);
    assert_eq!(report.threads_exited, 3);
    assert!(!process_exists(pid));
}

/// Process-wide counts straight from the OS, so nothing the engine does can hide a leak.
struct OsCounts {
    threads: i32,
    fds: i32,
    children: i32,
}

fn os_counts() -> OsCounts {
    let pid = std::process::id() as libc::pid_t;
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    let got = unsafe {
        libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, (&mut info as *mut libc::proc_taskinfo).cast(), size)
    };
    assert_eq!(got, size, "proc_pidinfo(TASKINFO) failed");
    // Each proc_fdinfo is 8 bytes. A null buffer only estimates (with headroom), so list for real.
    let mut fdbuf = vec![0u8; 8 * 16384];
    let fd_bytes = unsafe {
        libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, fdbuf.as_mut_ptr().cast(), fdbuf.len() as libc::c_int)
    };
    let mut kids = vec![0 as libc::pid_t; 4096];
    // Returns the number of child pids, not bytes.
    let kid_count = unsafe { libc::proc_listchildpids(pid, kids.as_mut_ptr().cast(), (kids.len() * 4) as libc::c_int) };
    OsCounts { threads: info.pti_threadnum, fds: fd_bytes / 8, children: kid_count.max(0) }
}

/// Runs in its own process (see `fifty_spawns_and_closes_leave_nothing_behind`), where nothing
/// else is spawning threads or opening files, so the OS counts mean something.
#[test]
#[ignore = "run by fifty_spawns_and_closes_leave_nothing_behind in a fresh process"]
fn leak_probe() {
    let mut t = terminals();
    // Warm up so one-time process state (allocator threads, lazy statics) isn't counted.
    let warm = t.spawn(sh_config("warm")).unwrap();
    type_line(&t, warm, "echo warm$((1+1))");
    wait_for(&t, warm, "warm2", |f| f.contains_line("warm2"));
    t.close(warm).unwrap().wait();
    std::thread::sleep(Duration::from_millis(200));

    let start = os_counts();
    let ids: Vec<_> = (0..50).map(|_| t.spawn(sh_config("leak")).unwrap()).collect();
    for (n, id) in ids.iter().enumerate() {
        let want = format!("n{}", 1000 + n);
        type_line(&t, *id, &format!("echo n$((1000+{n}))"));
        wait_for(&t, *id, &want, |f| f.contains_line(&want));
    }
    let busy = os_counts();
    assert!(busy.children >= start.children + 50, "50 real shells should be running, saw {}", busy.children);
    assert!(busy.threads >= start.threads + 150, "three threads each");

    let closings: Vec<_> = ids.into_iter().map(|id| t.close(id).unwrap()).collect();
    for c in closings {
        let report = c.wait();
        assert_eq!(report.threads_exited, 3);
        assert!(!report.escalated_to_sigkill);
    }
    assert_eq!(t.ids().len(), 0);

    // Thread exit is final once joined, but the OS may take a moment to reflect it.
    let deadline = Instant::now() + Duration::from_secs(5);
    let end = loop {
        let now = os_counts();
        if (now.threads, now.fds, now.children) == (start.threads, start.fds, start.children)
            || Instant::now() > deadline
        {
            break now;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(end.threads, start.threads, "threads leaked");
    assert_eq!(end.children, start.children, "child processes leaked");
    assert_eq!(end.fds, start.fds, "file descriptors leaked");
}

#[test]
fn fifty_spawns_and_closes_leave_nothing_behind() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["terminal::tests::leak_probe", "--exact", "--ignored", "--test-threads=1"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
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

/// A shell that asks `CSI ?996n`, then prints the 9-byte reply as plain characters.
fn scheme_query_config(light: bool) -> SpawnConfig {
    let mut cfg = sh_config("t1");
    cfg.theme.light = light;
    let script = "stty -echo -icanon; printf '\\033[?996n'; \
                  r=$(dd bs=1 count=9 2>/dev/null | od -An -c | tr -d ' '); echo \"reply=$r\"";
    cfg.program = Some(("/bin/sh".into(), vec!["-c".into(), script.into()]));
    cfg
}

#[test]
fn csi_996n_gets_the_dark_reply_under_a_dark_theme() {
    let mut t = terminals();
    let id = t.spawn(scheme_query_config(false)).unwrap();
    wait_for(&t, id, "dark reply", |f| f.contains_line("reply=033[?997;1n"));
    close_all(t);
}

#[test]
fn csi_996n_gets_the_light_reply_under_a_light_theme() {
    let mut t = terminals();
    let id = t.spawn(scheme_query_config(true)).unwrap();
    wait_for(&t, id, "light reply", |f| f.contains_line("reply=033[?997;2n"));
    close_all(t);
}

#[test]
fn mode_2031_pushes_a_report_when_the_theme_changes() {
    // The program enables 2031 and waits for 9 bytes it never asked for.
    let mut cfg = sh_config("t1");
    let script = "stty -echo -icanon; printf '\\033[?2031h'; printf 'ready\\n'; \
                  r=$(dd bs=1 count=9 2>/dev/null | od -An -c | tr -d ' '); echo \"push=$r\"";
    cfg.program = Some(("/bin/sh".into(), vec!["-c".into(), script.into()]));
    let mut t = terminals();
    let id = t.spawn(cfg).unwrap();
    wait_for(&t, id, "ready", |f| f.contains_line("ready"));
    let mut light = theme();
    light.light = true;
    t.set_theme(id, light);
    wait_for(&t, id, "pushed light report", |f| f.contains_line("push=033[?997;2n"));
    close_all(t);
}

mod hidden;
mod lifecycle;
mod load;
mod resize;
mod wake;
