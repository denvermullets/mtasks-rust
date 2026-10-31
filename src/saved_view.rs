//! Saved views: pure mapping between a Rails Saved view `query` (a
//! string -> string map) and the desktop's display options, filters and view
//! search. Decode side only for now; encode lives alongside it later.
//!
//! Rails keeps only the last value per key and stores everything as strings:
//! lists are comma-separated, booleans are `true`/`1`.

#![allow(dead_code)]

use std::collections::BTreeMap;

use crate::db::models::{Label, Lane, Member, Priority, Project};
use crate::display::{
    CardProperty, CompletedFilter, DisplayOptions, Filters, GroupBy, OrderBy, SubGroup, ViewMode,
};

/// A Saved view's raw Rails query.
pub type RawQuery = BTreeMap<String, String>;

/// Parse a stored query (JSON object text) into the string map. Non-string
/// scalars are stringified, nulls dropped; anything not an object is empty.
pub fn parse_stored_query(json: &str) -> RawQuery {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str(json) else {
        return RawQuery::new();
    };
    map.into_iter()
        .filter_map(|(k, v)| match v {
            serde_json::Value::Null => None,
            serde_json::Value::String(s) => Some((k, s)),
            other => Some((k, other.to_string())),
        })
        .collect()
}

/// The desktop state a Saved view drives: display options, filters and the
/// view search text.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BoardState {
    pub display: DisplayOptions,
    pub filters: Filters,
    pub search: String,
}

/// The team's mirrored records, used to translate remote ids to local ids.
#[derive(Debug, Clone, Copy)]
pub struct Mirror<'a> {
    pub lanes: &'a [Lane],
    pub members: &'a [Member],
    pub labels: &'a [Label],
    pub projects: &'a [Project],
}

/// Result of decoding: the board state plus the untouched raw query.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    pub state: BoardState,
    pub raw: RawQuery,
}

/// An open Saved view: the working copy that stands in for the user's own
/// board state while the view is shown. Edits change `state` only.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenView {
    /// Local id of the Saved view.
    pub view_id: String,
    /// Local team id the view belongs to.
    pub team_id: String,
    pub name: String,
    /// Working copy: display, filters and view search.
    pub state: BoardState,
    /// The stored query as decoded, kept for encode/revert later.
    pub raw: RawQuery,
    /// The working copy as it was when opened (the saved version, with keys
    /// the view doesn't set filled from the local state). Modified is
    /// measured against this.
    pub baseline: BoardState,
}

/// A modelled key's value for Modified detection. `visible_properties` is a
/// set (toggling a property off and on re-appends it), so its order is ignored.
fn normalized(q: &RawQuery, key: &str) -> Option<String> {
    let v = q.get(key)?;
    if key != "visible_properties" {
        return Some(v.clone());
    }
    let mut parts: Vec<&str> = v.split(',').collect();
    parts.sort_unstable();
    Some(parts.join(","))
}

impl OpenView {
    /// Open `sv`, falling back to `local` for keys the view doesn't set.
    pub fn open(sv: &crate::db::saved_views::SavedView, mirror: &Mirror, local: &BoardState) -> Self {
        let d = decode(&parse_stored_query(&sv.query), mirror, local);
        OpenView {
            view_id: sv.id.clone(),
            team_id: sv.team_id.clone(),
            name: sv.name.clone(),
            baseline: d.state.clone(),
            state: d.state,
            raw: d.raw,
        }
    }

    /// Apply a newly pulled version of this view. The name always follows the
    /// server. An unmodified view takes the new definition; a Modified one keeps its working copy and is measured
    /// against the new saved version.
    pub fn refresh(&mut self, latest: &crate::db::saved_views::SavedView, mirror: &Mirror, local: &BoardState) {
        let modified = self.is_modified(mirror);
        let fresh = OpenView::open(latest, mirror, local);
        self.name = fresh.name;
        self.raw = fresh.raw;
        self.baseline = fresh.baseline.clone();
        if !modified {
            self.state = fresh.baseline;
        }
    }

    /// The working copy as a Rails query: encoded over the original raw query.
    pub fn encoded(&self, mirror: &Mirror) -> RawQuery {
        encode(&self.state, &self.raw, mirror)
    }

    /// Whether the working copy differs from the saved version, compared over
    /// the desktop-modelled keys only.
    pub fn is_modified(&self, mirror: &Mirror) -> bool {
        let now = self.encoded(mirror);
        let saved = encode(&self.baseline, &self.raw, mirror);
        MODELLED_KEYS.iter().any(|k| normalized(&now, k) != normalized(&saved, k))
    }

    /// The working copy was saved as the view's new definition: it becomes the
    /// stored query and the baseline, so the view is no longer Modified.
    pub fn mark_saved(&mut self, mirror: &Mirror) {
        self.raw = self.encoded(mirror);
        self.baseline = self.state.clone();
    }

    /// Drop the working copy's changes and show the saved version again.
    pub fn revert(&mut self) {
        self.state = self.baseline.clone();
    }

    /// Whether the view is still the thing on screen: the selected team is its
    /// team and the pane is that team's board (or an issue opened from it).
    pub fn stays_open(&self, selected_team: Option<&str>, on_board_or_detail: bool) -> bool {
        on_board_or_detail && selected_team == Some(self.team_id.as_str())
    }
}

