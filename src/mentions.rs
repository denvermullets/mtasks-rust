//! `@user` mentions and `IDENT-123` issue references (JAIT-212).
//!
//! Rails does the server-side work (creating `IssueReference` rows and mention
//! notifications) by parsing plain text, so the desktop only has to put the
//! right tokens into descriptions/comments. Formats mirror
//! `IssueReferenceParser` and `UserMentionParser` upstream:
//!
//! * Issue reference: `\b([A-Za-z0-9]{3,4}-\d+)\b` (a leading `#` is tolerated
//!   but not required; the web client inserts the bare identifier). Only the
//!   issue's own team prefix resolves.
//! * Mention: `(?<!\w)@<member name>(?!\w)`, case-insensitive, longest name wins.
//!
//! Everything here is pure so it can be unit tested without iced or SQLite.

use crate::db::models::{Comment, Issue, Member, Team};

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

// ── Issue-reference tokens ──

/// One `IDENT-123` occurrence. Byte offsets into the scanned text; `start`
/// includes a directly preceding `#` when present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueToken {
    pub start: usize,
    pub end: usize,
    pub prefix: String,
    pub number: i64,
}

/// Find every issue-reference token in `text` (any prefix; callers filter by team).
pub fn find_issue_tokens(text: &str) -> Vec<IssueToken> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let n = chars.len();
    let byte_at = |i: usize| if i < n { chars[i].0 } else { text.len() };
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let at_boundary = i == 0 || !is_word(chars[i - 1].1);
        if at_boundary && chars[i].1.is_ascii_alphanumeric() {
            let mut k = 0;
            while i + k < n && chars[i + k].1.is_ascii_alphanumeric() {
                k += 1;
            }
            if (3..=4).contains(&k) && i + k < n && chars[i + k].1 == '-' {
                let mut e = i + k + 1;
                while e < n && chars[e].1.is_ascii_digit() {
                    e += 1;
                }
                let has_digits = e > i + k + 1;
                if has_digits && (e == n || !is_word(chars[e].1)) {
                    let digits = &text[byte_at(i + k + 1)..byte_at(e)];
                    if let Ok(number) = digits.parse::<i64>() {
                        let hash = i > 0 && chars[i - 1].1 == '#';
                        out.push(IssueToken {
                            start: if hash { chars[i - 1].0 } else { byte_at(i) },
                            end: byte_at(e),
                            prefix: text[byte_at(i)..byte_at(i + k)].to_string(),
                            number,
                        });
                    }
                    i = e;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// Team issue numbers referenced in `text` for the team `team_identifier`
/// (case-insensitive prefix match), deduplicated in first-seen order.
pub fn referenced_numbers(text: &str, team_identifier: &str) -> Vec<i64> {
    let mut seen = Vec::new();
    for tok in find_issue_tokens(text) {
        if tok.prefix.eq_ignore_ascii_case(team_identifier) && !seen.contains(&tok.number) {
            seen.push(tok.number);
        }
    }
    seen
}

// ── Mentions ──

/// One `@Name` occurrence (byte offsets, `start` at the `@`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionToken {
    pub start: usize,
    pub end: usize,
}

fn eq_ci(a: char, b: char) -> bool {
    a.to_lowercase().eq(b.to_lowercase())
}

/// Find `@Name` occurrences for any of `names`. Longest names are tried first
/// so "Ann Lee" beats "Ann".
pub fn find_mentions(text: &str, names: &[&str]) -> Vec<MentionToken> {
    let mut sorted: Vec<&str> = names.iter().copied().filter(|n| !n.is_empty()).collect();
    sorted.sort_by_key(|n| std::cmp::Reverse(n.chars().count()));
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if chars[i].1 == '@' && (i == 0 || !is_word(chars[i - 1].1)) {
            let matched = sorted.iter().find_map(|name| {
                let nc: Vec<char> = name.chars().collect();
                if i + 1 + nc.len() > n {
                    return None;
                }
                let ok = nc
                    .iter()
                    .enumerate()
                    .all(|(j, c)| eq_ci(chars[i + 1 + j].1, *c));
                let after = i + 1 + nc.len();
                (ok && (after == n || !is_word(chars[after].1))).then_some(after)
            });
            if let Some(after) = matched {
                out.push(MentionToken {
                    start: chars[i].0,
                    end: if after < n {
                        chars[after].0
                    } else {
                        text.len()
                    },
                });
                i = after;
                continue;
            }
        }
        i += 1;
    }
    out
}

// ── Rendering segments ──

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment<'a> {
    Text(&'a str),
    /// Issue reference; `number` is set only when the prefix is the team's.
    Issue {
        text: &'a str,
        number: Option<i64>,
    },
    Mention(&'a str),
}

/// Split `text` into plain/issue/mention segments for display.
pub fn segments<'a>(
    text: &'a str,
    team_identifier: &str,
    member_names: &[&str],
) -> Vec<Segment<'a>> {
    enum Kind {
        Issue(Option<i64>),
        Mention,
    }
    let mut spans: Vec<(usize, usize, Kind)> = Vec::new();
    for t in find_issue_tokens(text) {
        let own = t.prefix.eq_ignore_ascii_case(team_identifier);
        spans.push((t.start, t.end, Kind::Issue(own.then_some(t.number))));
    }
    for m in find_mentions(text, member_names) {
        spans.push((m.start, m.end, Kind::Mention));
    }
    spans.sort_by_key(|s| s.0);

    let mut out = Vec::new();
    let mut pos = 0;
    for (start, end, kind) in spans {
        if start < pos {
            continue; // overlapping token
        }
        if start > pos {
            out.push(Segment::Text(&text[pos..start]));
        }
        let slice = &text[start..end];
        out.push(match kind {
            Kind::Issue(number) => Segment::Issue {
                text: slice,
                number,
            },
            Kind::Mention => Segment::Mention(slice),
        });
        pos = end;
    }
    if pos < text.len() {
        out.push(Segment::Text(&text[pos..]));
    }
    out
}

