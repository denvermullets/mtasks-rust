//! Attention signals from real output: program status (OSC 7501), desktop
//! notifications (OSC 9) and the bell reach `take_signals`, hidden or not.
use super::*;

fn script(cmd: &str) -> SpawnConfig {
    let mut cfg = sh_config("t1");
    cfg.program = Some(("/bin/sh".into(), vec!["-c".into(), cmd.into()]));
    cfg
}

/// Poll `take_signals` until `n` signals came in, or panic after 10 s.
fn wait_signals(t: &Terminals, n: usize) -> Vec<(TerminalId, Signal)> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut got = Vec::new();
    while got.len() < n {
        got.extend(t.take_signals());
        assert!(Instant::now() < deadline, "timed out; got {got:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    got
}

#[test]
fn status_reports_notifications_and_bells_come_through_in_order() {
    let mut t = terminals();
    // `title` and `msg` are base64 ("Fix login", "Apply?"), as the protocol has it.
    let id = t
        .spawn(script(
            r"printf '\033]7501;state=blocked:kind=permission:app=claude-code:title=Rml4IGxvZ2lu:msg=QXBwbHk/\033\\'; \
              printf '\033]9;Build done\007'; printf '\007'; sleep 30",
        ))
        .unwrap();
    let got = wait_signals(&t, 3);
    let signals: Vec<Signal> = got
        .into_iter()
        .map(|(from, s)| {
            assert_eq!(from, id);
            s
        })
        .collect();
    assert_eq!(
        signals,
        [
            Signal::Status {
                state: ProgramState::Blocked(BlockKind::Permission),
                app: "claude-code".into(),
                title: "Fix login".into(),
                message: "Apply?".into(),
            },
            Signal::Notification {
                title: String::new(),
                body: "Build done".into()
            },
            Signal::Bell,
        ]
    );
    assert!(t.take_signals().is_empty(), "taken once");
    close_all(t);
}

#[test]
fn a_hidden_terminal_still_wakes_the_ui_for_a_signal() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    let mut t = Terminals::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    });
    let id = t
        .spawn(script(
            r"sleep 1; printf '\033]7501;state=done\033\\'; sleep 30",
        ))
        .unwrap();
    t.set_visible(id, false);
    std::thread::sleep(Duration::from_millis(300));
    t.ack_wake();
    let before = wakes.load(Ordering::SeqCst);
    let got = wait_signals(&t, 1);
    assert!(matches!(
        &got[0].1,
        Signal::Status {
            state: ProgramState::Done,
            ..
        }
    ));
    assert!(wakes.load(Ordering::SeqCst) > before, "the UI was woken");
    close_all(t);
}

#[test]
fn the_terminal_answers_the_program_status_probe() {
    // Claude Code asks `OSC 7501 ; ?` at startup and only reports status
    // when the terminal answers. Read the answer back and show it with ESC
    // spelled `E`.
    let mut t = terminals();
    let id = t
        .spawn(script(
            r"stty -echo; printf '\033]7501;?\033\\'; IFS= read -r -t 3 -d '\' reply; \
              printf 'reply:%s\n' $(printf '%s' $reply | tr '\033' E); sleep 30",
        ))
        .unwrap();
    wait_for_secs(10, &t, id, "the probe's answer", |f| {
        f.lines()
            .iter()
            .any(|l| l.starts_with("reply:E]7501;?") && l.contains("blocked"))
    });
    close_all(t);
}