/// Decode `query` against the mirror. Keys that are missing, or whose value is
/// bad or unknown, fall back to `local`. Unresolvable ids are left out.
pub fn decode(query: &RawQuery, mirror: &Mirror, local: &BoardState) -> Decoded {
    let mut s = local.clone();
    let get = |k: &str| query.get(k).map(String::as_str);

    if let Some(v) = get("view_mode").and_then(view_mode) {
        s.display.view_mode = v;
    }
    if let Some(v) = get("group_by").and_then(group_by) {
        s.display.group_by = v;
    }
    if let Some(v) = get("sub_group_by").and_then(sub_group) {
        s.display.sub_group = v;
    }
    if let Some(v) = get("order_by").and_then(order_by) {
        s.display.order_by = v;
    }
    if let Some(v) = get("completed_filter").and_then(completed_filter) {
        s.display.set_completed_filter(v);
    }
    if let Some(v) = get("show_sub_issues").and_then(boolean) {
        s.display.show_sub_issues = v;
    }
    if let Some(v) = get("show_empty_groups").and_then(boolean) {
        s.display.show_empty_groups = v;
    }
    if let Some(v) = get("visible_properties") {
        s.display.properties = csv(v).into_iter().filter_map(card_property).collect();
    }

    if let Some(v) = get("lane_ids") {
        s.filters.statuses = local_ids(v, mirror.lanes.iter().map(|l| (l.remote_id, &l.id)));
    }
    if let Some(v) = get("assignee_ids") {
        s.filters.assignees = local_ids(v, mirror.members.iter().map(|m| (m.remote_id, &m.id)));
    }
    if let Some(v) = get("label_ids") {
        s.filters.labels = local_ids(v, mirror.labels.iter().map(|l| (l.remote_id, &l.id)));
    }
    if let Some(v) = get("project_ids") {
        // "none" is the no-project bucket, an empty string in the desktop.
        let mut out = Vec::new();
        for part in csv(v) {
            if part == "none" {
                out.push(String::new());
            } else {
                out.extend(local_ids(
                    part,
                    mirror.projects.iter().map(|p| (p.remote_id, &p.id)),
                ));
            }
        }
        s.filters.projects = out;
    }
    if let Some(v) = get("priority") {
        s.filters.priorities = csv(v)
            .into_iter()
            .filter_map(|n| Priority::from_rails_str(n).ok())
            .map(|p| p.to_i32().to_string())
            .collect();
    }
    if let Some(v) = get("q") {
        s.search = v.to_string();
    }

    Decoded {
        state: s,
        raw: query.clone(),
    }
}

fn csv(v: &str) -> Vec<&str> {
    v.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect()
}

/// Prefix of a placeholder standing for a record created offline (no remote
/// id yet): `local:<local id>`. Resolved to the remote id at push.
pub const LOCAL_PLACEHOLDER: &str = "local:";

/// The local id a placeholder names, if `part` is one.
pub fn placeholder_local_id(part: &str) -> Option<&str> {
    part.strip_prefix(LOCAL_PLACEHOLDER).filter(|id| !id.is_empty())
}

/// Translate comma-separated remote ids (or local-id placeholders) to local
/// ids, dropping any that don't resolve (including non-numeric ones).
fn local_ids<'a>(
    v: &str,
    records: impl Iterator<Item = (Option<i64>, &'a String)>,
) -> Vec<String> {
    let records: Vec<_> = records.collect();
    csv(v)
        .into_iter()
        .filter_map(|p| {
            if let Some(local) = placeholder_local_id(p) {
                records.iter().find(|(_, id)| id.as_str() == local)
            } else {
                let rid = p.parse::<i64>().ok()?;
                records.iter().find(|(r, _)| *r == Some(rid))
            }
        })
        .map(|(_, id)| (*id).clone())
        .collect()
}

