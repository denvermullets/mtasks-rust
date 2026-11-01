//! Syntax highlighting for code blocks (JRST-282).
//!
//! Pure: `highlight(lang, text)` splits `text` into runs tagged with a color
//! [`Role`]; the renderer maps roles to colors. Roles follow the web's Monokai
//! palette. The syntax set takes tens of milliseconds to deserialize, so
//! [`preload`] builds it on a background thread at startup. If a code block
//! is drawn first, `highlight` initializes it on the spot (the `OnceLock`
//! makes the two safe: whoever is second waits for the first). Results are
//! cached with the parsed body (`body_cache`).

use std::sync::OnceLock;

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxSet};

/// A color role inside a code block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Plain,
    Comment,
    Keyword,
    String,
    Number,
    Function,
    Type,
    Parameter,
}

/// A run of code text with one role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub role: Role,
}

/// Start building the syntax set and scope rules on a background thread, so
/// the first code block doesn't stall `view`. Safe to skip or race with
/// `highlight`; the handle is only for tests.
pub fn preload() -> std::thread::JoinHandle<()> {
    std::thread::spawn(|| {
        syntaxes();
        rules();
    })
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Scope prefixes mapped to roles; the innermost scope on the stack that
/// matches any rule decides the role.
fn rules() -> &'static [(Scope, Role)] {
    static RULES: OnceLock<Vec<(Scope, Role)>> = OnceLock::new();
    RULES.get_or_init(|| {
        [
            ("comment", Role::Comment),
            ("string", Role::String),
            ("constant", Role::Number),
            ("variable.parameter", Role::Parameter),
            ("entity.name.function", Role::Function),
            ("support.function", Role::Function),
            ("entity.name.class", Role::Function),
            ("entity.other.inherited-class", Role::Function),
            ("storage.type.function", Role::Keyword),
            ("storage.type.class", Role::Keyword),
            ("storage.type", Role::Type),
            ("support.type", Role::Type),
            ("support.class", Role::Type),
            ("entity.name.type", Role::Type),
            ("keyword", Role::Keyword),
            ("storage", Role::Keyword),
        ]
        .into_iter()
        .filter_map(|(s, r)| Scope::new(s).ok().map(|s| (s, r)))
        .collect()
    })
}

fn role_of(stack: &ScopeStack) -> Role {
    for scope in stack.as_slice().iter().rev() {
        if let Some((_, r)) = rules().iter().find(|(p, _)| p.is_prefix_of(*scope)) {
            return *r;
        }
    }
    Role::Plain
}

fn plain(text: &str) -> Vec<Span> {
    vec![Span { text: text.to_string(), role: Role::Plain }]
}

fn push(out: &mut Vec<Span>, s: &str, role: Role) {
    if s.is_empty() {
        return;
    }
    match out.last_mut() {
        Some(last) if last.role == role => last.text.push_str(s),
        _ => out.push(Span { text: s.to_string(), role }),
    }
}

/// Split `text` into role-tagged runs. The run texts concatenate back to
/// `text` exactly. A missing or unknown `lang` gives one `Plain` run.
pub fn highlight(lang: Option<&str>, text: &str) -> Vec<Span> {
    let Some(lang) = lang.map(str::trim).filter(|l| !l.is_empty()) else {
        return plain(text);
    };
    let set = syntaxes();
    let Some(syntax) = set.find_syntax_by_token(lang) else {
        return plain(text);
    };
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut out: Vec<Span> = Vec::new();
    let lines: Vec<&str> = text.split('\n').collect();
    let last = lines.len() - 1;
    for (i, line) in lines.iter().enumerate() {
        // The newline-aware syntaxes expect every line to end in `\n`.
        let with_nl = format!("{line}\n");
        let Ok(ops) = state.parse_line(&with_nl, set) else {
            return plain(text);
        };
        let mut at = 0;
        for (pos, op) in ops {
            let to = pos.min(line.len());
            if to > at {
                push(&mut out, &line[at..to], role_of(&stack));
                at = to;
            }
            if stack.apply(&op).is_err() {
                return plain(text);
            }
        }
        push(&mut out, &line[at..], role_of(&stack));
        if i != last {
            push(&mut out, "\n", role_of(&stack));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preload_and_a_racing_highlight_agree() {
        let handle = preload();
        // Reached before the preload finishes (or after): same answer.
        let early = highlight(Some("rust"), "fn main() {}\n");
        handle.join().unwrap();
        assert_eq!(highlight(Some("rust"), "fn main() {}\n"), early);
        assert!(early.iter().any(|s| s.role != Role::Plain));
    }

    fn role_of_text(spans: &[Span], needle: &str) -> Option<Role> {
        spans.iter().find(|s| s.text.trim() == needle).map(|s| s.role)
    }

    #[test]
    fn known_languages_color_their_tokens() {
        // (lang, text, token, role)
        let rows = [
            ("rust", "fn main() { let x = 42; }", "fn", Role::Keyword),
            ("rust", "let s = \"hi\";", "\"hi\"", Role::String),
            ("rust", "let x = 42;", "42", Role::Number),
            ("rust", "// note\nlet x = 1;", "// note", Role::Comment),
            ("ruby", "def hello\n  puts 1\nend", "def", Role::Keyword),
            ("js", "const a = 'x';", "'x'", Role::String),
        ];
        for (lang, text, token, want) in rows {
            let spans = highlight(Some(lang), text);
            assert_eq!(role_of_text(&spans, token), Some(want), "{lang} {text}");
        }
    }

    #[test]
    fn unknown_or_missing_language_is_one_plain_run() {
        for lang in [None, Some(""), Some("  "), Some("klingon-script")] {
            assert_eq!(highlight(lang, "fn a() {}"), plain("fn a() {}"), "{lang:?}");
        }
    }

    #[test]
    fn run_texts_concatenate_back_to_the_input() {
        let texts = [
            "",
            "a",
            "fn a() {\n    b();\n\n\tc();\n}",
            "let s = \"multi\nline\";\n",
            "/* open\ncomment */ x",
            "ABC-12 @Ann ![x](y)\r\nnext",
        ];
        for lang in [Some("rust"), Some("js"), Some("python"), None, Some("nope")] {
            for t in texts {
                let joined: String = highlight(lang, t).into_iter().map(|s| s.text).collect();
                assert_eq!(joined, t, "{lang:?}");
            }
        }
    }
}
