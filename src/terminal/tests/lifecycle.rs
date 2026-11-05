//! Naming, busy detection, exit and quit through the public API (JRST-399,
//! 400, 401, 410).
use super::*;

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
