//! Links in Terminal output (JRST-388), with no Iced and no engine.
//!
//! Two kinds: web addresses found in the visible text, which may wrap over
//! several rows, and explicit OSC 8 links the program marked itself (the engine
//! puts those in `RowData::links`). Only some are safe to open: see `openable`.
use std::sync::Arc;

use super::RowData;

/// A link under the pointer, with the cells to underline.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub url: String,
    /// The program marked this link itself (OSC 8) rather than us finding it in text.
    pub explicit: bool,
    /// `(row, col, len)` pieces, one per row the link covers.
    pub spans: Vec<(usize, u16, u16)>,
}

const MAX_URL: usize = 2048;

/// Whether `url` may be handed to macOS `open`. Web addresses found in text
/// must be http or https. An explicit OSC 8 link may also be `mailto:`. Anything
/// else (file, ssh, javascript, an app's own scheme) could launch a program or
/// reveal a file just because output contained it, so it never opens.
pub fn openable(url: &str, explicit: bool) -> bool {
    if url.is_empty() || url.len() > MAX_URL || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let Some((scheme, rest)) = url.split_once(':') else {
        return false;
    };
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" => rest
            .strip_prefix("//")
            .is_some_and(|host| host.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '[')),
        "mailto" => explicit && !rest.is_empty(),
        _ => false,
    }
}

/// Open a link with macOS `open`, if it is safe to. Returns whether it was started.
pub fn open(url: &str, explicit: bool) -> bool {
    openable(url, explicit)
        && std::process::Command::new("open")
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
}

/// One character per cell of a row. Cells holding anything but ASCII are `'\0'`,
/// which ends a web address.
fn cells(row: &RowData, cols: usize) -> Vec<char> {
    let mut out = vec![' '; cols];
    for run in &row.runs {
        let at = run.col as usize;
        if run.ascii {
            for (i, ch) in run.text.chars().enumerate() {
                if let Some(cell) = out.get_mut(at + i) {
                    *cell = ch;
                }
            }
        } else {
            for cell in out.iter_mut().skip(at).take(run.cells as usize) {
                *cell = '\0';
            }
        }
    }
    out
}

fn is_word_before(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_url_end(c: char) -> bool {
    c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | '"' | '`' | '\0')
}

/// Where the web address starting at `start` ends (exclusive), or `None` if it
/// isn't one. Sentence punctuation and unbalanced closing brackets stay out.
fn url_end(chars: &[char], start: usize) -> Option<usize> {
    let lower: String = chars[start..].iter().take(8).map(|c| c.to_ascii_lowercase()).collect();
    let scheme_len = if lower.starts_with("https://") {
        8
    } else if lower.starts_with("http://") {
        7
    } else {
        return None;
    };
    let mut end = start + scheme_len;
    while end < chars.len() && !is_url_end(chars[end]) {
        end += 1;
    }
    loop {
        if end <= start + scheme_len {
            return None;
        }
        let last = chars[end - 1];
        let body = &chars[start..end];
        let count = |c: char| body.iter().filter(|x| **x == c).count();
        let opener = match last {
            ')' => Some('('),
            ']' => Some('['),
            '}' => Some('{'),
            _ => None,
        };
        let unbalanced = opener.is_some_and(|o| count(o) < count(last));
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '\'') || unbalanced {
            end -= 1;
        } else {
            break;
        }
    }
    chars[start + scheme_len].is_ascii_alphanumeric().then_some(end).or_else(|| {
        // Allow an IPv6 host.
        (chars[start + scheme_len] == '[').then_some(end)
    })
}

impl super::Frame {
    /// The link at a cell of this frame, if any.
    pub fn link_at(&self, row: usize, col: u16) -> Option<Link> {
        link_at(&self.rows, self.cols, row, col)
    }
}

