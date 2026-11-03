//! Recording of Pending changes (local edits to an issue, written to
//! `issue_events` until the server's Activity replaces them) and the wording
//! shared with the display layer. Private to the Activity module. Rows are
//! never enqueued to `sync_queue` themselves.

use rusqlite::{params, Connection, OptionalExtension, Result};

use crate::db::models::{Issue, Priority};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Created,
    Status,
    Priority,
    Assignee,
    Estimate,
    Project,
    DueDate,
    Parent,
    Title,
    Description,
    /// This issue now blocks / no longer blocks another.
    Blocking,
    /// This issue is now / no longer blocked by another.
    BlockedBy,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Created => "created",
            EventKind::Status => "status",
            EventKind::Priority => "priority",
            EventKind::Assignee => "assignee",
            EventKind::Estimate => "estimate",
            EventKind::Project => "project",
            EventKind::DueDate => "due_date",
            EventKind::Parent => "parent",
            EventKind::Title => "title",
            EventKind::Description => "description",
            EventKind::Blocking => "blocking",
            EventKind::BlockedBy => "blocked_by",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "created" => EventKind::Created,
            "status" => EventKind::Status,
            "priority" => EventKind::Priority,
            "assignee" => EventKind::Assignee,
            "estimate" => EventKind::Estimate,
            "project" => EventKind::Project,
            "due_date" => EventKind::DueDate,
            "parent" => EventKind::Parent,
            "title" => EventKind::Title,
            "description" => EventKind::Description,
            "blocking" => EventKind::Blocking,
            "blocked_by" => EventKind::BlockedBy,
            _ => return None,
        })
    }
}

/// Where a change was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Made on this desktop ("You" when no actor is recorded).
    Local,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Local => "local",
        }
    }
}

/// A field change with display-ready values.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub kind: EventKind,
    pub old: Option<String>,
    pub new: Option<String>,
    pub old_ref: Option<String>,
    pub new_ref: Option<String>,
}

impl Change {
    fn plain(kind: EventKind, old: Option<String>, new: Option<String>) -> Self {
        Self {
            kind,
            old,
            new,
            old_ref: None,
            new_ref: None,
        }
    }
}

/// Which id space a lookup refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    Lane,
    Member,
    Project,
    /// Display form is the identifier, e.g. `PROJ-12`.
    Issue,
}

pub(super) fn priority_label(p: i32) -> String {
    match Priority::from_i32(p) {
        Priority::Urgent => "Urgent",
        Priority::High => "High",
        Priority::Medium => "Medium",
        Priority::Low => "Low",
        Priority::None => "None",
    }
    .to_string()
}

/// A change to a field that points at another entity: frozen display names
/// plus the ids to re-resolve them at render time.
fn ref_change(
    kind: EventKind,
    lookup: Lookup,
    old: Option<&str>,
    new: Option<&str>,
    name: &dyn Fn(Lookup, &str) -> Option<String>,
) -> Change {
    let resolve = |id: Option<&str>| -> Option<String> {
        id.map(|i| name(lookup, i).unwrap_or_else(|| "Unknown".to_string()))
    };
    Change {
        kind,
        old: resolve(old),
        new: resolve(new),
        old_ref: old.map(str::to_string),
        new_ref: new.map(str::to_string),
    }
}

