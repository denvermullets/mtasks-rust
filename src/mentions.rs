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
use crate::issue_match::IssueQuery;

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

// ── Body segments: text runs and image blocks ──

/// Path prefix of a JAIT (Rails ActiveStorage) blob redirect URL.
const BLOB_PATH_PREFIX: &str = "/rails/active_storage/blobs/redirect/";
/// Scheme of the local placeholder used for a not-yet-uploaded image.
pub const PENDING_SCHEME: &str = "jait-pending://";

/// Where an image block's `src` points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind<'a> {
    /// JAIT blob-redirect URL (relative or on the configured host).
    Blob,
    /// Local `jait-pending://<uuid>` placeholder; carries the uuid.
    Pending(&'a str),
}

/// `![alt](src)` that passed the JAIT-only allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageBlock<'a> {
    pub alt: &'a str,
    pub src: &'a str,
    pub kind: ImageKind<'a>,
}

/// One piece of a description/comment body, in order. Text runs can be fed to
/// [`segments`] for mention and issue-reference highlighting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodySegment<'a> {
    Text(&'a str),
    Image(ImageBlock<'a>),
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Classify an image URL. Only JAIT blob URLs and pending placeholders pass;
/// `host` is the configured base URL (trailing slash tolerated).
pub(crate) fn classify_image_src<'a>(src: &'a str, host: &str) -> Option<ImageKind<'a>> {
    if let Some(id) = src.strip_prefix(PENDING_SCHEME) {
        return is_uuid(id).then_some(ImageKind::Pending(id));
    }
    let host = host.trim_end_matches('/');
    let path = if src.starts_with('/') {
        src
    } else if !host.is_empty() {
        src.strip_prefix(host).filter(|p| p.starts_with('/'))?
    } else {
        return None;
    };
    let rest = path.strip_prefix(BLOB_PATH_PREFIX)?;
    let (signed_id, filename) = rest.split_once('/')?;
    (!signed_id.is_empty() && !filename.is_empty() && !filename.contains('/'))
        .then_some(ImageKind::Blob)
}

/// Try to parse an image at the start of `s` (which begins with `![`).
/// Returns the block and the byte length consumed.
fn parse_image<'a>(s: &'a str, host: &str) -> Option<(ImageBlock<'a>, usize)> {
    let alt_end = s[2..].find(']')? + 2;
    let alt = &s[2..alt_end];
    if alt.contains(['[', '\n']) {
        return None;
    }
    let url_part = s[alt_end + 1..].strip_prefix('(')?;
    // The URL ends at the `)` that balances the opening one, so filenames
    // containing parentheses (which the web leaves unescaped) still parse.
    let mut depth = 1usize;
    let mut close = None;
    for (i, c) in url_part.char_indices() {
        match c {
            c if c.is_whitespace() => return None,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let src = &url_part[..close];
    if src.contains(char::is_whitespace) {
        return None;
    }
    let kind = classify_image_src(src, host)?;
    Some((ImageBlock { alt, src, kind }, alt_end + 2 + close + 1))
}

/// Split a body into ordered text runs and image blocks. `host` is the
/// configured JAIT base URL, used to recognise absolute blob URLs. Anything
/// that is not a recognised image (including malformed syntax) stays text.
pub fn body_segments<'a>(text: &'a str, host: &str) -> Vec<BodySegment<'a>> {
    let mut out = Vec::new();
    let mut run_start = 0;
    let mut i = 0;
    while let Some(off) = text[i..].find("![") {
        let at = i + off;
        match parse_image(&text[at..], host) {
            Some((block, len)) => {
                if at > run_start {
                    out.push(BodySegment::Text(&text[run_start..at]));
                }
                out.push(BodySegment::Image(block));
                i = at + len;
                run_start = i;
            }
            None => i = at + 2,
        }
    }
    if run_start < text.len() {
        out.push(BodySegment::Text(&text[run_start..]));
    }
    out
}

/// Result of [`rewrite_placeholders`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceholderRewrite {
    /// The body with every resolved placeholder URL replaced.
    pub body: String,
    /// Placeholder uuids still in the body with no entry in the map, in order of
    /// first appearance, without repeats.
    pub unresolved: Vec<String>,
}