// ── Referenced In ──

/// An issue that mentions the open issue, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub source_issue_id: String,
    pub in_description: bool,
    pub in_comment: bool,
}

/// Local equivalent of Rails' `IssueReference` list: scan `issues`'
/// descriptions and `comments` for tokens naming `target`. Self-references are
/// skipped (as upstream does). Results keep first-seen order.
pub fn referenced_in(
    target: &Issue,
    team_identifier: &str,
    issues: &[Issue],
    comments: &[Comment],
) -> Vec<Reference> {
    let Some(number) = target.team_number.map(i64::from) else {
        return Vec::new();
    };
    let hits = |text: &str| referenced_numbers(text, team_identifier).contains(&number);
    let mut out: Vec<Reference> = Vec::new();
    let mut mark = |issue_id: &str, desc: bool| {
        if issue_id == target.id {
            return;
        }
        match out.iter_mut().find(|r| r.source_issue_id == issue_id) {
            Some(r) => {
                r.in_description |= desc;
                r.in_comment |= !desc;
            }
            None => out.push(Reference {
                source_issue_id: issue_id.to_string(),
                in_description: desc,
                in_comment: !desc,
            }),
        }
    };
    for i in issues {
        if i.team_id == target.team_id && i.description.as_deref().is_some_and(hits) {
            mark(&i.id, true);
        }
    }
    for c in comments {
        if hits(&c.body) {
            if let Some(issue_id) = &c.issue_id {
                mark(issue_id, false);
            }
        }
    }
    out
}

// ── Autocomplete ──

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// `@` — member list.
    Mention,
    /// `#` — issue search.
    Issue,
}

/// A partial token ending at the cursor, e.g. `@ann` or `#12`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trailing {
    pub trigger: Trigger,
    pub query: String,
}

impl Trailing {
    /// Chars to delete (trigger + query) to replace the partial token.
    pub fn len_chars(&self) -> usize {
        1 + self.query.chars().count()
    }
}

/// Detect an in-progress `@query` / `#query` immediately before the cursor.
/// `before_cursor` is the current line's text up to the cursor. The trigger
/// must start a whitespace-delimited word and the query may not contain
/// whitespace, `@` or `#`.
pub fn trailing_query(before_cursor: &str) -> Option<Trailing> {
    let word_start = before_cursor
        .rfind(char::is_whitespace)
        .map(|i| i + before_cursor[i..].chars().next().map_or(1, char::len_utf8))
        .unwrap_or(0);
    let word = &before_cursor[word_start..];
    let mut chars = word.chars();
    let trigger = match chars.next()? {
        '@' => Trigger::Mention,
        '#' => Trigger::Issue,
        _ => return None,
    };
    let query: String = chars.collect();
    if query.contains(['@', '#']) {
        return None;
    }
    Some(Trailing { trigger, query })
}

