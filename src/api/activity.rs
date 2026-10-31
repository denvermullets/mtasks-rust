//! DTOs for the issue Activity feed (`GET .../issues/:id/activity`).

// Unused until the store and fetch routine land (JRST-131+); drop then.

use reqwest::header::HeaderMap;
use serde::Deserialize;

use super::{encode_query_value, ApiClient, ApiError};

/// Entry types the desktop shows. `comments` is left out (comments have their
/// own endpoint).
const TYPES: &str = "versions,dependencies,references";

/// Server cap on `limit`.
pub const MAX_LIMIT: u32 = 500;

/// One page of the feed (oldest first within the page).
#[derive(Debug, PartialEq)]
pub struct ActivityPage {
    pub entries: Vec<ActivityEntry>,
    /// `X-Next-Before-Id`: pass as `before_id` to get the next older page.
    /// `None` when nothing older exists.
    pub next_before_id: Option<String>,
    /// `X-Server-Time`. Absent on the unknown-issue 404 (that is an `Err`).
    pub server_time: Option<String>,
}

/// Query options for [`list`].
#[derive(Debug, Default, Clone)]
pub struct ActivityParams<'a> {
    /// Entry cursor (`"version:440"`) to page back from.
    pub before_id: Option<&'a str>,
    /// Strict `created_at > since` (ISO8601).
    pub since: Option<&'a str>,
    /// Newest `limit` matches; clamped to [`MAX_LIMIT`].
    pub limit: Option<u32>,
}

/// `GET /api/v1/teams/:team_id/issues/:issue_id/activity`. An unknown issue
/// (or one that left the team) is `ApiError::NotFound`; so is an unknown
/// `before_id`. Entry types we don't model are skipped, not an error.
pub async fn list(
    client: &ApiClient,
    team_id: i64,
    issue_id: i64,
    params: &ActivityParams<'_>,
) -> Result<ActivityPage, ApiError> {
    let path = list_path(team_id, issue_id, params);
    let (entries, headers) = client.get_list_with_headers(&path).await?;
    let (next_before_id, server_time) = page_meta(&headers);
    Ok(ActivityPage {
        entries,
        next_before_id,
        server_time,
    })
}

fn list_path(team_id: i64, issue_id: i64, p: &ActivityParams<'_>) -> String {
    let mut path = format!("api/v1/teams/{team_id}/issues/{issue_id}/activity?types={TYPES}");
    if let Some(b) = p.before_id {
        path.push_str(&format!("&before_id={}", encode_query_value(b)));
    }
    if let Some(s) = p.since {
        path.push_str(&format!("&since={}", encode_query_value(s)));
    }
    if let Some(l) = p.limit {
        path.push_str(&format!("&limit={}", l.min(MAX_LIMIT)));
    }
    path
}

/// `(X-Next-Before-Id, X-Server-Time)`; blank values are `None`.
fn page_meta(headers: &HeaderMap) -> (Option<String>, Option<String>) {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    (get("x-next-before-id"), get("x-server-time"))
}

/// One feed entry, tagged by the JSON `type`: `Version` (field changes),
/// `Reference` (incoming mention) or `Dependency` (relation added/removed).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActivityEntry {
    Version(VersionEntry),
    Reference(ReferenceEntry),
    Dependency(DependencyEntry),
}

/// Person behind an entry; the server sends `null` when unknown.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ActivityActor {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct VersionEntry {
    /// `"version:<id>"`; `id` alone collides across entry types.
    pub cursor: String,
    pub created_at: String,
    #[serde(default)]
    pub actor: Option<ActivityActor>,
    /// Server-rendered HTML (`<strong>` tags); strip before display.
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub changes: Vec<FieldChange>,
}

