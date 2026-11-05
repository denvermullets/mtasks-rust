//! Keys and paste reaching a real shell (JRST-380, 381, 382).
use super::*;

fn ctrl(c: char) -> KeyInput {
    KeyInput {
        key: TermKey::Char(c),
        mods: KeyMods { ctrl: true, ..KeyMods::default() },
        text: None,
        option_as_alt: true,
    }
}

#[test]
fn ctrl_c_interrupts_a_running_program() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "echo started; sleep 60");
    wait_for(&t, id, "started", |f| f.contains_line("started"));
    t.send_key(id, ctrl('c'));
    type_line(&t, id, "echo $((20+22))");
    wait_for(&t, id, "42 before the sleep ends", |f| f.contains_line("42"));
    close_all(t);
}