/// One row in the suggestion list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// Bold/primary text (identifier or member name).
    pub label: String,
    /// Secondary text (issue title or member email).
    pub detail: String,
    /// Text that replaces the partial token (includes a trailing space).
    pub insert: String,
}

pub const MAX_SUGGESTIONS: usize = 6;

pub fn member_suggestions(members: &[Member], query: &str) -> Vec<Suggestion> {
    let q = query.to_lowercase();
    members
        .iter()
        .filter(|m| m.name.to_lowercase().contains(&q))
        .take(MAX_SUGGESTIONS)
        .map(|m| Suggestion {
            label: m.name.clone(),
            detail: m.email.clone(),
            insert: format!("@{} ", m.name),
        })
        .collect()
}

/// Issues of the team matching `query` by identifier or title. Identifier
/// matches sort first. `exclude_id` is the open issue (no self-reference).
pub fn issue_suggestions(
    issues: &[Issue],
    team_identifier: &str,
    exclude_id: &str,
    query: &str,
) -> Vec<Suggestion> {
    let q = query.to_lowercase();
    let mut hits: Vec<(bool, i32, Suggestion)> = issues
        .iter()
        .filter(|i| i.id != exclude_id && i.archived_at.is_none())
        .filter_map(|i| {
            let n = i.team_number?;
            let ident = format!("{team_identifier}-{n}");
            let by_ident = ident.to_lowercase().contains(&q);
            let by_title = i.title.to_lowercase().contains(&q);
            (by_ident || by_title).then(|| {
                (
                    !by_ident,
                    -n,
                    Suggestion {
                        insert: format!("{ident} "),
                        label: ident,
                        detail: i.title.clone(),
                    },
                )
            })
        })
        .collect();
    hits.sort_by_key(|a| (a.0, a.1));
    hits.into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|h| h.2)
        .collect()
}

/// Which editor the open suggestion list belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorTarget {
    Description,
    Comment,
    Reply,
}

/// Open suggestion list under an editor.
#[derive(Debug, Clone)]
pub struct SuggestState {
    pub target: EditorTarget,
    /// Chars of the partial token to delete before inserting the choice.
    pub delete_chars: usize,
    pub items: Vec<Suggestion>,
    /// Highlighted row (keyboard navigation); always `< items.len()`.
    pub selected: usize,
}

impl SuggestState {
    /// Move the highlight by `delta` rows, wrapping at both ends.
    pub fn move_selection(&mut self, delta: i32) {
        let n = self.items.len() as i32;
        if n > 0 {
            self.selected = (self.selected as i32 + delta).rem_euclid(n) as usize;
        }
    }
}

/// Build the suggestion list for the cursor's `line`/`col` (byte offset, as
/// reported by `text_editor::Content::cursor_position`). `None` when there is
/// no partial `@`/`#` token or nothing matches.
pub fn build_suggest(
    target: EditorTarget,
    line: &str,
    col: usize,
    members: &[Member],
    issues: &[Issue],
    team_identifier: &str,
    exclude_id: &str,
) -> Option<SuggestState> {
    let t = trailing_query(line_before_cursor(line, col))?;
    let items = match t.trigger {
        Trigger::Mention => member_suggestions(members, &t.query),
        Trigger::Issue => issue_suggestions(issues, team_identifier, exclude_id, &t.query),
    };
    (!items.is_empty()).then(|| SuggestState {
        target,
        delete_chars: t.len_chars(),
        items,
        selected: 0,
    })
}

/// Split a token like `ENG-12` / `#eng-12` into (prefix, number).
pub fn parse_ident(token: &str) -> Option<(&str, i64)> {
    let (prefix, num) = token.trim_start_matches('#').split_once('-')?;
    Some((prefix, num.parse().ok()?))
}

/// Local issue for `PREFIX-number` across all teams (prefix case-insensitive).
pub fn resolve_issue<'a>(
    issues: &'a [Issue],
    teams: &[Team],
    prefix: &str,
    number: i64,
) -> Option<&'a Issue> {
    let team = teams
        .iter()
        .find(|t| t.identifier.eq_ignore_ascii_case(prefix))?;
    issues
        .iter()
        .find(|i| i.team_id == team.id && i.team_number.map(i64::from) == Some(number))
}

