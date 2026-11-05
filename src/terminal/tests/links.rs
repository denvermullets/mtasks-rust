//! Links in real output (JRST-388): what the engine reports about wrapping and OSC 8.
use super::*;

const URL: &str = "https://example.com/some/long/path/that/wraps";

#[test]
fn a_wrapped_address_is_found_from_either_row() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    t.resize(id, 20, 10, 8.0, 16.0);
    type_line(&t, id, &format!("echo {URL}"));
    let f = wait_for(&t, id, "the wrapped address", |f| f.contains_line("wraps"));
    let first = f.lines().iter().position(|l| l.starts_with("https://")).unwrap();
    assert!(f.rows[first].wrapped, "the engine says the row soft-wraps");
    for (row, col) in [(first, 3u16), (first + 1, 5), (first + 2, 1)] {
        let link = f.link_at(row, col).unwrap_or_else(|| panic!("no link at {row},{col}: {:?}", f.lines()));
        assert_eq!(link.url, URL);
        assert!(!link.explicit);
    }
    assert_eq!(f.link_at(first + 2, 10), None, "past the end of the address");
    close_all(t);
}

#[test]
fn an_osc_8_link_reports_its_target_not_its_text() {
    let mut t = terminals();
    let id = t.spawn(sh_config("t1")).unwrap();
    type_line(&t, id, "printf '\\033]8;;https://real.example/x\\033\\\\click me\\033]8;;\\033\\\\ after\\n'");
    let f = wait_for(&t, id, "the link text", |f| f.contains_line("click me after"));
    let row = f.lines().iter().position(|l| l == "click me after").unwrap();
    let link = f.link_at(row, 2).expect("link under the text");
    assert_eq!(link.url, "https://real.example/x");
    assert!(link.explicit);
    assert_eq!(link.spans, vec![(row, 0, 8)]);
    assert_eq!(f.link_at(row, 10), None, "text after the link isn't part of it");
    close_all(t);
}