/// Percent-encode like the web's JS `encodeURIComponent`: everything except
/// `A-Z a-z 0-9 - _ . ! ~ * ' ( )`.
fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Rewrite every resolved `jait-pending://<uuid>` image URL to the web's form
/// `/rails/active_storage/blobs/redirect/<signed_id>/<filename>`. `resolved` maps
/// placeholder uuid to `(signed_id, filename)`; the filename is encoded as the web
/// does. Everything else is left byte-for-byte as is.
pub fn rewrite_placeholders(
    text: &str,
    resolved: &std::collections::HashMap<String, (String, String)>,
) -> PlaceholderRewrite {
    let mut body = String::with_capacity(text.len());
    let mut unresolved: Vec<String> = Vec::new();
    for seg in body_segments(text, "") {
        match seg {
            BodySegment::Text(t) => body.push_str(t),
            BodySegment::Image(ImageBlock { alt, src, kind }) => match kind {
                ImageKind::Pending(id) => match resolved.get(id) {
                    Some((signed_id, filename)) => {
                        body.push_str(&format!(
                            "![{alt}]({BLOB_PATH_PREFIX}{signed_id}/{})",
                            encode_uri_component(filename)
                        ));
                    }
                    None => {
                        if !unresolved.iter().any(|u| u == id) {
                            unresolved.push(id.to_string());
                        }
                        body.push_str(&format!("![{alt}]({src})"));
                    }
                },
                ImageKind::Blob => body.push_str(&format!("![{alt}]({src})")),
            },
        }
    }
    PlaceholderRewrite { body, unresolved }
}

