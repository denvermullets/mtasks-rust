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

/// Translate comma-separated remote ids to local ids, dropping any that don't
/// resolve (including non-numeric ones).
fn local_ids<'a>(
    v: &str,
    records: impl Iterator<Item = (Option<i64>, &'a String)>,
) -> Vec<String> {
    let records: Vec<_> = records.collect();
    csv(v)
        .into_iter()
        .filter_map(|p| p.parse::<i64>().ok())
        .filter_map(|rid| records.iter().find(|(r, _)| *r == Some(rid)))
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
}
