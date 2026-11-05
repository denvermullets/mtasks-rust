//! Naming, busy detection, exit and quit through the public API (JRST-399,
//! 400, 401, 410).
use super::*;

fn wait_busy(t: &Terminals, id: TerminalId, want: bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while t.is_busy(id) != want {
        assert!(Instant::now() < deadline, "is_busy never became {want}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_shell_at_its_prompt_is_not_busy() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    wait_for(&t, id, "a prompt", |f| f.lines().iter().any(|l| l.contains('$')));
    assert!(!t.is_busy(id));
    close_all(t);
}

#[test]
fn a_foreground_program_makes_the_terminal_busy_until_it_ends() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "sleep 100");
    wait_busy(&t, id, true);
    // Ctrl+C reaches the program and the shell is back in front.
    t.send_key(
        id,
        KeyInput {
            key: TermKey::Char('c'),
            mods: KeyMods { ctrl: true, ..KeyMods::default() },
            text: None,
            option_as_alt: true,
        },
    );
    wait_busy(&t, id, false);
    close_all(t);
}

#[test]
fn an_exited_or_unknown_terminal_is_not_busy() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "exit");
    wait_for(&t, id, "exit", |f| f.exited);
    assert!(!t.is_busy(id));
    assert!(!t.is_busy(TerminalId(999)));
    close_all(t);
}

fn pwd_of(cwd: Option<std::path::PathBuf>) -> String {
    let mut t = terminals();
    let mut cfg = sh_config("t1");
    cfg.cwd = cwd;
    let id = t.spawn(cfg).unwrap();
    type_line(&t, id, "echo cwd=$(pwd -P)=end");
    let f = wait_for(&t, id, "the pwd line", |f| {
        f.lines().iter().any(|l| l.starts_with("cwd=") && l.ends_with("=end"))
    });
    let line = f.lines().into_iter().find(|l| l.starts_with("cwd=") && l.ends_with("=end")).unwrap();
    close_all(t);
    line["cwd=".len()..line.len() - "=end".len()].to_string()
}

#[test]
fn a_shell_starts_in_the_given_directory() {
    let dir = std::env::temp_dir().join(format!("mtasks-cwd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let want = dir.canonicalize().unwrap();
    assert_eq!(pwd_of(Some(dir.clone())), want.to_string_lossy());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_missing_directory_falls_back_to_home_and_the_shell_still_starts() {
    let gone = std::env::temp_dir().join(format!("mtasks-gone-{}", std::process::id()));
    assert!(!gone.exists());
    let home = dirs::home_dir().unwrap().canonicalize().unwrap();
    assert_eq!(pwd_of(Some(gone)), home.to_string_lossy());
    assert_eq!(pwd_of(None), home.to_string_lossy());
}

#[test]
fn a_program_setting_its_title_does_not_change_the_terminals_name() {
    let mut t = terminals();
    let mut roster = roster::Roster::default();
    let id = t.spawn(sh_config("t1")).unwrap();
    roster.add(id, "t1", None);
    type_line(&t, id, r"printf '\033]0;vim: notes\007'");
    let f = wait_for(&t, id, "the title", |f| f.title == "vim: notes");
    assert_eq!(f.title, "vim: notes");
    assert_eq!(roster.get(id).unwrap().name, "Terminal 1");
    close_all(t);
}

fn spawn_idle(t: &mut Terminals, team: &str) -> (TerminalId, u32) {
    let id = t.spawn(sh_config(team)).unwrap();
    wait_for(t, id, "a prompt", |f| f.lines().iter().any(|l| l.contains('$')));
    (id, t.pid(id).unwrap())
}

#[test]
fn a_busy_terminal_on_any_team_makes_the_set_busy_hidden_or_not() {
    let mut t = terminals();
    let (_a, _) = spawn_idle(&mut t, "team-a");
    let (b, _) = spawn_idle(&mut t, "team-b");
    assert!(!t.any_busy());
    t.set_visible(b, false);
    type_line(&t, b, "sleep 100");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !t.any_busy() {
        assert!(Instant::now() < deadline, "never busy");
        std::thread::sleep(Duration::from_millis(20));
    }
    close_all(t);
}

#[test]
fn closing_everything_is_prompt_when_the_shells_exit_quickly() {
    let mut t = terminals();
    let pids: Vec<u32> = (0..6).map(|n| spawn_idle(&mut t, &format!("team-{}", n % 3)).1).collect();
    let started = Instant::now();
    let closings = t.close_all();
    assert_eq!(closings.len(), 6);
    let reports = wait_all(closings, Duration::from_secs(8));
    // Well under the 2 second SIGKILL grace.
    assert!(started.elapsed() < Duration::from_millis(1500), "took {:?}", started.elapsed());
    assert_eq!(reports.len(), 6);
    assert!(reports.iter().all(|r| r.reaped && !r.escalated_to_sigkill && r.threads_exited == 3));
    assert!(pids.iter().all(|&p| !process_exists(p)));
    assert!(t.ids().is_empty());
}

#[test]
fn a_panicked_owner_does_not_hold_up_closing_everything() {
    let mut t = terminals();
    let (doomed, doomed_pid) = spawn_idle(&mut t, "t1");
    let (_ok, ok_pid) = spawn_idle(&mut t, "t1");
    t.crash_owner(doomed);
    wait_for(&t, doomed, "dead", |f| f.dead);
    let started = Instant::now();
    let reports = wait_all(t.close_all(), Duration::from_secs(8));
    assert!(started.elapsed() < Duration::from_millis(1500), "took {:?}", started.elapsed());
    assert_eq!(reports.len(), 2);
    assert!(!process_exists(doomed_pid) && !process_exists(ok_pid));
}

#[test]
fn waiting_gives_up_at_the_limit_instead_of_hanging() {
    let mut t = terminals();
    // Ignores SIGHUP, so the close takes the full 2 second grace.
    let mut cfg = sh_config("t1");
    cfg.program = Some(("/bin/sh".into(), vec!["-c".into(), "trap '' HUP; echo up; while :; do sleep 1; done".into()]));
    let id = t.spawn(cfg).unwrap();
    wait_for(&t, id, "up", |f| f.contains_line("up"));
    let started = Instant::now();
    let reports = wait_all(t.close_all(), Duration::from_millis(300));
    assert!(started.elapsed() < Duration::from_millis(1500));
    assert!(reports.is_empty(), "not finished yet, so no report");
}

#[test]
fn a_failure_after_the_shell_starts_kills_and_reaps_it() {
    use crate::terminal::engine::seam;
    let mut t = terminals();
    seam::FAIL_AFTER_START.with(|f| f.set(true));
    let result = t.spawn(sh_config("t1"));
    seam::FAIL_AFTER_START.with(|f| f.set(false));
    assert!(result.is_err());
    let pid = seam::LAST_PID.with(|p| p.get()) as libc::pid_t;
    assert!(pid > 0, "the shell did start");
    // Reaped, not just signalled: a zombie would still answer signal 0.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "the shell is gone");
    assert_eq!(t.open_count("t1"), 0);
}