/// Pure diff of the tracked fields (mirrors Rails' `TRACKED_ATTRIBUTES`).
/// `name` resolves an id to a display name; ids it can't resolve show as
/// "Unknown".
pub fn diff(
    old: &Issue,
    new: &Issue,
    name: &dyn Fn(Lookup, &str) -> Option<String>,
) -> Vec<Change> {
    let mut out = Vec::new();
    if old.lane_id != new.lane_id {
        out.push(ref_change(
            EventKind::Status,
            Lookup::Lane,
            Some(&old.lane_id),
            Some(&new.lane_id),
            name,
        ));
    }
    if old.assignee_id != new.assignee_id {
        out.push(ref_change(
            EventKind::Assignee,
            Lookup::Member,
            old.assignee_id.as_deref(),
            new.assignee_id.as_deref(),
            name,
        ));
    }
    if old.priority != new.priority {
        out.push(Change::plain(
            EventKind::Priority,
            Some(priority_label(old.priority)),
            Some(priority_label(new.priority)),
        ));
    }
    if old.title != new.title {
        out.push(Change::plain(
            EventKind::Title,
            Some(old.title.clone()),
            Some(new.title.clone()),
        ));
    }
    // Treat None and "" as the same so a blank description edit isn't noise.
    let desc = |i: &Issue| i.description.clone().filter(|d| !d.is_empty());
    if desc(old) != desc(new) {
        out.push(Change::plain(EventKind::Description, None, None));
    }
    if old.estimate != new.estimate {
        out.push(Change::plain(
            EventKind::Estimate,
            old.estimate.map(|e| e.to_string()),
            new.estimate.map(|e| e.to_string()),
        ));
    }
    if old.due_date != new.due_date {
        out.push(Change::plain(
            EventKind::DueDate,
            old.due_date.clone(),
            new.due_date.clone(),
        ));
    }
    if old.project_id != new.project_id {
        out.push(ref_change(
            EventKind::Project,
            Lookup::Project,
            old.project_id.as_deref(),
            new.project_id.as_deref(),
            name,
        ));
    }
    if old.parent_issue_id != new.parent_issue_id {
        out.push(ref_change(
            EventKind::Parent,
            Lookup::Issue,
            old.parent_issue_id.as_deref(),
            new.parent_issue_id.as_deref(),
            name,
        ));
    }
    out
}

/// Human-readable line, worded like Rails' `VersionDescriptionService`.
pub fn describe(kind: EventKind, old: Option<&str>, new: Option<&str>) -> String {
    let or = |v: Option<&str>, d: &'static str| v.unwrap_or(d).to_string();
    match kind {
        EventKind::Created => "created the issue".to_string(),
        EventKind::Status => format!(
            "changed status from {} to {}",
            or(old, "Unknown"),
            or(new, "Unknown")
        ),
        EventKind::Priority => format!(
            "changed priority from {} to {}",
            or(old, "None"),
            or(new, "None")
        ),
        EventKind::Assignee => match (old, new) {
            (Some(o), None) => format!("unassigned {o}"),
            (None, Some(n)) => format!("assigned to {n}"),
            (Some(o), Some(n)) => format!("reassigned from {o} to {n}"),
            (None, None) => "changed the assignee".to_string(),
        },
        EventKind::Estimate => format!(
            "changed estimate from {} to {}",
            or(old, "none"),
            or(new, "none")
        ),
        EventKind::DueDate => format!(
            "changed due date from {} to {}",
            or(old, "none"),
            or(new, "none")
        ),
        EventKind::Project => format!("moved to project {}", or(new, "None")),
        EventKind::Parent => match (old, new) {
            (Some(o), None) => format!("removed parent {o}"),
            (None, Some(n)) => format!("set parent to {n}"),
            (Some(o), Some(n)) => format!("changed parent from {o} to {n}"),
            (None, None) => "changed parent issue".to_string(),
        },
        EventKind::Blocking => match (old, new) {
            (Some(o), None) => format!("no longer blocks {o}"),
            (_, Some(n)) => format!("now blocks {n}"),
            (None, None) => "changed blocking issues".to_string(),
        },
        EventKind::BlockedBy => match (old, new) {
            (Some(o), None) => format!("no longer blocked by {o}"),
            (_, Some(n)) => format!("now blocked by {n}"),
            (None, None) => "changed blocked-by issues".to_string(),
        },
        EventKind::Title => "changed the title".to_string(),
        EventKind::Description => "updated the description".to_string(),
    }
}

fn lookup_name(conn: &Connection, lookup: Lookup, id: &str) -> Option<String> {
    let sql = match lookup {
        Lookup::Lane => "SELECT name FROM lanes WHERE id = ?1",
        Lookup::Member => "SELECT name FROM members WHERE id = ?1",
        Lookup::Project => "SELECT name FROM projects WHERE id = ?1",
        Lookup::Issue => {
            "SELECT t.identifier || '-' || i.team_number FROM issues i
             JOIN teams t ON t.id = i.team_id WHERE i.id = ?1"
        }
    };
    conn.query_row(sql, params![id], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
}

fn insert_event(
    conn: &Connection,
    issue_id: &str,
    actor_id: Option<&str>,
    source: Source,
    change: &Change,
    created_at: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO issue_events
            (issue_id, actor_id, source, kind, old_value, new_value, old_ref, new_ref, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            issue_id,
            actor_id,
            source.as_str(),
            change.kind.as_str(),
            change.old,
            change.new,
            change.old_ref,
            change.new_ref,
            created_at
        ],
    )?;
    Ok(())
}