/// An incoming reference ("mentioned in JRST-12"). Unlike the other variants
/// the person is `user` (nullable), and the server sends no description.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ReferenceEntry {
    /// `"reference:<id>"`.
    pub cursor: String,
    pub created_at: String,
    #[serde(default)]
    pub user: Option<ActivityActor>,
    /// Where the mention lives, e.g. `"comment"` or `"issue"`.
    #[serde(default)]
    pub source_type: String,
    /// The issue doing the referencing; `null` if it is gone.
    #[serde(default)]
    pub source_issue: Option<SourceIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SourceIssue {
    pub id: i64,
    pub identifier: String,
    #[serde(default)]
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DependencyEntry {
    /// `"dependency:<id>"`.
    pub cursor: String,
    pub created_at: String,
    #[serde(default)]
    pub actor: Option<ActivityActor>,
    pub event: DependencyEvent,
    pub kind: DependencyKind,
    /// Relation as seen from the viewed issue; word the entry from this, not
    /// from `description` (which is written from the blocking side).
    pub direction: DependencyDirection,
    /// `None` when the other issue no longer exists.
    #[serde(default)]
    pub other_issue: Option<OtherIssue>,
    /// Server-rendered HTML.
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyEvent {
    Create,
    Destroy,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    Blocks,
    Relates,
    Duplicates,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyDirection {
    Blocking,
    BlockedBy,
    Relates,
    Duplicates,
    DuplicatedBy,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OtherIssue {
    pub id: i64,
    pub identifier: String,
    #[serde(default)]
    pub title: String,
}

/// A changed field. `from`/`to` are `None` when the server sent `null`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FieldChange {
    pub field: String,
    #[serde(default)]
    pub from: Option<ChangeValue>,
    #[serde(default)]
    pub to: Option<ChangeValue>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum ChangeValue {
    /// Lane/assignee/project/parent/team: remote id plus a display attribute.
    Ref(ActivityRef),
    /// Priority name, title, estimate, due date, description, ...
    Scalar(serde_json::Value),
}

/// `{id, name}` (or `{id, identifier}` for `parent_issue_id`); the display
/// attribute is `null` when the record was deleted.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ActivityRef {
    pub id: i64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub identifier: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn path_defaults_and_encoding() {
        let p = ActivityParams::default();
        assert_eq!(
            list_path(1, 2, &p),
            "api/v1/teams/1/issues/2/activity?types=versions,dependencies,references"
        );
        let p = ActivityParams {
            before_id: Some("version:440"),
            since: Some("2026-10-31T17:59:21+00:00"),
            limit: Some(9999),
        };
        assert_eq!(
            list_path(1, 2, &p),
            "api/v1/teams/1/issues/2/activity?types=versions,dependencies,references\
             &before_id=version%3A440&since=2026-10-31T17%3A59%3A21%2B00%3A00&limit=500"
        );
    }

    #[test]
    fn page_meta_reads_string_cursor_and_server_time() {
        let mut h = HeaderMap::new();
        h.insert("X-Next-Before-Id", "version:440".parse().unwrap());
        h.insert(
            "X-Server-Time",
            "2026-10-31T17:59:21.515570Z".parse().unwrap(),
        );
        assert_eq!(
            page_meta(&h),
            (
                Some("version:440".to_string()),
                Some("2026-10-31T17:59:21.515570Z".to_string())
            )
        );
        assert_eq!(page_meta(&HeaderMap::new()), (None, None));
    }

    #[test]
    fn unknown_entry_type_is_skipped_not_fatal() {
        let value = json!([
            {"type":"comment","id":1,"cursor":"comment:1","created_at":"2026-07-06T00:00:00Z"},
            {"type":"reference","id":1,"cursor":"reference:1","created_at":"2026-07-08T13:35:39Z",
             "source_issue":null,"source_type":"comment","user":null}
        ]);
        let rows: Vec<ActivityEntry> =
            crate::api::client::decode_list_lenient(value, "activity").unwrap();
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0], ActivityEntry::Reference(_)));
    }

    fn version(body: &str) -> VersionEntry {
        match serde_json::from_str(body).unwrap() {
            ActivityEntry::Version(v) => v,
            other => panic!("expected version, got {other:?}"),
        }
    }

    fn reference(body: &str) -> ReferenceEntry {
        match serde_json::from_str(body).unwrap() {
            ActivityEntry::Reference(r) => r,
            other => panic!("expected reference, got {other:?}"),
        }
    }

    fn dependency(body: &str) -> DependencyEntry {
        match serde_json::from_str(body).unwrap() {
            ActivityEntry::Dependency(d) => d,
            other => panic!("expected dependency, got {other:?}"),
        }
    }

    #[test]
    fn decodes_dependency_entry() {
        let d = dependency(
            r#"{"type":"dependency","id":443,"cursor":"dependency:443",
            "created_at":"2026-07-06T20:07:55Z","event":"create","kind":"blocks",
            "direction":"blocked_by",
            "other_issue":{"id":2,"identifier":"MTK-2","title":"Add password reset"},
            "actor":{"id":2,"name":"ryan"},"description":"added <strong>MTK-2</strong>"}"#,
        );
        assert_eq!(d.cursor, "dependency:443");
        assert_eq!(d.event, DependencyEvent::Create);
        assert_eq!(d.kind, DependencyKind::Blocks);
        assert_eq!(d.direction, DependencyDirection::BlockedBy);
        assert_eq!(d.actor, Some(ActivityActor { id: 2, name: "ryan".into() }));
        assert_eq!(
            d.other_issue,
            Some(OtherIssue { id: 2, identifier: "MTK-2".into(), title: "Add password reset".into() })
        );
    }

    #[test]
    fn decodes_dependency_with_null_other_issue_and_actor() {
        let d = dependency(
            r#"{"type":"dependency","id":9,"cursor":"dependency:9","created_at":"t",
            "event":"destroy","kind":"duplicates","direction":"duplicated_by",
            "other_issue":null,"actor":null,"description":""}"#,
        );
        assert_eq!(d.event, DependencyEvent::Destroy);
        assert_eq!(d.kind, DependencyKind::Duplicates);
        assert_eq!(d.direction, DependencyDirection::DuplicatedBy);
        assert_eq!(d.other_issue, None);
        assert_eq!(d.actor, None);
    }

    #[test]
    fn unknown_dependency_enum_values_do_not_fail_decoding() {
        let d = dependency(
            r#"{"type":"dependency","id":1,"cursor":"dependency:1","created_at":"t",
            "event":"restore","kind":"x","direction":"y"}"#,
        );
        assert_eq!(d.event, DependencyEvent::Unknown);
        assert_eq!(d.kind, DependencyKind::Unknown);
        assert_eq!(d.direction, DependencyDirection::Unknown);
    }

    #[test]
    fn decodes_version_entry_with_scalar_change() {
        let body = r#"{"type":"version","id":438,"created_at":"2026-07-06T00:40:47Z",
            "actor":{"id":2,"name":"ryan"},"event":"update",
            "changes":[{"field":"priority","from":"high","to":"urgent"}],
            "description":"changed priority from <strong>High</strong> to <strong>Urgent</strong>",
            "cursor":"version:438"}"#;
        let v = version(body);
        assert_eq!(v.cursor, "version:438");
        assert_eq!(v.created_at, "2026-07-06T00:40:47Z");
        assert_eq!(v.actor, Some(ActivityActor { id: 2, name: "ryan".into() }));
        assert!(v.description.contains("<strong>"));
        assert_eq!(v.changes.len(), 1);
        assert_eq!(v.changes[0].field, "priority");
        assert_eq!(v.changes[0].from, Some(ChangeValue::Scalar(json!("high"))));
        assert_eq!(v.changes[0].to, Some(ChangeValue::Scalar(json!("urgent"))));
    }

    #[test]
    fn decodes_reference_values_and_nulls() {
        let body = r#"{"type":"version","id":812,"created_at":"2026-07-07T09:12:03Z",
            "actor":null,"event":"update",
            "changes":[
              {"field":"lane_id","from":{"id":3,"name":"Backlog"},"to":{"id":4,"name":"In Progress"}},
              {"field":"assignee_id","from":null,"to":{"id":2,"name":"ryan"}},
              {"field":"parent_issue_id","from":null,"to":{"id":9,"identifier":"MTK-9"}},
              {"field":"project_id","from":{"id":5,"name":null},"to":null},
              {"field":"estimate","from":3,"to":null}],
            "description":"x","cursor":"version:812"}"#;
        let v = version(body);
        assert_eq!(v.actor, None);
        let r = |id, name: Option<&str>, identifier: Option<&str>| {
            Some(ChangeValue::Ref(ActivityRef {
                id,
                name: name.map(Into::into),
                identifier: identifier.map(Into::into),
            }))
        };
        assert_eq!(v.changes[0].from, r(3, Some("Backlog"), None));
        assert_eq!(v.changes[0].to, r(4, Some("In Progress"), None));
        assert_eq!(v.changes[1].from, None);
        assert_eq!(v.changes[1].to, r(2, Some("ryan"), None));
        assert_eq!(v.changes[2].to, r(9, None, Some("MTK-9")));
        assert_eq!(v.changes[3].from, r(5, None, None));
        assert_eq!(v.changes[3].to, None);
        assert_eq!(v.changes[4].from, Some(ChangeValue::Scalar(json!(3))));
    }

    #[test]
    fn decodes_empty_changes() {
        let body = r#"{"type":"version","id":1,"created_at":"t","actor":null,
            "event":"update","changes":[],"description":"","cursor":"version:1"}"#;
        let v = version(body);
        assert!(v.changes.is_empty());
    }

    #[test]
    fn decodes_reference_entry() {
        let body = r#"{"type":"reference","id":1,"cursor":"reference:1",
            "created_at":"2026-07-08T13:35:39Z",
            "source_issue":{"id":2,"identifier":"MTK-2","title":"Add password reset functionality"},
            "source_type":"comment","user":{"id":2,"name":"ryan"}}"#;
        let r = reference(body);
        assert_eq!(r.cursor, "reference:1");
        assert_eq!(r.created_at, "2026-07-08T13:35:39Z");
        assert_eq!(r.user, Some(ActivityActor { id: 2, name: "ryan".into() }));
        assert_eq!(r.source_type, "comment");
        let src = r.source_issue.unwrap();
        assert_eq!(src.id, 2);
        assert_eq!(src.identifier, "MTK-2");
        assert_eq!(src.title, "Add password reset functionality");
    }

    #[test]
    fn decodes_reference_with_null_user() {
        let body = r#"{"type":"reference","id":7,"cursor":"reference:7",
            "created_at":"t","source_issue":{"id":3,"identifier":"MTK-3","title":"x"},
            "source_type":"issue","user":null}"#;
        let r = reference(body);
        assert_eq!(r.user, None);
    }
}
