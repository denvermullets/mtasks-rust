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

/// What `cat -v` echoes for one key followed by Enter.
fn cat_v_shows(key: KeyInput) -> String {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "stty -icanon -echo -isig; cat -v");
    // stty can discard input typed before it ran; let cat start first.
    std::thread::sleep(Duration::from_millis(700));
    t.send_key(id, key);
    t.send_key(id, enter());
    let f = wait_for(&t, id, "echoed key", |f| {
        f.lines().iter().any(|l| l.starts_with('^'))
    });
    let line = f.lines().into_iter().find(|l| l.starts_with('^')).unwrap();
    close_all(t);
    line
}

#[test]
fn a_paste_reaches_the_shell() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    t.paste(id, "echo pasted-$((6*7))\n".to_string());
    wait_for(&t, id, "pasted output", |f| f.contains_line("pasted-42"));
    close_all(t);
}

#[test]
fn a_paste_is_bracketed_once_the_program_asks_for_it() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "stty -icanon -echo; printf '\\033[?2004h'; cat -v");
    std::thread::sleep(Duration::from_millis(700));
    t.paste(id, "one\ntwo".to_string());
    let f = wait_for(&t, id, "bracketed paste", |f| {
        f.lines().iter().any(|l| l.contains("^[[200~one"))
    });
    assert!(f.lines().iter().any(|l| l.contains("two^[[201~")), "{:?}", f.lines());
    close_all(t);
}

#[test]
fn an_empty_paste_still_brackets_so_the_program_can_read_an_image() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "stty -icanon -echo; printf '\\033[?2004h'; cat -v");
    std::thread::sleep(Duration::from_millis(700));
    t.paste(id, String::new());
    wait_for(&t, id, "empty bracketed paste", |f| {
        f.lines().iter().any(|l| l.contains("^[[200~^[[201~"))
    });
    close_all(t);
}

#[test]
fn a_job_keeps_running_while_the_terminal_is_hidden() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "sleep 1; echo job-finished");
    t.set_visible(id, false);
    std::thread::sleep(Duration::from_millis(2500));
    t.set_visible(id, true);
    wait_for(&t, id, "the job's output on return", |f| f.contains_line("job-finished"));
    close_all(t);
}

fn routed(code: iced::keyboard::key::Code, mods: iced::keyboard::Modifiers) -> KeyInput {
    use crate::terminal::input::{route, Route};
    match route(code, mods, None) {
        Route::Send(k) => k,
        other => panic!("expected a key to send, got {other:?}"),
    }
}

#[test]
fn editing_keys_reach_the_shell_as_control_and_escape_sequences() {
    use iced::keyboard::{key::Code, Modifiers};
    assert_eq!(cat_v_shows(routed(Code::Backspace, Modifiers::LOGO)), "^U");
    assert_eq!(cat_v_shows(routed(Code::ArrowLeft, Modifiers::LOGO)), "^A");
    assert_eq!(cat_v_shows(routed(Code::ArrowRight, Modifiers::LOGO)), "^E");
    assert_eq!(cat_v_shows(routed(Code::ArrowLeft, Modifiers::ALT)), "^[b");
    assert_eq!(cat_v_shows(routed(Code::ArrowRight, Modifiers::ALT)), "^[f");
}
