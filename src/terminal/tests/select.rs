//! Drag selection (JRST-385). Cells are 8x16 px until a resize says otherwise.
use super::*;

fn ev(action: MouseAction, col: i32, row: i32) -> MouseInput {
    MouseInput { action, col, row, mods: KeyMods::default() }
}

fn drag(t: &Terminals, id: TerminalId, from: (i32, i32), to: (i32, i32)) {
    t.mouse(id, ev(MouseAction::Press(MouseButton::Left), from.0, from.1));
    t.mouse(id, ev(MouseAction::Move, to.0, to.1));
    t.mouse(id, ev(MouseAction::Release(MouseButton::Left), to.0, to.1));
}

fn row_of(f: &Frame, line: &str) -> i32 {
    f.lines().iter().position(|l| l.trim() == line).unwrap_or_else(|| panic!("no {line} in {:?}", f.lines()))
        as i32
}

/// A terminal showing `one`, `two`, `three` on consecutive rows.
fn with_three_lines() -> (Terminals, TerminalId, Arc<Frame>) {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "printf 'one\\ntwo\\nthree\\n'");
    let f = wait_for(&t, id, "three", |f| f.contains_line("three"));
    (t, id, f)
}

#[test]
fn dragging_selects_text_across_lines_and_draws_it_in_the_selection_colour() {
    let (t, id, f) = with_three_lines();
    let r = row_of(&f, "one");
    drag(&t, id, (1, r), (3, r + 2));
    let f = wait_for(&t, id, "a selection", |f| f.rows.iter().any(|r| r.selected.is_some()));
    assert_eq!(f.rows[r as usize].selected, Some((1, 199)), "first row runs to its end");
    assert_eq!(f.rows[r as usize + 1].selected, Some((0, 199)), "middle rows are whole");
    assert_eq!(f.rows[r as usize + 2].selected, Some((0, 3)), "last row stops at the drag");
    assert_eq!(f.selection_bg, [60; 3], "the theme's selection colour");
    assert_eq!(t.selected_text(id).as_deref(), Some("ne\ntwo\nthre"));
    close_all(t);
}

#[test]
fn a_click_clears_the_selection() {
    let (t, id, f) = with_three_lines();
    let r = row_of(&f, "one");
    drag(&t, id, (0, r), (4, r + 2));
    wait_for(&t, id, "a selection", |f| f.rows.iter().any(|r| r.selected.is_some()));
    t.mouse(id, ev(MouseAction::Press(MouseButton::Left), 10, r));
    t.mouse(id, ev(MouseAction::Release(MouseButton::Left), 10, r));
    wait_for(&t, id, "no selection", |f| f.rows.iter().all(|r| r.selected.is_none()));
    assert_eq!(t.selected_text(id), None);
    close_all(t);
}

#[test]
fn a_drag_past_the_top_scrolls_into_scrollback_and_keeps_selecting() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "seq 1 300");
    let f = wait_for(&t, id, "the last line", |f| f.contains_line("300"));
    let first_on_screen: u32 = f.lines()[0].trim().parse().unwrap();
    let bottom = row_of(&f, "300");
    t.mouse(id, ev(MouseAction::Press(MouseButton::Left), 0, bottom));
    // Held above the pane, the viewport keeps scrolling by itself.
    t.mouse(id, ev(MouseAction::Move, 2, -2));
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let text = t.selected_text(id).unwrap_or_default();
        let first: Option<u32> = text.lines().next().and_then(|l| l.trim().parse().ok());
        if first.is_some_and(|n| n + 20 < first_on_screen) {
            break;
        }
        assert!(Instant::now() < deadline, "selection never reached scrollback: starts {first:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
    t.mouse(id, ev(MouseAction::Release(MouseButton::Left), 2, -2));
    close_all(t);
}

#[test]
fn a_selection_stays_on_its_text_as_output_arrives() {
    let (t, id, f) = with_three_lines();
    let r = row_of(&f, "one");
    drag(&t, id, (0, r), (4, r + 2));
    wait_for(&t, id, "a selection", |f| f.rows.iter().any(|r| r.selected.is_some()));
    type_line(&t, id, "seq 1 100");
    wait_for(&t, id, "output", |f| f.contains_line("100"));
    // The engine's selection follows its text into scrollback.
    assert_eq!(t.selected_text(id).as_deref(), Some("one\ntwo\nthree"));
    close_all(t);
}

#[test]
fn selecting_and_leaving_it_alone_costs_no_wakes() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    let mut t = Terminals::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    });
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "printf 'one\\ntwo\\nthree\\n'");
    let f = wait_for(&t, id, "three", |f| f.contains_line("three"));
    let r = row_of(&f, "one");
    drag(&t, id, (0, r), (4, r + 2));
    wait_for(&t, id, "a selection", |f| f.rows.iter().any(|r| r.selected.is_some()));
    std::thread::sleep(Duration::from_millis(300));
    t.ack_wake();
    let before = wakes.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(wakes.load(Ordering::SeqCst), before, "an idle selection must not wake the UI");
    close_all(t);
}
