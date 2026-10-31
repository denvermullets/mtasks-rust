//! DTOs for the issue Activity feed (`GET .../issues/:id/activity`).

// Unused until the client call and store land (JRST-130 / JRST-131); drop then.
#![allow(dead_code)]

use serde::Deserialize;

/// One feed entry, tagged by the JSON `type`. Dependency and reference
/// variants are added alongside `Version` (JRST-125 / JRST-126).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActivityEntry {
    Version(VersionEntry),
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
    fn decodes_version_entry_with_scalar_change() {
        let body = r#"{"type":"version","id":438,"created_at":"2026-07-06T00:40:47Z",
            "actor":{"id":2,"name":"ryan"},"event":"update",
            "changes":[{"field":"priority","from":"high","to":"urgent"}],
            "description":"changed priority from <strong>High</strong> to <strong>Urgent</strong>",
            "cursor":"version:438"}"#;
        let ActivityEntry::Version(v) = serde_json::from_str(body).unwrap();
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
        let ActivityEntry::Version(v) = serde_json::from_str(body).unwrap();
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
        let ActivityEntry::Version(v) = serde_json::from_str(body).unwrap();
        assert!(v.changes.is_empty());
    }
}