fn boolean(v: &str) -> Option<bool> {
    match v {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

fn view_mode(v: &str) -> Option<ViewMode> {
    match v {
        "board" => Some(ViewMode::Board),
        "list" => Some(ViewMode::List),
        _ => None,
    }
}

fn group_by(v: &str) -> Option<GroupBy> {
    match v {
        "none" => Some(GroupBy::None),
        "status" => Some(GroupBy::Status),
        "assignee" => Some(GroupBy::Assignee),
        "project" => Some(GroupBy::Project),
        "priority" => Some(GroupBy::Priority),
        "label" => Some(GroupBy::Label),
        "parent_issue" => Some(GroupBy::Parent),
        _ => None,
    }
}

fn sub_group(v: &str) -> Option<SubGroup> {
    match v {
        "none" => Some(SubGroup::None),
        "status" => Some(SubGroup::Status),
        "priority" => Some(SubGroup::Priority),
        "assignee" => Some(SubGroup::Assignee),
        "project" => Some(SubGroup::Project),
        _ => None,
    }
}

fn order_by(v: &str) -> Option<OrderBy> {
    match v {
        "manual" => Some(OrderBy::Manual),
        "priority" => Some(OrderBy::Priority),
        "due_date" => Some(OrderBy::DueDate),
        "created_at" => Some(OrderBy::Created),
        "updated_at" => Some(OrderBy::Updated),
        _ => None,
    }
}

fn completed_filter(v: &str) -> Option<CompletedFilter> {
    match v {
        "" => Some(CompletedFilter::Hide),
        "past_day" => Some(CompletedFilter::PastDay),
        "past_week" => Some(CompletedFilter::PastWeek),
        "past_month" => Some(CompletedFilter::PastMonth),
        "all_time" | "all_completed" => Some(CompletedFilter::All),
        _ => None,
    }
}

fn card_property(v: &str) -> Option<CardProperty> {
    match v {
        "id" => Some(CardProperty::Id),
        "priority" => Some(CardProperty::Priority),
        "labels" => Some(CardProperty::Labels),
        "assignee" => Some(CardProperty::Assignee),
        "project" => Some(CardProperty::Project),
        "due_date" => Some(CardProperty::DueDate),
        "status" => Some(CardProperty::Status),
        "estimate" => Some(CardProperty::Estimate),
        "created_at" => Some(CardProperty::Created),
        "updated_at" => Some(CardProperty::Updated),
        _ => None,
    }
}

/// The query keys the desktop models (everything else is passed through).
pub const MODELLED_KEYS: [&str; 14] = [
    "view_mode",
    "group_by",
    "sub_group_by",
    "order_by",
    "completed_filter",
    "show_sub_issues",
    "show_empty_groups",
    "visible_properties",
    "lane_ids",
    "assignee_ids",
    "label_ids",
    "project_ids",
    "priority",
    "q",
];

/// Encode the working copy over `raw`: modelled keys are overwritten, every
/// other key survives. Unknown property names and ids that couldn't be
/// resolved when decoding are written back.
pub fn encode(state: &BoardState, raw: &RawQuery, mirror: &Mirror) -> RawQuery {
    let mut out = raw.clone();
    let d = &state.display;
    let raw_get = |k: &str| raw.get(k).map(String::as_str);

    out.insert("view_mode".into(), enc_view_mode(d.view_mode).into());
    out.insert("group_by".into(), enc_group_by(d.group_by).into());
    out.insert("sub_group_by".into(), enc_sub_group(d.sub_group).into());
    out.insert("order_by".into(), enc_order_by(d.order_by).into());

    // Keep the stored spelling when it already means the same thing.
    let cf = d.completed_filter();
    let cf_str = match raw_get("completed_filter") {
        Some(r) if completed_filter(r) == Some(cf) => r.to_string(),
        _ => enc_completed(cf).to_string(),
    };
    out.insert("completed_filter".into(), cf_str);

    for (key, val) in [
        ("show_sub_issues", d.show_sub_issues),
        ("show_empty_groups", d.show_empty_groups),
    ] {
        let s = match raw_get(key) {
            Some(r) if boolean(r) == Some(val) => r.to_string(),
            _ => val.to_string(),
        };
        out.insert(key.into(), s);
    }

    let mut props: Vec<String> = d
        .properties
        .iter()
        .map(|p| enc_card_property(*p).to_string())
        .collect();
    if let Some(r) = raw_get("visible_properties") {
        props.extend(
            csv(r)
                .into_iter()
                .filter(|n| card_property(n).is_none())
                .map(String::from),
        );
    }
    out.insert("visible_properties".into(), props.join(","));

    let f = &state.filters;
    let mut put = |key: &str, parts: Vec<String>| {
        if parts.is_empty() && !raw.contains_key(key) {
            return;
        }
        out.insert(key.into(), parts.join(","));
    };
    put(
        "lane_ids",
        remote_ids(
            &f.statuses,
            raw_get("lane_ids"),
            mirror.lanes.iter().map(|l| (l.remote_id, &l.id)),
        ),
    );
    put(
        "assignee_ids",
        remote_ids(
            &f.assignees,
            raw_get("assignee_ids"),
            mirror.members.iter().map(|m| (m.remote_id, &m.id)),
        ),
    );
    put(
        "label_ids",
        remote_ids(
            &f.labels,
            raw_get("label_ids"),
            mirror.labels.iter().map(|l| (l.remote_id, &l.id)),
        ),
    );

    let mut projects = Vec::new();
    if f.projects.iter().any(String::is_empty) {
        projects.push("none".to_string());
    }
    let real: Vec<String> = f.projects.iter().filter(|p| !p.is_empty()).cloned().collect();
    let raw_real = raw_get("project_ids").map(|v| {
        csv(v)
            .into_iter()
            .filter(|p| *p != "none")
            .collect::<Vec<_>>()
            .join(",")
    });
    projects.extend(remote_ids(
        &real,
        raw_real.as_deref(),
        mirror.projects.iter().map(|p| (p.remote_id, &p.id)),
    ));
    put("project_ids", projects);

    let mut pr: Vec<String> = f
        .priorities
        .iter()
        .filter_map(|n| n.parse::<i32>().ok())
        .map(|n| Priority::from_i32(n).as_rails_str().to_string())
        .collect();
    if let Some(r) = raw_get("priority") {
        pr.extend(
            csv(r)
                .into_iter()
                .filter(|n| Priority::from_rails_str(n).is_err())
                .map(String::from),
        );
    }
    put("priority", pr);

    if !state.search.is_empty() {
        out.insert("q".into(), state.search.clone());
    } else if raw.contains_key("q") {
        out.insert("q".into(), String::new());
    }
    out
}

/// Rails rejects Saved view names longer than this.
pub const MAX_NAME_LEN: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    Blank,
    TooLong,
}

impl NameError {
    pub fn message(self) -> &'static str {
        match self {
            NameError::Blank => "Name can't be blank",
            NameError::TooLong => "Name must be 80 characters or fewer",
        }
    }
}

