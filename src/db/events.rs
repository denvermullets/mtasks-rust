//! Local-only issue activity history. The Rails API exposes no versions
//! endpoint, so changes are recorded here as they are written (desktop edits)
//! or pulled (sync). Never enqueued to `sync_queue`.

use rusqlite::{params, Connection, OptionalExtension, Result, Row};

use super::models::{Comment, Issue, Priority};

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
    /// Seen on sync pull ("Someone" when no actor is recorded).
    Sync,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Local => "local",
            Source::Sync => "sync",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct IssueEvent {
    pub id: i64,
    pub issue_id: String,
    pub actor_id: Option<String>,
    pub source: String,
    pub kind: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    /// Ids behind `old_value`/`new_value` (lane/member/project/issue), so the
    /// current name can be shown even after a rename. The values stay as the
    /// frozen fallback.
    pub old_ref: Option<String>,
    pub new_ref: Option<String>,
    pub created_at: String,
    /// Resolved actor name, filled by `resolve_events`.
    pub actor_name: Option<String>,
}

impl IssueEvent {
    pub fn from_row(row: &Row<'_>) -> Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            issue_id: row.get("issue_id")?,
            actor_id: row.get("actor_id")?,
            source: row.get("source")?,
            kind: row.get("kind")?,
            old_value: row.get("old_value")?,
            new_value: row.get("new_value")?,
            old_ref: row.get("old_ref")?,
            new_ref: row.get("new_ref")?,
            created_at: row.get("created_at")?,
            actor_name: None,
        })
    }

    /// Fallback actor label when no member name resolves.
    pub fn default_actor(&self) -> &'static str {
        if self.source == "sync" {
            "Someone"
        } else {
            "You"
        }
    }

    pub fn description(&self) -> String {
        match EventKind::parse(&self.kind) {
            Some(kind) => describe(kind, self.old_value.as_deref(), self.new_value.as_deref()),
            None => "updated the issue".to_string(),
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

fn priority_label(p: i32) -> String {
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

/// Record a "created" event for an issue, dated its `created_at`.
pub fn record_created(conn: &Connection, issue: &Issue, source: Source) -> Result<()> {
    insert_event(
        conn,
        &issue.id,
        issue.creator_id.as_deref(),
        source,
        &Change::plain(EventKind::Created, None, None),
        &issue.created_at,
    )?;
    // Born with a parent (sub-issue created from the form).
    if let Some(parent) = &issue.parent_issue_id {
        let change = ref_change(
            EventKind::Parent,
            Lookup::Issue,
            None,
            Some(parent),
            &|l, id| lookup_name(conn, l, id),
        );
        insert_event(conn, &issue.id, None, source, &change, &issue.created_at)?;
    }
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

/// Diff `old` against `new` and record one event per changed tracked field.
/// Returns the number of events written.
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

pub fn events_for_issue(conn: &Connection, issue_id: &str) -> Result<Vec<IssueEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, issue_id, actor_id, source, kind, old_value, new_value, old_ref, new_ref,
                created_at
         FROM issue_events WHERE issue_id = ?1 ORDER BY created_at, id",
    )?;
    let rows = stmt.query_map(params![issue_id], IssueEvent::from_row)?;
    rows.collect()
}

/// Swap frozen names for current ones where the referenced entity still
/// exists (falls back to the frozen string otherwise), and resolve actors:
/// a recorded member, else for local changes the signed-in user.
pub fn resolve_events(
    conn: &Connection,
    events: &mut [IssueEvent],
    current_user_remote_id: Option<i64>,
) {
    let me = current_user_remote_id
        .and_then(|rid| {
            super::queries::find_member_by_remote_id(conn, rid)
                .ok()
                .flatten()
        })
        .map(|m| m.name);
    for e in events {
        let lookup = match EventKind::parse(&e.kind) {
            Some(EventKind::Status) => Some(Lookup::Lane),
            Some(EventKind::Assignee) => Some(Lookup::Member),
            Some(EventKind::Project) => Some(Lookup::Project),
            Some(EventKind::Parent | EventKind::Blocking | EventKind::BlockedBy) => {
                Some(Lookup::Issue)
            }
            _ => None,
        };
        if let Some(l) = lookup {
            if let Some(n) = e.old_ref.as_deref().and_then(|id| lookup_name(conn, l, id)) {
                e.old_value = Some(n);
            }
            if let Some(n) = e.new_ref.as_deref().and_then(|id| lookup_name(conn, l, id)) {
                e.new_value = Some(n);
            }
        }
        e.actor_name = match &e.actor_id {
            Some(aid) => lookup_name(conn, Lookup::Member, aid),
            None if e.source == "local" => me.clone(),
            None => None,
        };
    }
}

/// `events_for_issue` with names/actors resolved for display.
pub fn events_for_issue_resolved(
    conn: &Connection,
    issue_id: &str,
    current_user_remote_id: Option<i64>,
) -> Result<Vec<IssueEvent>> {
    let mut evs = events_for_issue(conn, issue_id)?;
    resolve_events(conn, &mut evs, current_user_remote_id);
    Ok(evs)
}

/// One entry of the merged activity stream.
#[derive(Debug)]
pub enum TimelineItem<'a> {
    Event(&'a IssueEvent),
    /// A top-level comment with all its replies, as `(depth, comment)` in
    /// display order. Kept together so reply nesting isn't broken.
    Thread(Vec<(usize, &'a Comment)>),
}

impl TimelineItem<'_> {
    fn sort_key(&self) -> &str {
        match self {
            TimelineItem::Event(e) => &e.created_at,
            TimelineItem::Thread(t) => &t[0].1.created_at,
        }
    }
}

/// Interleave events with comment threads, oldest first (stable: events come
/// before a thread with an identical timestamp).
pub fn timeline<'a>(events: &'a [IssueEvent], comments: &'a [Comment]) -> Vec<TimelineItem<'a>> {
    let mut items: Vec<TimelineItem<'a>> = events.iter().map(TimelineItem::Event).collect();
    let mut current: Vec<(usize, &Comment)> = Vec::new();
    for (depth, c) in Comment::thread_order(comments) {
        if depth == 0 && !current.is_empty() {
            items.push(TimelineItem::Thread(std::mem::take(&mut current)));
        }
        current.push((depth, c));
    }
    if !current.is_empty() {
        items.push(TimelineItem::Thread(current));
    }
    items.sort_by(|a, b| a.sort_key().cmp(b.sort_key()));
    items
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
            archived_at: None,
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

    fn ev(id: i64, at: &str) -> IssueEvent {
        IssueEvent {
            id,
            issue_id: "i1".into(),
            actor_id: None,
            source: "local".into(),
            kind: "created".into(),
            old_value: None,
            new_value: None,
            old_ref: None,
            new_ref: None,
            created_at: at.into(),
            actor_name: None,
        }
    }

    fn cm(id: &str, parent: Option<&str>, at: &str) -> Comment {
        Comment {
            id: id.into(),
            remote_id: None,
            issue_id: Some("i1".into()),
            project_id: None,
            author_id: None,
            body: String::new(),
            parent_id: parent.map(str::to_string),
            created_at: at.into(),
            updated_at: at.into(),
        }
    }

    #[test]
    fn timeline_interleaves_and_keeps_threads_whole() {
        let events = vec![ev(1, "2026-01-01"), ev(2, "2026-01-03")];
        let comments = vec![
            cm("a", None, "2026-01-02"),
            cm("a1", Some("a"), "2026-01-04"),
            cm("b", None, "2026-01-05"),
        ];
        let items = timeline(&events, &comments);
        let shape: Vec<String> = items
            .iter()
            .map(|i| match i {
                TimelineItem::Event(e) => format!("e{}", e.id),
                TimelineItem::Thread(t) => t
                    .iter()
                    .map(|(_, c)| c.id.as_str())
                    .collect::<Vec<_>>()
                    .join("+"),
            })
            .collect();
        assert_eq!(shape, vec!["e1", "a+a1", "e2", "b"]);
    }
}