/// Record a dependency edge being added/removed on both issues it touches.
/// `blocking` blocks `blocked`; each side gets its own wording.
pub fn record_dependency(
    conn: &Connection,
    blocking: &str,
    blocked: &str,
    added: bool,
    source: Source,
    created_at: &str,
) -> Result<()> {
    let name = |l: Lookup, id: &str| lookup_name(conn, l, id);
    let side = |kind: EventKind, other: &str| {
        let (old, new) = if added {
            (None, Some(other))
        } else {
            (Some(other), None)
        };
        ref_change(kind, Lookup::Issue, old, new, &name)
    };
    let c = side(EventKind::Blocking, blocked);
    insert_event(conn, blocking, None, source, &c, created_at)?;
    let c = side(EventKind::BlockedBy, blocking);
    insert_event(conn, blocked, None, source, &c, created_at)?;
    Ok(())
}

/// Diff `old` against `new` and record one Pending change per changed tracked field.
/// Returns the number of Pending changes written.
pub fn record_changes(
    conn: &Connection,
    old: &Issue,
    new: &Issue,
    source: Source,
    created_at: &str,
) -> Result<usize> {
    let changes = diff(old, new, &|l, id| lookup_name(conn, l, id));
    for c in &changes {
        insert_event(conn, &new.id, None, source, c, created_at)?;
    }
    Ok(changes.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue() -> Issue {
        Issue {
            id: "i1".into(),
            title: "T".into(),
            description: None,
            priority: 4,
            team_number: Some(1),
            due_date: None,
            estimate: None,
            started_at: None,
            completed_at: None,
            canceled_at: None,
            lane_id: "l1".into(),
            team_id: "t1".into(),
            project_id: None,
            parent_issue_id: None,
            assignee_id: None,
            creator_id: None,
            remote_id: None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
        }
    }

    fn names(l: Lookup, id: &str) -> Option<String> {
        match (l, id) {
            (Lookup::Lane, "l1") => Some("Backlog".into()),
            (Lookup::Lane, "l2") => Some("In Progress".into()),
            (Lookup::Member, "m1") => Some("Ann".into()),
            (Lookup::Project, "p1") => Some("Proj".into()),
            (Lookup::Issue, "i0") => Some("T-1".into()),
            _ => None,
        }
    }

    fn lines(old: &Issue, new: &Issue) -> Vec<String> {
        diff(old, new, &names)
            .iter()
            .map(|c| describe(c.kind, c.old.as_deref(), c.new.as_deref()))
            .collect()
    }

    #[test]
    fn no_change_no_events() {
        assert!(diff(&issue(), &issue(), &names).is_empty());
    }

    #[test]
    fn describes_each_tracked_field() {
        let old = issue();
        let mut new = issue();
        new.lane_id = "l2".into();
        new.priority = 1;
        new.assignee_id = Some("m1".into());
        new.estimate = Some(3.0);
        new.due_date = Some("2026-02-01".into());
        new.project_id = Some("p1".into());
        new.parent_issue_id = Some("i0".into());
        new.title = "New".into();
        new.description = Some("hi".into());
        assert_eq!(
            lines(&old, &new),
            vec![
                "changed status from Backlog to In Progress",
                "assigned to Ann",
                "changed priority from None to High",
                "changed the title",
                "updated the description",
                "changed estimate from none to 3",
                "changed due date from none to 2026-02-01",
                "moved to project Proj",
                "set parent to T-1",
            ]
        );
    }

    #[test]
    fn assignee_variants_and_unknown_ids() {
        assert_eq!(
            describe(EventKind::Assignee, Some("A"), None),
            "unassigned A"
        );
        assert_eq!(
            describe(EventKind::Assignee, Some("A"), Some("B")),
            "reassigned from A to B"
        );
        let mut new = issue();
        new.lane_id = "gone".into();
        assert_eq!(
            lines(&issue(), &new),
            vec!["changed status from Backlog to Unknown"]
        );
    }

    #[test]
    fn empty_description_equals_none() {
        let mut new = issue();
        new.description = Some(String::new());
        assert!(diff(&issue(), &new, &names).is_empty());
    }
}