/// Local check of a new view's name: trimmed, 1 to 80 characters.
pub fn validate_name(name: &str) -> Result<String, NameError> {
    let name = name.trim();
    if name.is_empty() {
        Err(NameError::Blank)
    } else if name.chars().count() > MAX_NAME_LEN {
        Err(NameError::TooLong)
    } else {
        Ok(name.to_string())
    }
}

/// Why "Save view…" is unavailable, or `None` when it is offered.
/// `on_team_board` is true only on a single team's Issues board (not My Issues,
/// project pages and other cross-team or non-board views).
pub fn save_block_reason(on_team_board: bool, read_only: bool) -> Option<&'static str> {
    if !on_team_board {
        Some("Open a single team's Issues board to save a view")
    } else if read_only {
        Some("Read-only token: can't save a view")
    } else {
        None
    }
}

/// Whether the filters hold the Unassigned assignee, which Rails can't store.
pub fn has_unassigned(state: &BoardState) -> bool {
    state.filters.assignees.iter().any(String::is_empty)
}

/// The query for a brand-new view made from `state`. There is no stored query
/// to preserve, so it is encoded over an empty one; Unassigned has no remote
/// id and is dropped by the encode.
pub fn new_view_query(state: &BoardState, mirror: &Mirror) -> RawQuery {
    encode(state, &RawQuery::new(), mirror)
}

/// Local ids -> remote ids (records without a remote id become a local-id
/// placeholder, resolved at push), then the raw ids that resolve to no mirrored record, so they survive.
fn remote_ids<'a>(
    local: &[String],
    raw: Option<&str>,
    records: impl Iterator<Item = (Option<i64>, &'a String)>,
) -> Vec<String> {
    let records: Vec<_> = records.collect();
    let mut out: Vec<String> = local
        .iter()
        .filter_map(|id| records.iter().find(|(_, l)| *l == id))
        .map(|(r, l)| match r {
            Some(r) => r.to_string(),
            None => format!("{LOCAL_PLACEHOLDER}{l}"),
        })
        .collect();
    for part in raw.map(csv).unwrap_or_default() {
        let resolves = match placeholder_local_id(part) {
            Some(local) => records.iter().any(|(_, l)| l.as_str() == local),
            None => part
                .parse::<i64>()
                .ok()
                .is_some_and(|rid| records.iter().any(|(r, _)| *r == Some(rid))),
        };
        if !resolves {
            out.push(part.to_string());
        }
    }
    out
}

fn enc_view_mode(v: ViewMode) -> &'static str {
    match v {
        ViewMode::Board => "board",
        ViewMode::List => "list",
    }
}

fn enc_group_by(v: GroupBy) -> &'static str {
    match v {
        GroupBy::None => "none",
        GroupBy::Status => "status",
        GroupBy::Assignee => "assignee",
        GroupBy::Project => "project",
        GroupBy::Priority => "priority",
        GroupBy::Label => "label",
        GroupBy::Parent => "parent_issue",
    }
}

fn enc_sub_group(v: SubGroup) -> &'static str {
    match v {
        SubGroup::None => "none",
        SubGroup::Status => "status",
        SubGroup::Priority => "priority",
        SubGroup::Assignee => "assignee",
        SubGroup::Project => "project",
    }
}

fn enc_order_by(v: OrderBy) -> &'static str {
    match v {
        OrderBy::Manual => "manual",
        OrderBy::Priority => "priority",
        OrderBy::DueDate => "due_date",
        OrderBy::Created => "created_at",
        OrderBy::Updated => "updated_at",
    }
}

fn enc_completed(v: CompletedFilter) -> &'static str {
    match v {
        CompletedFilter::Hide => "",
        CompletedFilter::PastDay => "past_day",
        CompletedFilter::PastWeek => "past_week",
        CompletedFilter::PastMonth => "past_month",
        CompletedFilter::All => "all_time",
    }
}