/// The link at a cell of the visible rows, if any. `cols` is the grid width.
pub fn link_at(rows: &[Arc<RowData>], cols: u16, row: usize, col: u16) -> Option<Link> {
    let cols = cols as usize;
    let here = rows.get(row)?;
    if let Some(span) = here.links.iter().find(|l| col >= l.col && col < l.col + l.len) {
        let spans = rows
            .iter()
            .enumerate()
            .flat_map(|(r, data)| {
                data.links.iter().filter(|l| l.uri == span.uri).map(move |l| (r, l.col, l.len))
            })
            .collect();
        return Some(Link { url: span.uri.clone(), explicit: true, spans });
    }
    // A web address can wrap: join the rows that run into each other.
    let mut first = row;
    while first > 0 && rows[first - 1].wrapped {
        first -= 1;
    }
    let mut last = row;
    while last + 1 < rows.len() && rows[last].wrapped {
        last += 1;
    }
    let chars: Vec<char> = rows[first..=last].iter().flat_map(|r| cells(r, cols)).collect();
    let at = (row - first) * cols + col as usize;
    let mut i = 0;
    while i < chars.len() {
        if i > 0 && is_word_before(chars[i - 1]) {
            i += 1;
            continue;
        }
        match url_end(&chars, i) {
            Some(end) if at >= i && at < end => {
                let mut spans = Vec::new();
                let mut idx = i;
                while idx < end {
                    let (r, c) = (idx / cols, idx % cols);
                    let len = (cols - c).min(end - idx);
                    spans.push((first + r, c as u16, len as u16));
                    idx += len;
                }
                return Some(Link {
                    url: chars[i..end].iter().collect(),
                    explicit: false,
                    spans,
                });
            }
            Some(end) => i = end,
            None => i += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{LinkSpan, Run};

    fn row(text: &str, wrapped: bool) -> Arc<RowData> {
        Arc::new(RowData {
            runs: vec![Run {
                col: 0,
                text: text.into(),
                cells: text.chars().count() as u16,
                fg: [0; 3],
                bold: false,
                italic: false,
                faint: false,
                underline: false,
                strike: false,
                ascii: true,
            }],
            wrapped,
            ..Default::default()
        })
    }

    fn url_at(text: &str, col: u16) -> Option<String> {
        link_at(&[row(text, false)], 80, 0, col).map(|l| l.url)
    }

    #[test]
    fn a_web_address_is_found_under_any_of_its_cells_and_nowhere_else() {
        let line = "see https://example.com/a?b=1 now";
        assert_eq!(url_at(line, 4).as_deref(), Some("https://example.com/a?b=1"));
        assert_eq!(url_at(line, 28).as_deref(), Some("https://example.com/a?b=1"));
        assert_eq!(url_at(line, 3), None);
        assert_eq!(url_at(line, 29), None);
        assert_eq!(url_at(line, 0), None);
    }

    #[test]
    fn sentence_punctuation_and_unbalanced_brackets_stay_out_of_the_address() {
        assert_eq!(url_at("go to http://a.io/x.", 8).as_deref(), Some("http://a.io/x"));
        assert_eq!(url_at("(see https://a.io/x)", 8).as_deref(), Some("https://a.io/x"));
        assert_eq!(url_at("https://a.io/wiki/Foo_(bar)", 3).as_deref(), Some("https://a.io/wiki/Foo_(bar)"));
        assert_eq!(url_at("<https://a.io/x>, ok", 3).as_deref(), Some("https://a.io/x"));
        assert_eq!(url_at("\"https://a.io/x\"", 3).as_deref(), Some("https://a.io/x"));
    }

    #[test]
    fn a_wrapped_address_is_one_link_underlined_on_both_rows() {
        let rows = [row("see https://example.com/very", true), row("/long/path ok", false)];
        let from_second = link_at(&rows, 28, 1, 2).unwrap();
        assert_eq!(from_second.url, "https://example.com/very/long/path");
        assert_eq!(from_second.spans, vec![(0, 4, 24), (1, 0, 10)]);
        let from_first = link_at(&rows, 28, 0, 5).unwrap();
        assert_eq!(from_first, from_second);
    }

    #[test]
    fn rows_that_do_not_wrap_into_each_other_are_not_joined() {
        let rows = [row("https://example.com/very", false), row("/long/path", false)];
        assert_eq!(link_at(&rows, 24, 0, 3).unwrap().url, "https://example.com/very");
        assert_eq!(link_at(&rows, 24, 1, 3), None);
    }

    #[test]
    fn other_schemes_and_bare_words_are_not_links() {
        for text in ["ftp://a.io/x", "file:///etc/passwd", "javascript:alert(1)", "http:// nothing", "xhttps://a.io"] {
            assert_eq!(url_at(text, 3), None, "{text}");
        }
        assert_eq!(url_at("HTTPS://A.IO/x", 3).as_deref(), Some("HTTPS://A.IO/x"));
    }

    #[test]
    fn an_address_ends_at_a_wide_character() {
        let wide = Arc::new(RowData {
            runs: vec![
                Run { col: 0, text: "http://a.io/x".into(), cells: 13, ascii: true, ..plain() },
                Run { col: 13, text: "日".into(), cells: 2, ascii: false, ..plain() },
            ],
            ..Default::default()
        });
        assert_eq!(link_at(&[wide], 80, 0, 3).unwrap().url, "http://a.io/x");
    }

    fn plain() -> Run {
        Run {
            col: 0,
            text: String::new(),
            cells: 0,
            fg: [0; 3],
            bold: false,
            italic: false,
            faint: false,
            underline: false,
            strike: false,
            ascii: true,
        }
    }

    #[test]
    fn an_explicit_link_wins_over_the_text_under_it() {
        let data = Arc::new(RowData {
            runs: row("a click here", false).runs.clone(),
            links: vec![LinkSpan { col: 2, len: 5, uri: "https://real.example/".into() }],
            ..Default::default()
        });
        let l = link_at(&[data], 80, 0, 4).unwrap();
        assert_eq!(l.url, "https://real.example/");
        assert!(l.explicit);
        assert_eq!(l.spans, vec![(0, 2, 5)]);
        assert_eq!(link_at(&[row("a click here", false)], 80, 0, 4), None);
    }

    #[test]
    fn only_http_and_https_open_unless_the_program_marked_a_safe_link() {
        assert!(openable("http://a.io", false));
        assert!(openable("https://a.io/x?y=1#z", false));
        assert!(openable("https://a.io", true));
        // Not web addresses.
        for url in ["file:///etc/passwd", "ssh://host", "javascript:alert(1)", "ftp://a.io", "x-apple.systempreferences:", "vscode://open", "data:text/html,hi", "open", ""] {
            assert!(!openable(url, false), "{url}");
            assert!(!openable(url, true), "{url} (explicit)");
        }
        // mailto is allowed only when the program marked it.
        assert!(!openable("mailto:a@b.io", false));
        assert!(openable("mailto:a@b.io", true));
        assert!(!openable("mailto:", true));
        // Whitespace and control characters never get through.
        assert!(!openable("https://a.io/x y", true));
        assert!(!openable("https://a.io/\n-a", true));
        assert!(!openable("https://", false));
        assert!(!openable(&format!("https://a.io/{}", "x".repeat(3000)), false));
    }
}