/// Text of the cursor's line up to the cursor. `col` is the editor's column
/// (a byte offset; clamped defensively to a char boundary).
pub fn line_before_cursor(line: &str, col: usize) -> &str {
    let mut end = col.min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    &line[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(id: &str, num: i32, desc: Option<&str>) -> Issue {
        Issue {
            id: id.into(),
            title: format!("Title {num}"),
            description: desc.map(String::from),
            priority: 0,
            team_number: Some(num),
            due_date: None,
            estimate: None,
            started_at: None,
            completed_at: None,
            canceled_at: None,
            lane_id: "l".into(),
            team_id: "t".into(),
            project_id: None,
            parent_issue_id: None,
            assignee_id: None,
            creator_id: None,
            archived_at: None,
            remote_id: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn comment(issue_id: &str, body: &str) -> Comment {
        Comment {
            id: format!("c-{issue_id}"),
            remote_id: None,
            issue_id: Some(issue_id.into()),
            project_id: None,
            author_id: None,
            body: body.into(),
            parent_id: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn member(name: &str) -> Member {
        Member {
            id: name.into(),
            remote_id: None,
            team_id: "t".into(),
            name: name.into(),
            email: format!("{name}@x.io"),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn tokens_match_rails_regex() {
        let t = find_issue_tokens("see JAIT-12, #jait-3 and (ABC-7).");
        let got: Vec<_> = t.iter().map(|t| (t.prefix.as_str(), t.number)).collect();
        assert_eq!(got, vec![("JAIT", 12), ("jait", 3), ("ABC", 7)]);
        // `#` is included in the span.
        assert_eq!(&"see JAIT-12, #jait-3"[t[1].start..t[1].end], "#jait-3");
    }

    #[test]
    fn tokens_reject_non_matches() {
        assert!(find_issue_tokens("AB-1 TOOLONG-1 xJAIT-1 JAIT-1x JAIT- JAIT_-1").is_empty());
        assert!(find_issue_tokens("").is_empty());
        // 5-char prefix doesn't match even if its tail looks like a prefix.
        assert!(find_issue_tokens("ABCDE-5").is_empty());
    }

    #[test]
    fn tokens_handle_unicode_neighbours() {
        let t = find_issue_tokens("é JAIT-1 日本");
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].number, 1);
    }

    #[test]
    fn referenced_numbers_filters_team_and_dedups() {
        assert_eq!(
            referenced_numbers("JAIT-2 jait-2 OTH-9 #JAIT-5", "JAIT"),
            vec![2, 5]
        );
    }

    #[test]
    fn mentions_longest_name_and_boundaries() {
        let names = ["Ann", "Ann Lee"];
        let text = "hi @ann lee, @Ann! not mail@ann or @Anna";
        let m = find_mentions(text, &names);
        let got: Vec<_> = m.iter().map(|m| &text[m.start..m.end]).collect();
        assert_eq!(got, vec!["@ann lee", "@Ann"]);
    }

    #[test]
    fn segments_split_text() {
        let s = segments("ping @Bob re JAIT-4 / OTH-1", "JAIT", &["Bob"]);
        assert_eq!(
            s,
            vec![
                Segment::Text("ping "),
                Segment::Mention("@Bob"),
                Segment::Text(" re "),
                Segment::Issue {
                    text: "JAIT-4",
                    number: Some(4)
                },
                Segment::Text(" / "),
                Segment::Issue {
                    text: "OTH-1",
                    number: None
                },
            ]
        );
        assert_eq!(segments("plain", "JAIT", &[]), vec![Segment::Text("plain")]);
    }

    #[test]
    fn trailing_query_detects_triggers() {
        assert_eq!(
            trailing_query("hello @an"),
            Some(Trailing {
                trigger: Trigger::Mention,
                query: "an".into()
            })
        );
        assert_eq!(
            trailing_query("#"),
            Some(Trailing {
                trigger: Trigger::Issue,
                query: String::new()
            })
        );
        assert_eq!(trailing_query("see #12").unwrap().len_chars(), 3);
        assert_eq!(trailing_query("mail@ann"), None);
        assert_eq!(trailing_query("done @ann "), None);
        assert_eq!(trailing_query("#a#b"), None);
        assert_eq!(trailing_query(""), None);
        // Multibyte whitespace/chars before the trigger.
        assert!(trailing_query("é\u{00a0}#x").is_some());
    }

    #[test]
    fn cursor_column_is_a_byte_offset() {
        // iced 0.13 reports cosmic-text's byte index, not a char index.
        // "héllo @a": 'é' is 2 bytes, so the cursor after "@a" is at byte 9.
        let line = "héllo @a";
        assert_eq!(line.len(), 9);
        assert_eq!(line_before_cursor(line, 9), line);
        let t = trailing_query(line_before_cursor(line, 9)).unwrap();
        assert_eq!((t.trigger, t.query.as_str()), (Trigger::Mention, "a"));
        // Cursor mid-line after "é": must not be treated as char index 2.
        assert_eq!(line_before_cursor(line, 3), "hé");
        // CJK + emoji before the trigger.
        let line = "日本 🎉 #1";
        let t = trailing_query(line_before_cursor(line, line.len())).unwrap();
        assert_eq!((t.trigger, t.query.as_str()), (Trigger::Issue, "1"));
    }

    #[test]
    fn build_suggest_and_selection_wrap() {
        let ms = [member("Ann"), member("Anders")];
        let mut s = build_suggest(EditorTarget::Comment, "héy @An", 8, &ms, &[], "ENG", "x")
            .unwrap();
        assert_eq!((s.items.len(), s.selected, s.delete_chars), (2, 0, 3));
        s.move_selection(-1);
        assert_eq!(s.selected, 1);
        s.move_selection(1);
        assert_eq!(s.selected, 0);
        assert!(build_suggest(EditorTarget::Comment, "plain", 5, &ms, &[], "ENG", "x").is_none());
    }

    #[test]
    fn parse_and_resolve_cross_team() {
        assert_eq!(parse_ident("#ENG-12"), Some(("ENG", 12)));
        assert_eq!(parse_ident("ENG-x"), None);
        let mut a = issue("a", 5, None);
        a.team_id = "t2".into();
        let mut t = Team {
            id: "t2".into(),
            name: "Other".into(),
            identifier: "OTH".into(),
            remote_id: None,
            archived_at: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let issues = [a];
        assert!(resolve_issue(&issues, std::slice::from_ref(&t), "oth", 5).is_some());
        assert!(resolve_issue(&issues, std::slice::from_ref(&t), "OTH", 6).is_none());
        t.identifier = "ZZZ".into();
        assert!(resolve_issue(&issues, &[t], "OTH", 5).is_none());
    }

    #[test]
    fn line_before_cursor_clamps() {
        assert_eq!(line_before_cursor("abc", 2), "ab");
        assert_eq!(line_before_cursor("abc", 99), "abc");
        assert_eq!(line_before_cursor("é", 1), "");
    }

    #[test]
    fn suggestions_filter_and_format() {
        let ms = [member("Ann Lee"), member("Bob")];
        let s = member_suggestions(&ms, "ann");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].insert, "@Ann Lee ");

        let is = [
            issue("a", 1, None),
            issue("b", 12, None),
            issue("c", 2, None),
        ];
        let s = issue_suggestions(&is, "JAIT", "c", "1");
        // identifier matches (JAIT-1, JAIT-12), newest first; excluded id absent.
        let labels: Vec<_> = s.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, vec!["JAIT-12", "JAIT-1"]);
        assert_eq!(s[0].insert, "JAIT-12 ");
        // Title search.
        assert_eq!(issue_suggestions(&is, "JAIT", "", "title 2").len(), 1);
    }

    #[test]
    fn referenced_in_scans_descriptions_and_comments() {
        let target = issue("t1", 5, None);
        let issues = [
            target.clone(),
            issue("a", 1, Some("blocked by JAIT-5")),
            issue("b", 2, Some("nothing, or OTH-5")),
            issue("c", 3, Some("self ref JAIT-3 and #jait-5")),
            issue("d", 4, Some("JAIT-5 again")),
            issue("t1", 5, Some("I mention JAIT-5 myself")),
        ];
        let comments = [
            comment("b", "see #JAIT-5"),
            comment("a", "also JAIT-5"),
            comment("e", "JAIT-50 is different"),
        ];
        let refs = referenced_in(&target, "JAIT", &issues, &comments);
        let got: Vec<_> = refs
            .iter()
            .map(|r| (r.source_issue_id.as_str(), r.in_description, r.in_comment))
            .collect();
        assert_eq!(
            got,
            vec![
                ("a", true, true),
                ("c", true, false),
                ("d", true, false),
                ("b", false, true)
            ]
        );
    }

    #[test]
    fn referenced_in_without_number_is_empty() {
        let mut target = issue("t", 1, None);
        target.team_number = None;
        assert!(referenced_in(&target, "JAIT", &[], &[]).is_empty());
    }
}
