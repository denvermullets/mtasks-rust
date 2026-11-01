//! Memoized parsed bodies (JRST-266).
//!
//! View code runs on every message, so a body is parsed once and reused.
//! Mentions are tokenized at parse time against the member names, so the key
//! is body, host AND member names: a member change is a different key and
//! shows at once, with no invalidation. References resolve at draw time and
//! need no part in the key. The cache is bounded; the least recently used
//! entry goes first.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::document::{self, Block, Document};
use crate::highlight::{self, Span};

/// Most bodies kept. A long thread fits; memory stays bounded.
pub const CAPACITY: usize = 256;

/// Highlighted runs per code block, keyed by (language, text).
type CodeMap = HashMap<(Option<String>, String), Vec<Span>>;

/// A parsed body plus the highlighted output of its code blocks (JRST-282).
pub struct Parsed {
    pub doc: Document,
    code: CodeMap,
}

impl Parsed {
    /// Highlighted runs for a code block of this document.
    pub fn code_spans(&self, lang: Option<&str>, text: &str) -> Option<&[Span]> {
        self.code
            .get(&(lang.map(str::to_string), text.to_string()))
            .map(Vec::as_slice)
    }
}

fn build(body: &str, host: &str, names: &[&str]) -> Parsed {
    let doc = document::parse_with(body, host, names);
    let mut code = CodeMap::new();
    collect_code(&doc.blocks, &mut code);
    Parsed { doc, code }
}

fn collect_code(blocks: &[Block], out: &mut CodeMap) {
    for b in blocks {
        match b {
            Block::Code(c) => {
                out.entry((c.lang.clone(), c.text.clone()))
                    .or_insert_with(|| highlight::highlight(c.lang.as_deref(), &c.text));
            }
            Block::List(l) => l.items.iter().for_each(|i| collect_code(i, out)),
            Block::Quote(inner) => collect_code(inner, out),
            Block::Paragraph(_) | Block::Image(_) => {}
        }
    }
}

type Key = (String, String, Vec<String>);

/// A size-bounded, least-recently-used map.
struct Lru<V> {
    cap: usize,
    tick: u64,
    map: HashMap<Key, (u64, Arc<V>)>,
}

impl<V> Lru<V> {
    fn new(cap: usize) -> Self {
        Lru { cap, tick: 0, map: HashMap::new() }
    }

    fn get_or_insert_with(&mut self, key: Key, make: impl FnOnce() -> V) -> Arc<V> {
        self.tick += 1;
        if let Some((used, v)) = self.map.get_mut(&key) {
            *used = self.tick;
            return v.clone();
        }
        if self.map.len() >= self.cap {
            if let Some(oldest) = self.map.iter().min_by_key(|(_, (t, _))| *t).map(|(k, _)| k.clone()) {
                self.map.remove(&oldest);
            }
        }
        let v = Arc::new(make());
        self.map.insert(key, (self.tick, v.clone()));
        v
    }
}

/// The parsed body for `body`, parsing it only if this exact body, host and
/// member-name list hasn't been seen recently.
pub fn parsed(body: &str, host: &str, member_names: &[&str]) -> Arc<Parsed> {
    static CACHE: OnceLock<Mutex<Lru<Parsed>>> = OnceLock::new();
    let key = (
        body.to_string(),
        host.to_string(),
        member_names.iter().map(|s| s.to_string()).collect(),
    );
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(Lru::new(CAPACITY)))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    cache.get_or_insert_with(key, || build(body, host, member_names))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn key(body: &str, names: &[&str]) -> Key {
        (body.into(), "h".into(), names.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn an_unchanged_key_is_not_rebuilt_and_a_changed_one_is() {
        let builds = Cell::new(0);
        let mut c = Lru::new(8);
        // (body, names, expected total builds after this call)
        let rows = [
            ("a", vec!["Ann"], 1),
            ("a", vec!["Ann"], 1),
            ("b", vec!["Ann"], 2),
            ("a", vec!["Ann", "Bob"], 3),
            ("a", vec!["Ann", "Bob"], 3),
            ("a", vec!["Ann"], 3),
        ];
        for (body, names, want) in rows {
            c.get_or_insert_with(key(body, &names), || {
                builds.set(builds.get() + 1);
            });
            assert_eq!(builds.get(), want, "{body} {names:?}");
        }
    }

    #[test]
    fn the_cache_never_exceeds_its_limit_and_evicts_the_least_recently_used() {
        let mut c = Lru::new(3);
        for b in ["a", "b", "c"] {
            c.get_or_insert_with(key(b, &[]), || b.to_string());
        }
        c.get_or_insert_with(key("a", &[]), || unreachable!()); // refresh a
        c.get_or_insert_with(key("d", &[]), || "d".to_string()); // evicts b
        assert_eq!(c.map.len(), 3);
        assert!(c.map.contains_key(&key("a", &[])));
        assert!(!c.map.contains_key(&key("b", &[])));
        for i in 0..50 {
            c.get_or_insert_with(key(&format!("x{i}"), &[]), String::new);
            assert!(c.map.len() <= 3);
        }
    }

    #[test]
    fn an_edited_body_shows_its_new_content() {
        let a = parsed("first", "", &[]);
        let b = parsed("second", "", &[]);
        assert_ne!(a.doc.blocks, b.doc.blocks);
        assert_eq!(parsed("first", "", &[]).doc.blocks, a.doc.blocks);
    }

    #[test]
    fn code_blocks_are_highlighted_with_the_parsed_body() {
        let p = parsed("> ```rust\n> fn a() {}\n> ```\n\n```text\nfn a() {}\n```", "", &[]);
        let rust = p.code_spans(Some("rust"), "fn a() {}").expect("nested block cached");
        assert!(rust.iter().any(|s| s.role != highlight::Role::Plain));
        let text = p.code_spans(Some("text"), "fn a() {}").expect("cached");
        assert!(text.iter().all(|s| s.role == highlight::Role::Plain));
        assert!(p.code_spans(Some("rust"), "other").is_none());
    }

    #[test]
    fn a_member_change_retokenizes_mentions() {
        let without = parsed("hi @Ann", "", &[]);
        let with = parsed("hi @Ann", "", &["Ann"]);
        assert_ne!(without.doc.blocks, with.doc.blocks);
    }
}