fn enc_card_property(v: CardProperty) -> &'static str {
    match v {
        CardProperty::Id => "id",
        CardProperty::Priority => "priority",
        CardProperty::Labels => "labels",
        CardProperty::Assignee => "assignee",
        CardProperty::Project => "project",
        CardProperty::DueDate => "due_date",
        CardProperty::Status => "status",
        CardProperty::Estimate => "estimate",
        CardProperty::Created => "created_at",
        CardProperty::Updated => "updated_at",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(pairs: &[(&str, &str)]) -> RawQuery {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn lane(id: &str, rid: Option<i64>) -> Lane {
        Lane {
            id: id.into(),
            name: id.into(),
            color: None,
            position: 0,
            team_id: "t".into(),
            remote_id: rid,
            category: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn member(id: &str, rid: Option<i64>) -> Member {
        Member {
            id: id.into(),
            remote_id: rid,
            team_id: "t".into(),
            name: id.into(),
            email: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn label(id: &str, rid: Option<i64>) -> Label {
        Label {
            id: id.into(),
            remote_id: rid,
            team_id: "t".into(),
            name: id.into(),
            color: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn project(id: &str, rid: Option<i64>) -> Project {
        Project {
            id: id.into(),
            remote_id: rid,
            team_id: "t".into(),
            name: id.into(),
            description: None,
            status: None,
            priority: 0,
            lead_id: None,
            start_date: None,
            due_date: None,
            roadmap_commitment: None,
            velocity_score: 0,
            completed_issues_count: 0,
            total_issues_count: 0,
            progress_percentage: 0,
            behind_schedule: false,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    struct Fx {
        lanes: Vec<Lane>,
        members: Vec<Member>,
        labels: Vec<Label>,
        projects: Vec<Project>,
    }

    impl Fx {
        fn new() -> Self {
            Fx {
                lanes: vec![lane("l-a", Some(10)), lane("l-b", Some(11))],
                members: vec![member("m-a", Some(20)), member("m-b", Some(21))],
                // "lb-new" was created offline: no remote id, so no remote id
                // can resolve to it.
                labels: vec![label("lb-a", Some(30)), label("lb-new", None)],
                projects: vec![project("p-a", Some(40))],
            }
        }

        fn mirror(&self) -> Mirror<'_> {
            Mirror {
                lanes: &self.lanes,
                members: &self.members,
                labels: &self.labels,
                projects: &self.projects,
            }
        }

        fn decode_with(&self, query: &RawQuery, local: &BoardState) -> Decoded {
            decode(query, &self.mirror(), local)
        }

        fn decode(&self, query: &RawQuery) -> Decoded {
            self.decode_with(query, &BoardState::default())
        }
    }

    #[test]
    fn decodes_display_enums() {
        let d = Fx::new().decode(&q(&[
            ("view_mode", "list"),
            ("group_by", "parent_issue"),
            ("sub_group_by", "assignee"),
            ("order_by", "created_at"),
        ]));
        let o = d.state.display;
        assert_eq!(o.view_mode, ViewMode::List);
        assert_eq!(o.group_by, GroupBy::Parent);
        assert_eq!(o.sub_group, SubGroup::Assignee);
        assert_eq!(o.order_by, OrderBy::Created);

        let d = Fx::new().decode(&q(&[("order_by", "updated_at"), ("group_by", "none")]));
        assert_eq!(d.state.display.order_by, OrderBy::Updated);
        assert_eq!(d.state.display.group_by, GroupBy::None);
    }

    #[test]
    fn decodes_completed_windows() {
        let cases = [
            ("", CompletedFilter::Hide),
            ("past_day", CompletedFilter::PastDay),
            ("past_week", CompletedFilter::PastWeek),
            ("past_month", CompletedFilter::PastMonth),
            ("all_time", CompletedFilter::All),
            ("all_completed", CompletedFilter::All),
        ];
        for (raw, want) in cases {
            let d = Fx::new().decode(&q(&[("completed_filter", raw)]));
            assert_eq!(d.state.display.completed_filter(), want, "{raw:?}");
        }
    }

    #[test]
    fn decodes_booleans() {
        let d = Fx::new().decode(&q(&[("show_sub_issues", "0"), ("show_empty_groups", "1")]));
        assert!(!d.state.display.show_sub_issues);
        assert!(d.state.display.show_empty_groups);

        let d = Fx::new().decode(&q(&[
            ("show_sub_issues", "true"),
            ("show_empty_groups", "false"),
        ]));
        assert!(d.state.display.show_sub_issues);
        assert!(!d.state.display.show_empty_groups);
    }

    #[test]
    fn decodes_visible_properties_ignoring_unknown() {
        let d = Fx::new().decode(&q(&[(
            "visible_properties",
            "id,created_at,updated_at,time_in_status,pull_requests_and_commits,due_date",
        )]));
        assert_eq!(
            d.state.display.properties,
            vec![
                CardProperty::Id,
                CardProperty::Created,
                CardProperty::Updated,
                CardProperty::DueDate
            ]
        );
    }

    #[test]
    fn translates_remote_ids_to_local_and_drops_unresolved() {
        let d = Fx::new().decode(&q(&[
            ("lane_ids", "11,10,999"),
            ("assignee_ids", "21,abc,77"),
            ("label_ids", "30,31"),
            ("project_ids", "40,41"),
        ]));
        let f = d.state.filters;
        assert_eq!(f.statuses, vec!["l-b", "l-a"]);
        assert_eq!(f.assignees, vec!["m-b"]);
        assert_eq!(f.labels, vec!["lb-a"]);
        assert_eq!(f.projects, vec!["p-a"]);
    }

    #[test]
    fn project_none_is_no_project() {
        let d = Fx::new().decode(&q(&[("project_ids", "none,40")]));
        assert_eq!(
            d.state.filters.projects,
            vec![String::new(), "p-a".to_string()]
        );
    }

    #[test]
    fn priority_names_map_to_numbers() {
        let d = Fx::new().decode(&q(&[("priority", "urgent,low,no_priority,bogus")]));
        assert_eq!(d.state.filters.priorities, vec!["0", "3", "4"]);
    }

    #[test]
    fn q_is_view_search() {
        let d = Fx::new().decode(&q(&[("q", "login bug")]));
        assert_eq!(d.state.search, "login bug");
    }

    #[test]
    fn raw_query_kept_and_unknown_keys_ignored() {
        let raw = q(&[
            ("creator_ids", "5"),
            ("future_key", "x"),
            ("lane_ids", "999"),
            ("show_dependencies", "1"),
        ]);
        let d = Fx::new().decode(&raw);
        assert_eq!(d.raw, raw);
        // Only the (unresolvable) lane filter is modelled; nothing else moved.
        assert_eq!(d.state, BoardState::default());
    }

    #[test]
    fn missing_keys_fall_back_to_local() {
        let mut local = BoardState::default();
        local.display.view_mode = ViewMode::List;
        local.display.group_by = GroupBy::Label;
        local.display.show_sub_issues = false;
        local.display.set_completed_filter(CompletedFilter::PastWeek);
        local.filters.statuses = vec!["l-a".into()];
        local.filters.priorities = vec!["1".into()];
        local.search = "mine".into();

        let d = Fx::new().decode_with(&q(&[("order_by", "priority")]), &local);

        let mut want = local.clone();
        want.display.order_by = OrderBy::Priority;
        assert_eq!(d.state, want);
    }

    #[test]
    fn bad_enum_values_fall_back_without_panicking() {
        let mut local = BoardState::default();
        local.display.view_mode = ViewMode::List;
        local.display.group_by = GroupBy::Project;
        local.display.set_completed_filter(CompletedFilter::PastDay);

        let d = Fx::new().decode_with(
            &q(&[
                ("view_mode", "kanban"),
                ("group_by", "???"),
                ("sub_group_by", "parent_issue"),
                ("order_by", ""),
                ("completed_filter", "forever"),
                ("show_sub_issues", "maybe"),
            ]),
            &local,
        );
        assert_eq!(d.state, local);
    }

    #[test]
    fn parses_stored_query_json_into_a_string_map() {
        let raw = parse_stored_query(r#"{"view_mode":"list","show_sub_issues":true,"n":3,"x":null}"#);
        assert_eq!(raw["view_mode"], "list");
        assert_eq!(raw["show_sub_issues"], "true");
        assert_eq!(raw["n"], "3");
        assert!(!raw.contains_key("x"));
        assert!(parse_stored_query("not json").is_empty());
        assert!(parse_stored_query("[1]").is_empty());
    }

    #[test]
    fn open_view_decodes_stored_row_into_a_working_copy() {
        let sv = crate::db::saved_views::SavedView {
            id: "v1".into(),
            remote_id: Some(9),
            team_id: "t1".into(),
            name: "Urgent".into(),
            query: r#"{"view_mode":"list","q":"bug","creator_ids":"4"}"#.into(),
            position: 0,
            updated_at: String::new(),
        };
        let fx = Fx::new();
        let ov = OpenView::open(&sv, &fx.mirror(), &BoardState::default());
        assert_eq!((ov.view_id.as_str(), ov.name.as_str()), ("v1", "Urgent"));
        assert_eq!(ov.state.display.view_mode, ViewMode::List);
        assert_eq!(ov.state.search, "bug");
        assert_eq!(ov.raw["creator_ids"], "4");
    }

    #[test]
    fn open_view_applies_only_on_its_teams_board_or_issue_detail() {
        let sv = crate::db::saved_views::SavedView {
            id: "v1".into(),
            remote_id: None,
            team_id: "t1".into(),
            name: "V".into(),
            query: "{}".into(),
            position: 0,
            updated_at: String::new(),
        };
        let ov = OpenView::open(&sv, &Fx::new().mirror(), &BoardState::default());
        assert!(ov.stays_open(Some("t1"), true));
        assert!(!ov.stays_open(Some("t2"), true));
        assert!(!ov.stays_open(Some("t1"), false));
        assert!(!ov.stays_open(None, true));
    }

    fn open_with(fx: &Fx, pairs: &[(&str, &str)]) -> OpenView {
        let query = serde_json::to_string(&q(pairs)).unwrap();
        let sv = crate::db::saved_views::SavedView {
            id: "v1".into(),
            remote_id: None,
            team_id: "t".into(),
            name: "V".into(),
            query,
            position: 0,
            updated_at: String::new(),
        };
        OpenView::open(&sv, &fx.mirror(), &BoardState::default())
    }

    #[test]
    fn encode_round_trips_every_mapped_key() {
        let fx = Fx::new();
        let pairs = [
            ("view_mode", "list"),
            ("group_by", "parent_issue"),
            ("sub_group_by", "priority"),
            ("order_by", "due_date"),
            ("completed_filter", "past_week"),
            ("show_sub_issues", "false"),
            ("show_empty_groups", "true"),
            ("visible_properties", "id,labels,due_date"),
            ("lane_ids", "10,11"),
            ("assignee_ids", "21"),
            ("label_ids", "30"),
            ("project_ids", "none,40"),
            ("priority", "urgent,no_priority"),
            ("q", "bug"),
        ];
        let ov = open_with(&fx, &pairs);
        assert_eq!(ov.encoded(&fx.mirror()), q(&pairs));
        assert!(!ov.is_modified(&fx.mirror()));
    }

    #[test]
    fn encode_writes_rails_names() {
        let fx = Fx::new();
        let mut ov = open_with(&fx, &[]);
        ov.state.filters.priorities = vec!["0".into(), "4".into()];
        ov.state.filters.projects = vec![String::new()];
        ov.state.display.group_by = GroupBy::Parent;
        let e = ov.encoded(&fx.mirror());
        assert_eq!(e["priority"], "urgent,no_priority");
        assert_eq!(e["project_ids"], "none");
        assert_eq!(e["group_by"], "parent_issue");
    }

    #[test]
    fn encode_keeps_unknown_keys_properties_and_unresolved_ids() {
        let fx = Fx::new();
        let mut ov = open_with(
            &fx,
            &[
                ("creator_ids", "4"),
                ("show_dependencies", "true"),
                ("visible_properties", "id,time_in_status,pull_requests_and_commits"),
                ("lane_ids", "10,999"),
                ("project_ids", "none,777"),
                ("priority", "high,bogus"),
            ],
        );
        ov.state.filters.statuses = vec!["l-b".into()];
        ov.state.display.properties = vec![CardProperty::Labels];
        let e = ov.encoded(&fx.mirror());
        assert_eq!(e["creator_ids"], "4");
        assert_eq!(e["show_dependencies"], "true");
        assert_eq!(e["visible_properties"], "labels,time_in_status,pull_requests_and_commits");
        assert_eq!(e["lane_ids"], "11,999");
        assert_eq!(e["project_ids"], "none,777");
        assert_eq!(e["priority"], "high,bogus");
    }

    #[test]
    fn encode_writes_offline_records_as_placeholders_and_decode_resolves_them() {
        let fx = Fx::new();
        let mut ov = open_with(&fx, &[("label_ids", "30")]);
        ov.state.filters.labels = vec!["lb-a".into(), "lb-new".into()];
        let e = ov.encoded(&fx.mirror());
        assert_eq!(e["label_ids"], "30,local:lb-new");
        let d = fx.decode(&e);
        assert_eq!(d.state.filters.labels, vec!["lb-a", "lb-new"]);
        // Re-encoding a stored placeholder neither duplicates nor drops it.
        let mut ov2 = open_with(&fx, &[("label_ids", "local:lb-new")]);
        assert_eq!(ov2.state.filters.labels, vec!["lb-new"]);
        assert_eq!(ov2.encoded(&fx.mirror())["label_ids"], "local:lb-new");
        ov2.state.filters.labels.clear();
        assert_eq!(ov2.encoded(&fx.mirror())["label_ids"], "");
    }

    #[test]
    fn encode_writes_offline_projects_as_placeholders() {
        let mut fx = Fx::new();
        fx.projects.push(project("p-new", None));
        let mut ov = open_with(&fx, &[]);
        ov.state.filters.projects = vec![String::new(), "p-a".into(), "p-new".into()];
        assert_eq!(ov.encoded(&fx.mirror())["project_ids"], "none,40,local:p-new");
        let d = fx.decode(&ov.encoded(&fx.mirror()));
        assert_eq!(d.state.filters.projects, vec![String::new(), "p-a".into(), "p-new".into()]);
    }

    #[test]
    fn modified_follows_the_working_copy_and_clears_on_undo() {
        let fx = Fx::new();
        let mut ov = open_with(&fx, &[("order_by", "priority")]);
        let m = fx.mirror();
        assert!(!ov.is_modified(&m));
        ov.state.display.order_by = OrderBy::Manual;
        assert!(ov.is_modified(&m));
        ov.state.display.order_by = OrderBy::Priority;
        assert!(!ov.is_modified(&m));
        ov.state.search = "x".into();
        assert!(ov.is_modified(&m));
        ov.state.search.clear();
        ov.state.filters.assignees = vec!["m-a".into()];
        assert!(ov.is_modified(&m));
    }

    #[test]
    fn toggling_a_property_off_and_on_is_not_modified() {
        let fx = Fx::new();
        let mut ov = open_with(&fx, &[("visible_properties", "id,priority,labels")]);
        let m = fx.mirror();
        assert!(!ov.is_modified(&m));
        ov.state.display.toggle_property(CardProperty::Id);
        assert!(ov.is_modified(&m));
        ov.state.display.toggle_property(CardProperty::Id);
        assert!(!ov.is_modified(&m));
    }

    #[test]
    fn mark_saved_clears_modified_and_keeps_unknown_keys_in_the_stored_query() {
        let fx = Fx::new();
        let mut ov = open_with(&fx, &[("order_by", "priority"), ("future_key", "x")]);
        let m = fx.mirror();
        ov.state.display.order_by = OrderBy::Manual;
        assert!(ov.is_modified(&m));
        ov.mark_saved(&m);
        assert!(!ov.is_modified(&m));
        assert_eq!(ov.raw["future_key"], "x");
        assert_eq!(ov.raw["order_by"], ov.encoded(&m)["order_by"]);
        // Editing again after saving is Modified relative to the new definition.
        ov.state.display.order_by = OrderBy::Priority;
        assert!(ov.is_modified(&m));
    }

    #[test]
    fn modified_ignores_keys_the_desktop_does_not_model() {
        let fx = Fx::new();
        let mut ov = open_with(&fx, &[("creator_ids", "4")]);
        ov.raw.insert("creator_ids".into(), "5".into());
        assert!(!ov.is_modified(&fx.mirror()));
    }
    #[test]
    fn view_name_is_trimmed_and_must_be_1_to_80_chars() {
        assert_eq!(validate_name("  Bugs  "), Ok("Bugs".to_string()));
        assert_eq!(validate_name("   "), Err(NameError::Blank));
        assert_eq!(validate_name(""), Err(NameError::Blank));
        assert!(validate_name(&"a".repeat(80)).is_ok());
        assert_eq!(validate_name(&"a".repeat(81)), Err(NameError::TooLong));
        // Length counts characters, not bytes.
        assert!(validate_name(&"é".repeat(80)).is_ok());
    }

    #[test]
    fn save_is_offered_only_on_a_team_board_with_a_writable_token() {
        assert_eq!(save_block_reason(true, false), None);
        assert!(save_block_reason(false, false).unwrap().contains("team"));
        assert!(save_block_reason(true, true).unwrap().contains("Read-only"));
        // Not being on a team board is the more useful reason to show.
        assert!(save_block_reason(false, true).unwrap().contains("team"));
    }

    #[test]
    fn new_view_query_drops_unassigned_and_reports_it() {
        let members = [member("m1", Some(5))];
        let mirror = Mirror { lanes: &[], members: &members, labels: &[], projects: &[] };
        let mut state = BoardState::default();
        state.filters.assignees = vec!["m1".into(), String::new()];
        state.search = "crash".into();
        assert!(has_unassigned(&state));
        let query = new_view_query(&state, &mirror);
        assert_eq!(query.get("assignee_ids").map(String::as_str), Some("5"));
        assert_eq!(query.get("q").map(String::as_str), Some("crash"));
        state.filters.assignees = vec!["m1".into()];
        assert!(!has_unassigned(&state));
    }

    fn sv_with(name: &str, pairs: &[(&str, &str)]) -> crate::db::saved_views::SavedView {
        crate::db::saved_views::SavedView {
            id: "v1".into(),
            remote_id: Some(1),
            team_id: "t".into(),
            name: name.into(),
            query: serde_json::to_string(&q(pairs)).unwrap(),
            position: 0,
            updated_at: String::new(),
        }
    }

    #[test]
    fn refresh_of_an_unmodified_view_takes_the_pulled_definition() {
        let fx = Fx::new();
        let mut ov = open_with(&fx, &[("order_by", "priority")]);
        ov.refresh(&sv_with("Renamed", &[("order_by", "due_date"), ("q", "x")]), &fx.mirror(), &BoardState::default());
        assert_eq!(ov.name, "Renamed");
        assert_eq!(ov.state.display.order_by, OrderBy::DueDate);
        assert_eq!(ov.state.search, "x");
        assert!(!ov.is_modified(&fx.mirror()));
    }

    #[test]
    fn refresh_of_a_modified_view_keeps_the_working_copy_and_remeasures() {
        let fx = Fx::new();
        let mut ov = open_with(&fx, &[("order_by", "priority")]);
        ov.state.display.order_by = OrderBy::Manual;
        ov.refresh(&sv_with("New", &[("order_by", "due_date")]), &fx.mirror(), &BoardState::default());
        assert_eq!(ov.name, "New");
        assert_eq!(ov.state.display.order_by, OrderBy::Manual);
        assert!(ov.is_modified(&fx.mirror()));
        // The server now matches the working copy: no longer modified.
        ov.refresh(&sv_with("New", &[("order_by", "manual")]), &fx.mirror(), &BoardState::default());
        assert_eq!(ov.state.display.order_by, OrderBy::Manual);
        assert!(!ov.is_modified(&fx.mirror()));
    }

    #[test]
    fn save_as_new_encodes_the_working_copy_over_the_original_raw_query() {
        let fx = Fx::new();
        let mut ov = open_with(&fx, &[("order_by", "priority"), ("web_only", "keep")]);
        ov.state.display.order_by = OrderBy::Manual;
        ov.state.search = "bug".into();
        let query = ov.encoded(&fx.mirror());
        assert_eq!(query.get("web_only").map(String::as_str), Some("keep"));
        assert_eq!(query.get("order_by").map(String::as_str), Some("manual"));
        assert_eq!(query.get("q").map(String::as_str), Some("bug"));
        // The original's stored definition is untouched.
        assert_eq!(ov.raw.get("order_by").map(String::as_str), Some("priority"));
        assert!(ov.is_modified(&fx.mirror()));
    }

    #[test]
    fn revert_restores_the_saved_version_and_clears_modified() {
        let fx = Fx::new();
        let mut ov = open_with(
            &fx,
            &[("order_by", "priority"), ("lane_ids", "10"), ("q", "bug")],
        );
        let saved = ov.state.clone();
        let m = fx.mirror();
        ov.state.display.order_by = OrderBy::Manual;
        ov.state.filters.statuses = vec!["l-b".into()];
        ov.state.search = "other".into();
        assert!(ov.is_modified(&m));
        ov.revert();
        assert_eq!(ov.state, saved);
        assert!(!ov.is_modified(&m));
    }
}

