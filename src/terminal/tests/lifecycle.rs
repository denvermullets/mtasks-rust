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