/// Remove the whole `![alt](jait-pending://<uuid>)` for one placeholder (e.g. a
/// failed Inline image), alt text included. Only the first occurrence goes; the
/// rest of the body is unchanged.
pub fn strip_placeholder(text: &str, uuid: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut removed = false;
    for seg in body_segments(text, "") {
        match seg {
            BodySegment::Text(t) => out.push_str(t),
            BodySegment::Image(ImageBlock { alt, src, kind }) => {
                if !removed && kind == ImageKind::Pending(uuid) {
                    removed = true;
                } else {
                    out.push_str(&format!("![{alt}]({src})"));
                }
            }
        }
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

/// Issues of the team matching `query` via the shared matcher
/// (`issue_match`), newest number first. `exclude_id` is the open issue (no
/// self-reference).
pub fn issue_suggestions(
    issues: &[Issue],
    team_identifier: &str,
    exclude_id: &str,
    query: &str,
) -> Vec<Suggestion> {
    let q = IssueQuery::parse(query);
    let mut hits: Vec<(i32, Suggestion)> = issues
        .iter()
        .filter(|i| i.id != exclude_id)
        .filter(|i| q.matches(i, team_identifier))
        .filter_map(|i| {
            let n = i.team_number?;
            let ident = format!("{team_identifier}-{n}");
            Some((
                -n,
                Suggestion {
                    insert: format!("{ident} "),
                    label: ident,
                    detail: i.title.clone(),
                },
            ))
        })
        .collect();
    hits.sort_by_key(|h| h.0);
    hits.into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|h| h.1)
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

/// What a link in rendered body text points at. Later link kinds (mentions,
/// external URLs) are new variants; the renderer and app routing stay put.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// Local issue UUID.
    Issue(String),
    /// External URL.
    External(String),
    /// Mention text including the `@` (resolved to a member at hover time).
    Mention(String),
}

/// Typed target for an `IDENT-n` token, if it resolves to a local issue.
pub fn issue_link_target(issues: &[Issue], teams: &[Team], token: &str) -> Option<LinkTarget> {
    let (prefix, number) = parse_ident(token)?;
    resolve_issue(issues, teams, prefix, number).map(|i| LinkTarget::Issue(i.id.clone()))
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
mod body_segment_tests {
    use super::*;

    const HOST: &str = "https://jait.example.com";
    const BLOB: &str = "/rails/active_storage/blobs/redirect/abc123/shot.png";
    const UUID: &str = "123e4567-e89b-12d3-a456-426614174000";

    fn img<'a>(alt: &'a str, src: &'a str, kind: ImageKind<'a>) -> BodySegment<'a> {
        BodySegment::Image(ImageBlock { alt, src, kind })
    }

    #[test]
    fn text_only_is_one_run_identical_to_input() {
        let t = "hello world\nsecond line";
        assert_eq!(body_segments(t, HOST), vec![BodySegment::Text(t)]);
    }

    #[test]
    fn empty_body_has_no_segments() {
        assert!(body_segments("", HOST).is_empty());
    }

    #[test]
    fn single_image() {
        let t = format!("![shot]({BLOB})");
        assert_eq!(
            body_segments(&t, HOST),
            vec![img("shot", BLOB, ImageKind::Blob)]
        );
    }

    #[test]
    fn image_between_text_runs() {
        let t = format!("before ![a]({BLOB}) after");
        assert_eq!(
            body_segments(&t, HOST),
            vec![
                BodySegment::Text("before "),
                img("a", BLOB, ImageKind::Blob),
                BodySegment::Text(" after"),
            ]
        );
    }

    #[test]
    fn adjacent_images() {
        let t = format!("![a]({BLOB})![b]({BLOB})");
        assert_eq!(
            body_segments(&t, HOST),
            vec![
                img("a", BLOB, ImageKind::Blob),
                img("b", BLOB, ImageKind::Blob)
            ]
        );
    }

    #[test]
    fn absolute_blob_url_on_configured_host() {
        let src = format!("{HOST}{BLOB}");
        let t = format!("![a]({src})");
        assert_eq!(
            body_segments(&t, "https://jait.example.com/"),
            vec![img("a", &src, ImageKind::Blob)]
        );
    }

    #[test]
    fn foreign_host_blob_url_stays_text() {
        let t = format!("![a](https://evil.example.org{BLOB})");
        assert_eq!(body_segments(&t, HOST), vec![BodySegment::Text(&t)]);
    }

    #[test]
    fn non_jait_url_stays_text() {
        let t = "![a](https://example.com/cat.png)";
        assert_eq!(body_segments(t, HOST), vec![BodySegment::Text(t)]);
    }

    #[test]
    fn pending_placeholder_exposes_uuid() {
        let src = format!("jait-pending://{UUID}");
        let t = format!("x ![a]({src})");
        assert_eq!(
            body_segments(&t, HOST),
            vec![
                BodySegment::Text("x "),
                img("a", &src, ImageKind::Pending(UUID))
            ]
        );
    }

    #[test]
    fn pending_with_bad_uuid_stays_text() {
        let t = "![a](jait-pending://not-a-uuid)";
        assert_eq!(body_segments(t, HOST), vec![BodySegment::Text(t)]);
    }

    #[test]
    fn malformed_syntax_stays_text() {
        for t in [
            "![alt](",
            "![alt](/rails/active_storage/blobs/redirect/a/b.png",
            "![alt]",
            "![alt] (/rails/active_storage/blobs/redirect/a/b.png)",
            "![alt",
        ] {
            assert_eq!(body_segments(t, HOST), vec![BodySegment::Text(t)], "{t}");
        }
    }

    #[test]
    fn malformed_then_valid_image() {
        let t = format!("![oops ![a]({BLOB})");
        assert_eq!(
            body_segments(&t, HOST),
            vec![
                BodySegment::Text("![oops "),
                img("a", BLOB, ImageKind::Blob)
            ]
        );
    }

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
            role: None,
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
    fn issue_token_resolves_to_a_typed_issue_target() {
        let a = issue("a-id", 5, None);
        let t = Team {
            id: "t".into(),
            name: "T".into(),
            identifier: "OTH".into(),
            remote_id: None,
            archived_at: None,
            role: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let issues = [a];
        let teams = [t];
        assert_eq!(
            issue_link_target(&issues, &teams, "#oth-5"),
            Some(LinkTarget::Issue("a-id".into()))
        );
        assert_eq!(issue_link_target(&issues, &teams, "OTH-6"), None);
        assert_eq!(issue_link_target(&issues, &teams, "nonsense"), None);
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
        // Exact number only (Rails semantics): #1, not #12; excluded id absent.
        let labels: Vec<_> = s.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, vec!["JAIT-1"]);
        assert_eq!(s[0].insert, "JAIT-1 ");
        let s = issue_suggestions(&is, "JAIT", "", "jait-12");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].label, "JAIT-12");
        // Wrong prefix matches nothing.
        assert!(issue_suggestions(&is, "JAIT", "", "ENG-12").is_empty());
        // Title search.
        assert_eq!(issue_suggestions(&is, "JAIT", "", "title 2").len(), 1);
        // Blank query matches nothing.
        assert!(issue_suggestions(&is, "JAIT", "", "  ").is_empty());
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

    // ── placeholder rewrite ──

    const U1: &str = "11111111-1111-4111-8111-111111111111";
    const U2: &str = "22222222-2222-4222-8222-222222222222";

    fn map(entries: &[(&str, &str, &str)]) -> std::collections::HashMap<String, (String, String)> {
        entries
            .iter()
            .map(|(u, s, f)| (u.to_string(), (s.to_string(), f.to_string())))
            .collect()
    }

    #[test]
    fn rewrite_full_map() {
        let text = format!("a\n![x]({PENDING_SCHEME}{U1}) b ![y]({PENDING_SCHEME}{U2})");
        let r = rewrite_placeholders(&text, &map(&[(U1, "sig1", "a.png"), (U2, "sig2", "b.png")]));
        assert_eq!(
            r.body,
            "a\n![x](/rails/active_storage/blobs/redirect/sig1/a.png) b ![y](/rails/active_storage/blobs/redirect/sig2/b.png)"
        );
        assert!(r.unresolved.is_empty());
    }

    #[test]
    fn rewrite_partial_and_empty_map() {
        let text = format!("![x]({PENDING_SCHEME}{U1})\n![y]({PENDING_SCHEME}{U2})\n![z]({PENDING_SCHEME}{U2})");
        let r = rewrite_placeholders(&text, &map(&[(U1, "sig1", "a.png")]));
        assert_eq!(
            r.body,
            format!("![x](/rails/active_storage/blobs/redirect/sig1/a.png)\n![y]({PENDING_SCHEME}{U2})\n![z]({PENDING_SCHEME}{U2})")
        );
        assert_eq!(r.unresolved, vec![U2.to_string()]);

        let r = rewrite_placeholders(&text, &map(&[]));
        assert_eq!(r.body, text);
        assert_eq!(r.unresolved, vec![U1.to_string(), U2.to_string()]);
    }

    #[test]
    fn rewrite_leaves_other_text_untouched() {
        let text = format!(
            "héllo 🎉\r\n\n![](/rails/active_storage/blobs/redirect/s/k.png) [x] ![bad]( {U1}) ![y]({PENDING_SCHEME}{U1})  \n"
        );
        let r = rewrite_placeholders(&text, &map(&[(U1, "s", "k.png")]));
        assert_eq!(
            r.body,
            "héllo 🎉\r\n\n![](/rails/active_storage/blobs/redirect/s/k.png) [x] ![bad]( 11111111-1111-4111-8111-111111111111) ![y](/rails/active_storage/blobs/redirect/s/k.png)  \n"
        );
        assert!(rewrite_placeholders("no images é", &map(&[])).body == "no images é");
    }

    #[test]
    fn rewrite_encodes_filename_like_encode_uri_component() {
        let text = format!("![x]({PENDING_SCHEME}{U1})");
        let r = rewrite_placeholders(&text, &map(&[(U1, "s", "my pic é+#?.png")]));
        assert_eq!(
            r.body,
            "![x](/rails/active_storage/blobs/redirect/s/my%20pic%20%C3%A9%2B%23%3F.png)"
        );
    }

    #[test]
    fn filename_with_parens_round_trips_through_rewrite() {
        let text = format!("![x]({PENDING_SCHEME}{U1}) tail");
        let r = rewrite_placeholders(&text, &map(&[(U1, "s", "shot (1).png")]));
        let segs = body_segments(&r.body, "");
        assert_eq!(segs.len(), 2);
        match segs[0] {
            BodySegment::Image(b) => {
                assert_eq!(b.kind, ImageKind::Blob);
                assert!(b.src.ends_with("/shot%20(1).png"));
            }
            _ => panic!("expected image"),
        }
        assert_eq!(segs[1], BodySegment::Text(" tail"));
    }

    #[test]
    fn web_body_with_unescaped_parens_parses() {
        let body = "a ![p](/rails/active_storage/blobs/redirect/sid/photo(2).png) b";
        let segs = body_segments(body, "");
        assert_eq!(segs.len(), 3);
        assert!(matches!(segs[1], BodySegment::Image(b) if b.src.ends_with("photo(2).png")));
        assert_eq!(segs[2], BodySegment::Text(" b"));
        // Unbalanced stays text.
        let bad = "![p](/rails/active_storage/blobs/redirect/sid/a(b.png";
        assert_eq!(body_segments(bad, ""), vec![BodySegment::Text(bad)]);
    }

    #[test]
    fn strip_removes_exactly_one_placeholder_image() {
        let text = format!("a ![alt text]({PENDING_SCHEME}{U1}) b ![k]({PENDING_SCHEME}{U2})\n");
        assert_eq!(
            strip_placeholder(&text, U1),
            format!("a  b ![k]({PENDING_SCHEME}{U2})\n")
        );
        // Unknown uuid: unchanged.
        assert_eq!(strip_placeholder(&text, "33333333-3333-4333-8333-333333333333"), text);
        // Duplicate: only the first goes.
        let dup = format!("![a]({PENDING_SCHEME}{U1})|![b]({PENDING_SCHEME}{U1})");
        assert_eq!(strip_placeholder(&dup, U1), format!("|![b]({PENDING_SCHEME}{U1})"));
    }
}
