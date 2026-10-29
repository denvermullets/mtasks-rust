//! CSV import / export for a team's issues (JAIT-214).
//!
//! The column set mirrors Rails' `IssueExporter::HEADERS` / `IssueImporter`
//! so a file exported by either side can be imported by the other. The parsing
//! and formatting functions are pure; only [`apply_import`] touches SQLite.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::{params, Connection};

use crate::db::models::{Issue, Label, Lane, Member, Priority, Project, Team};
use crate::db::{queries, sync_queue};

/// Rails' documented header set, in export order.
pub const HEADERS: [&str; 20] = [
    "ID",
    "Team",
    "Title",
    "Description",
    "Status",
    "Estimate",
    "Priority",
    "Project ID",
    "Project",
    "Creator",
    "Assignee",
    "Labels",
    "Created",
    "Updated",
    "Started",
    "Completed",
    "Canceled",
    "Archived",
    "Due Date",
    "Parent issue",
];

const TS_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.3fZ";

/// Colors for labels created during import (same swatches as the label modal).
const LABEL_PALETTE: [&str; 6] = [
    "#ef4444", "#f59e0b", "#10b981", "#3b82f6", "#8b5cf6", "#ec4899",
];

// ───────────────────────────── Export ─────────────────────────────

/// Everything the exporter needs, preloaded so the formatter stays pure.
pub struct ExportData<'a> {
    pub team: &'a Team,
    pub issues: &'a [Issue],
    pub lanes: &'a [Lane],
    pub projects: &'a [Project],
    pub members: &'a [Member],
    /// issue id -> label names
    pub labels: &'a HashMap<String, Vec<String>>,
}

/// Rails: `<identifier lowercased>-issues-<date>.csv`.
pub fn export_filename(team: &Team, date: NaiveDate) -> String {
    format!("{}-issues-{}.csv", team.identifier.to_lowercase(), date)
}

fn priority_label(p: i32) -> &'static str {
    match Priority::from_i32(p) {
        Priority::Urgent => "Urgent",
        Priority::High => "High",
        Priority::Medium => "Medium",
        Priority::Low => "Low",
        Priority::None => "No priority",
    }
}

fn identifier(team: &Team, issue: &Issue) -> String {
    match issue.team_number {
        Some(n) => format!("{}-{}", team.identifier, n),
        None => String::new(),
    }
}

fn format_estimate(e: f64) -> String {
    if e.fract() == 0.0 {
        format!("{}", e as i64)
    } else {
        format!("{e}")
    }
}

pub fn export_csv(data: &ExportData) -> Result<String, csv::Error> {
    let mut wtr = csv::Writer::from_writer(Vec::new());
    wtr.write_record(HEADERS)?;

    let mut issues: Vec<&Issue> = data.issues.iter().collect();
    issues.sort_by_key(|i| i.team_number.unwrap_or(i32::MAX));

    let lane_name = |id: &str| {
        data.lanes
            .iter()
            .find(|l| l.id == id)
            .map(|l| l.name.clone())
            .unwrap_or_default()
    };
    let member_name = |id: &Option<String>| {
        id.as_deref()
            .and_then(|id| data.members.iter().find(|m| m.id == id))
            .map(|m| m.name.clone())
            .unwrap_or_default()
    };
    let opt = |v: &Option<String>| v.clone().unwrap_or_default();

    for issue in issues {
        let project = issue
            .project_id
            .as_deref()
            .and_then(|id| data.projects.iter().find(|p| p.id == id));
        let parent = issue
            .parent_issue_id
            .as_deref()
            .and_then(|id| data.issues.iter().find(|i| i.id == id))
            .map(|p| identifier(data.team, p))
            .unwrap_or_default();
        let labels = data
            .labels
            .get(&issue.id)
            .map(|l| l.join(", "))
            .unwrap_or_default();

        wtr.write_record([
            identifier(data.team, issue),
            data.team.name.clone(),
            issue.title.clone(),
            opt(&issue.description),
            lane_name(&issue.lane_id),
            issue.estimate.map(format_estimate).unwrap_or_default(),
            priority_label(issue.priority).to_string(),
            project
                .and_then(|p| p.remote_id)
                .map(|id| id.to_string())
                .unwrap_or_default(),
            project.map(|p| p.name.clone()).unwrap_or_default(),
            member_name(&issue.creator_id),
            member_name(&issue.assignee_id),
            labels,
            issue.created_at.clone(),
            issue.updated_at.clone(),
            opt(&issue.started_at),
            opt(&issue.completed_at),
            opt(&issue.canceled_at),
            opt(&issue.archived_at),
            opt(&issue.due_date),
            parent,
        ])?;
    }

    let bytes = wtr.into_inner().map_err(|e| e.into_error())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

// ───────────────────────────── Import ─────────────────────────────

/// Existing team data that imported names are resolved against.
pub struct ImportContext<'a> {
    pub lanes: &'a [Lane],
    pub projects: &'a [Project],
    pub members: &'a [Member],
}

/// One validated CSV row, ready to be inserted.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportRow {
    /// 1-based line number in the file (header is line 1).
    pub line: u64,
    /// The `ID` column (e.g. `PROJ-3`); used to resolve `Parent issue`.
    pub source_id: String,
    pub title: String,
    pub description: Option<String>,
    pub lane_id: String,
    pub estimate: Option<f64>,
    pub priority: i32,
    pub project_id: Option<String>,
    pub assignee_id: Option<String>,
    pub label_names: Vec<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub canceled_at: Option<String>,
    pub archived_at: Option<String>,
    pub due_date: Option<String>,
    pub parent_source_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RowProblem {
    pub line: u64,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct ImportPreview {
    pub rows: Vec<ImportRow>,
    /// Rows that will be skipped.
    pub errors: Vec<RowProblem>,
    /// Rows that import, but with something dropped / defaulted.
    pub warnings: Vec<RowProblem>,
}

/// Parse a timestamp cell into the local timestamp format. `Ok(None)` if empty.
fn parse_ts(raw: &str) -> Result<Option<String>, ()> {
    let s = raw.trim();
    if s.is_empty() {
        return Ok(None);
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(Some(dt.with_timezone(&Utc).format(TS_FORMAT).to_string()));
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let midnight = d.and_hms_opt(0, 0, 0).ok_or(())?.and_utc();
        return Ok(Some(midnight.format(TS_FORMAT).to_string()));
    }
    Err(())
}

fn parse_due(raw: &str) -> Result<Option<String>, ()> {
    let s = raw.trim();
    if s.is_empty() {
        return Ok(None);
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(Some(d.to_string()));
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(Some(dt.date_naive().to_string()));
    }
    Err(())
}

fn parse_priority(raw: &str) -> Result<i32, ()> {
    match raw.trim().to_lowercase().as_str() {
        "" | "no priority" | "no_priority" | "none" => Ok(Priority::None.to_i32()),
        "urgent" => Ok(Priority::Urgent.to_i32()),
        "high" => Ok(Priority::High.to_i32()),
        "medium" => Ok(Priority::Medium.to_i32()),
        "low" => Ok(Priority::Low.to_i32()),
        _ => Err(()),
    }
}

/// Parse and validate `text`. `Err` is a fatal, whole-file problem (no usable
/// header); per-row problems land in [`ImportPreview::errors`] and never abort.
///
/// `Title` is the only required header; any other missing documented header
/// yields a single warning (line 1) and those cells are treated as empty.
pub fn parse_import(text: &str, ctx: &ImportContext) -> Result<ImportPreview, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rdr = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(text.as_bytes());

    let header_record = rdr
        .headers()
        .map_err(|e| format!("Could not read CSV header: {e}"))?
        .clone();
    if header_record.iter().all(|h| h.trim().is_empty()) {
        return Err("The file is empty.".into());
    }
    let index: HashMap<String, usize> = header_record
        .iter()
        .enumerate()
        .map(|(i, h)| (h.trim().to_lowercase(), i))
        .collect();
    if !index.contains_key("title") {
        return Err(format!(
            "Missing required \"Title\" column. Expected headers: {}",
            HEADERS.join(", ")
        ));
    }

    let mut preview = ImportPreview::default();
    let missing: Vec<&str> = HEADERS
        .iter()
        .filter(|h| !index.contains_key(&h.to_lowercase()))
        .copied()
        .collect();
    if !missing.is_empty() {
        preview.warnings.push(RowProblem {
            line: 1,
            message: format!(
                "Missing optional columns (treated as empty): {}",
                missing.join(", ")
            ),
        });
    }

    let mut lanes: Vec<&Lane> = ctx.lanes.iter().collect();
    lanes.sort_by_key(|l| l.position);

    for result in rdr.records() {
        let record = match result {
            Ok(r) => r,
            Err(e) => {
                let line = e.position().map(|p| p.line()).unwrap_or(0);
                preview.errors.push(RowProblem {
                    line,
                    message: format!("Malformed CSV row: {e}"),
                });
                continue;
            }
        };
        if record.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        let line = record.position().map(|p| p.line()).unwrap_or(0);
        let cell = |name: &str| -> &str {
            index
                .get(name)
                .and_then(|&i| record.get(i))
                .unwrap_or("")
                .trim()
        };
        let mut errs: Vec<String> = Vec::new();
        let mut warns: Vec<String> = Vec::new();

        let title = cell("title").to_string();
        if title.is_empty() {
            errs.push("Title can't be blank".into());
        }

        let status = cell("status");
        let lane_id = if status.is_empty() {
            lanes.first().map(|l| l.id.clone())
        } else {
            match lanes.iter().find(|l| l.name.eq_ignore_ascii_case(status)) {
                Some(l) => Some(l.id.clone()),
                None => {
                    warns.push(format!("Unknown status \"{status}\"; using first lane"));
                    lanes.first().map(|l| l.id.clone())
                }
            }
        };
        if lane_id.is_none() {
            errs.push("Team has no lanes".into());
        }

        let estimate = match cell("estimate") {
            "" => None,
            s => match s.parse::<f64>() {
                Ok(v) if v.is_finite() && v >= 0.0 => Some(v),
                _ => {
                    errs.push(format!("Invalid estimate \"{s}\""));
                    None
                }
            },
        };

        let priority = parse_priority(cell("priority")).unwrap_or_else(|_| {
            errs.push(format!("Invalid priority \"{}\"", cell("priority")));
            Priority::None.to_i32()
        });

        let mut ts = |col: &str, label: &str| -> Option<String> {
            parse_ts(cell(col)).unwrap_or_else(|_| {
                errs.push(format!("Invalid {label} date \"{}\"", cell(col)));
                None
            })
        };
        let created_at = ts("created", "Created");
        let updated_at = ts("updated", "Updated");
        let started_at = ts("started", "Started");
        let completed_at = ts("completed", "Completed");
        let canceled_at = ts("canceled", "Canceled");
        let archived_at = ts("archived", "Archived");
        let due_date = parse_due(cell("due date")).unwrap_or_else(|_| {
            errs.push(format!("Invalid due date \"{}\"", cell("due date")));
            None
        });

        let project_name = cell("project");
        let project_id = if project_name.is_empty() {
            None
        } else {
            let found = ctx
                .projects
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case(project_name));
            if found.is_none() {
                warns.push(format!(
                    "Unknown project \"{project_name}\"; left unassigned"
                ));
            }
            found.map(|p| p.id.clone())
        };

        let assignee = cell("assignee");
        let assignee_id = if assignee.is_empty() {
            None
        } else {
            let found = ctx
                .members
                .iter()
                .find(|m| m.name.eq_ignore_ascii_case(assignee));
            if found.is_none() {
                warns.push(format!("Unknown assignee \"{assignee}\"; left unassigned"));
            }
            found.map(|m| m.id.clone())
        };

        let mut label_names: Vec<String> = Vec::new();
        for name in cell("labels")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if !label_names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                label_names.push(name.to_string());
            }
        }

        if !errs.is_empty() {
            preview.errors.push(RowProblem {
                line,
                message: errs.join("; "),
            });
            continue;
        }
        if !warns.is_empty() {
            preview.warnings.push(RowProblem {
                line,
                message: warns.join("; "),
            });
        }

        let desc = cell("description");
        let parent = cell("parent issue");
        preview.rows.push(ImportRow {
            line,
            source_id: cell("id").to_string(),
            title,
            description: (!desc.is_empty()).then(|| desc.to_string()),
            lane_id: lane_id.unwrap_or_default(),
            estimate,
            priority,
            project_id,
            assignee_id,
            label_names,
            created_at,
            updated_at,
            started_at,
            completed_at,
            canceled_at,
            archived_at,
            due_date,
            parent_source_id: (!parent.is_empty()).then(|| parent.to_string()),
        });
    }

    Ok(preview)
}

#[derive(Debug, Default, PartialEq)]
pub struct ImportOutcome {
    pub imported: usize,
    pub labels_created: usize,
}

/// Insert `rows` as new issues in `team_id` and enqueue creates for sync, all in
/// one transaction. Missing labels are created (and enqueued first, so issues can
/// reference them); parents are resolved by `ID` among the imported rows and
/// enqueued before their children.
pub fn apply_import(
    conn: &Connection,
    team_id: &str,
    creator_id: Option<&str>,
    rows: &[ImportRow],
) -> rusqlite::Result<ImportOutcome> {
    let now = Utc::now().format(TS_FORMAT).to_string();
    let mut outcome = ImportOutcome::default();

    let mut label_ids: HashMap<String, String> = queries::labels_for_team(conn, team_id)?
        .into_iter()
        .map(|l| (l.name.to_lowercase(), l.id))
        .collect();
    let first_number = queries::next_team_number(conn, team_id)?;

    let tx = conn.unchecked_transaction()?;
    let mut issue_ids: Vec<String> = Vec::new();
    let mut by_source: HashMap<&str, String> = HashMap::new();

    for (n, row) in rows.iter().enumerate() {
        let id = uuid::Uuid::new_v4().to_string();
        let issue = Issue {
            id: id.clone(),
            title: row.title.clone(),
            description: row.description.clone(),
            priority: row.priority,
            team_number: Some(first_number + n as i32),
            due_date: row.due_date.clone(),
            estimate: row.estimate,
            started_at: row.started_at.clone(),
            completed_at: row.completed_at.clone(),
            canceled_at: row.canceled_at.clone(),
            lane_id: row.lane_id.clone(),
            team_id: team_id.to_string(),
            project_id: row.project_id.clone(),
            parent_issue_id: None,
            assignee_id: row.assignee_id.clone(),
            creator_id: creator_id.map(str::to_string),
            archived_at: row.archived_at.clone(),
            remote_id: None,
            created_at: row.created_at.clone().unwrap_or_else(|| now.clone()),
            updated_at: row.updated_at.clone().unwrap_or_else(|| now.clone()),
        };
        queries::insert_issue(&tx, &issue)?;
        // insert_issue doesn't write these columns.
        tx.execute(
            "UPDATE issues SET project_id = ?1, archived_at = ?2 WHERE id = ?3",
            params![issue.project_id, issue.archived_at, issue.id],
        )?;

        let mut ids_for_issue = Vec::new();
        for name in &row.label_names {
            let key = name.to_lowercase();
            let lid = match label_ids.get(&key) {
                Some(id) => id.clone(),
                None => {
                    let label = Label {
                        id: uuid::Uuid::new_v4().to_string(),
                        remote_id: None,
                        team_id: team_id.to_string(),
                        name: name.clone(),
                        color: Some(LABEL_PALETTE[label_ids.len() % LABEL_PALETTE.len()].into()),
                        created_at: now.clone(),
                        updated_at: now.clone(),
                    };
                    queries::insert_label(&tx, &label)?;
                    sync_queue::enqueue(&tx, "label", &label.id, "create", None)?;
                    outcome.labels_created += 1;
                    label_ids.insert(key, label.id.clone());
                    label.id
                }
            };
            ids_for_issue.push(lid);
        }
        // (set_issue_labels opens its own transaction, so insert directly.)
        for lid in &ids_for_issue {
            tx.execute(
                "INSERT OR IGNORE INTO issue_labels (issue_id, label_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?3)",
                params![id, lid, now],
            )?;
        }

        if !row.source_id.is_empty() {
            by_source.insert(row.source_id.as_str(), id.clone());
        }
        issue_ids.push(id);
        outcome.imported += 1;
    }

    // Second pass: parents. A self-reference is ignored.
    let mut has_parent = vec![false; rows.len()];
    for (i, row) in rows.iter().enumerate() {
        if let Some(pid) = row
            .parent_source_id
            .as_deref()
            .and_then(|s| by_source.get(s))
            .filter(|pid| **pid != issue_ids[i])
        {
            tx.execute(
                "UPDATE issues SET parent_issue_id = ?1 WHERE id = ?2",
                params![pid, issue_ids[i]],
            )?;
            has_parent[i] = true;
        }
    }

    // Enqueue parents before children so the child's parent has a remote id.
    for pass_children in [false, true] {
        for (i, id) in issue_ids.iter().enumerate() {
            if has_parent[i] == pass_children {
                sync_queue::enqueue(&tx, "issue", id, "create", None)?;
            }
        }
    }
    tx.commit()?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;

    const NOW: &str = "2026-01-01T00:00:00.000Z";

    type Sample = (
        Team,
        Vec<Lane>,
        Vec<Member>,
        Vec<Issue>,
        HashMap<String, Vec<String>>,
    );

    fn team() -> Team {
        Team {
            id: "t1".into(),
            name: "My Team".into(),
            identifier: "PROJ".into(),
            remote_id: None,
            archived_at: None,
            created_at: NOW.into(),
            updated_at: NOW.into(),
        }
    }

    fn lane(id: &str, name: &str, pos: i32) -> Lane {
        Lane {
            id: id.into(),
            name: name.into(),
            color: None,
            position: pos,
            team_id: "t1".into(),
            remote_id: None,
            created_at: NOW.into(),
            updated_at: NOW.into(),
        }
    }

    fn member(id: &str, name: &str) -> Member {
        Member {
            id: id.into(),
            remote_id: None,
            team_id: "t1".into(),
            name: name.into(),
            email: format!("{id}@x.io"),
            created_at: NOW.into(),
            updated_at: NOW.into(),
        }
    }

    fn issue(id: &str, n: i32, title: &str) -> Issue {
        Issue {
            id: id.into(),
            title: title.into(),
            description: None,
            priority: 4,
            team_number: Some(n),
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
            created_at: NOW.into(),
            updated_at: NOW.into(),
        }
    }

    fn sample() -> Sample {
        let mut i2 = issue("b", 2, "Second, with \"quotes\"\nand newline");
        i2.priority = 0;
        i2.estimate = Some(3.0);
        i2.due_date = Some("2026-02-03".into());
        i2.lane_id = "l2".into();
        i2.assignee_id = Some("m1".into());
        i2.parent_issue_id = Some("a".into());
        i2.description = Some("desc".into());
        let issues = vec![i2, issue("a", 1, "First")];
        let mut labels = HashMap::new();
        labels.insert("b".to_string(), vec!["bug".to_string(), "ui".to_string()]);
        (
            team(),
            vec![lane("l1", "Backlog", 0), lane("l2", "In Progress", 1)],
            vec![member("m1", "Ann Lee")],
            issues,
            labels,
        )
    }

    fn export(s: &Sample) -> String {
        export_csv(&ExportData {
            team: &s.0,
            issues: &s.3,
            lanes: &s.1,
            projects: &[],
            members: &s.2,
            labels: &s.4,
        })
        .unwrap()
    }

    #[test]
    fn export_has_rails_headers_and_ordering() {
        let s = sample();
        let out = export(&s);
        let mut rdr = csv::Reader::from_reader(out.as_bytes());
        assert_eq!(rdr.headers().unwrap().iter().collect::<Vec<_>>(), HEADERS);
        let rows: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(&rows[0][0], "PROJ-1");
        assert_eq!(&rows[1][0], "PROJ-2");
        assert_eq!(&rows[1][1], "My Team");
        assert_eq!(&rows[1][2], "Second, with \"quotes\"\nand newline");
        assert_eq!(&rows[1][4], "In Progress");
        assert_eq!(&rows[1][5], "3");
        assert_eq!(&rows[1][6], "Urgent");
        assert_eq!(&rows[1][10], "Ann Lee");
        assert_eq!(&rows[1][11], "bug, ui");
        assert_eq!(&rows[1][18], "2026-02-03");
        assert_eq!(&rows[1][19], "PROJ-1");
        assert_eq!(&rows[0][6], "No priority");
    }

    #[test]
    fn filename_matches_rails() {
        let d = NaiveDate::from_ymd_opt(2026, 3, 4).unwrap();
        assert_eq!(export_filename(&team(), d), "proj-issues-2026-03-04.csv");
    }

    #[test]
    fn import_reports_bad_rows_and_keeps_good_ones() {
        let lanes = vec![lane("l1", "Backlog", 0), lane("l2", "Done", 1)];
        let ctx = ImportContext {
            lanes: &lanes,
            projects: &[],
            members: &[],
        };
        let csv = "ID,Title,Status,Priority,Estimate,Due Date,Assignee\n\
                   X-1,Good,Done,High,2,2026-05-01,\n\
                   X-2,,Backlog,,,,\n\
                   X-3,Bad prio,Backlog,Huge,,,\n\
                   X-4,Bad est,Backlog,,abc,,\n\
                   X-5,Bad date,Backlog,,,31/12/2026,\n\
                   X-6,Odd status,Nowhere,,,,Ghost\n\
                   ,,,,,,\n";
        let p = parse_import(csv, &ctx).unwrap();
        assert_eq!(p.rows.len(), 2);
        assert_eq!(p.rows[0].lane_id, "l2");
        assert_eq!(p.rows[0].priority, 1);
        assert_eq!(p.rows[0].estimate, Some(2.0));
        assert_eq!(p.rows[0].due_date.as_deref(), Some("2026-05-01"));
        assert_eq!(p.rows[1].lane_id, "l1");
        assert_eq!(p.errors.len(), 4);
        assert!(p.errors[0].message.contains("Title"));
        assert!(p.errors[1].message.contains("priority"));
        assert!(p.errors[2].message.contains("estimate"));
        assert!(p.errors[3].message.contains("due date"));
        // lines are file lines (header = 1)
        assert_eq!(p.errors[0].line, 3);
        assert!(p.warnings.iter().any(|w| w.line == 1));
        assert!(p
            .warnings
            .iter()
            .any(|w| w.message.contains("Unknown status")));
        assert!(p
            .warnings
            .iter()
            .any(|w| w.message.contains("Unknown assignee")));
    }

    #[test]
    fn import_handles_ragged_rows() {
        let lanes = vec![lane("l1", "Backlog", 0)];
        let ctx = ImportContext {
            lanes: &lanes,
            projects: &[],
            members: &[],
        };
        let csv = "Title,Status,Labels\nShort\nLong,Backlog,a,extra,cells\n";
        let p = parse_import(csv, &ctx).unwrap();
        assert_eq!(p.rows.len(), 2);
        assert_eq!(p.rows[1].label_names, vec!["a"]);
    }

    #[test]
    fn import_rejects_missing_title_header_and_empty_file() {
        let ctx = ImportContext {
            lanes: &[],
            projects: &[],
            members: &[],
        };
        assert!(parse_import("Name,Status\nx,y\n", &ctx)
            .unwrap_err()
            .contains("Title"));
        assert!(parse_import("", &ctx).is_err());
    }

    #[test]
    fn import_strips_bom_and_dedups_labels() {
        let lanes = vec![lane("l1", "Backlog", 0)];
        let ctx = ImportContext {
            lanes: &lanes,
            projects: &[],
            members: &[],
        };
        let p = parse_import("\u{feff}Title,Labels\nA,\"x, X ,y\"\n", &ctx).unwrap();
        assert_eq!(p.rows[0].label_names, vec!["x", "y"]);
    }

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        migrations::run(&conn).unwrap();
        conn.execute(
            "INSERT INTO teams (id, name, identifier, created_at, updated_at)
             VALUES ('t1','My Team','PROJ',?1,?1)",
            params![NOW],
        )
        .unwrap();
        for (id, name, pos) in [("l1", "Backlog", 0), ("l2", "In Progress", 1)] {
            conn.execute(
                "INSERT INTO lanes (id, name, position, team_id, created_at, updated_at)
                 VALUES (?1,?2,?3,'t1',?4,?4)",
                params![id, name, pos, NOW],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn round_trip_export_then_import() {
        let s = sample();
        let out = export(&s);

        let conn = db();
        let ctx = ImportContext {
            lanes: &s.1,
            projects: &[],
            members: &s.2,
        };
        let preview = parse_import(&out, &ctx).unwrap();
        assert!(preview.errors.is_empty(), "{:?}", preview.errors);
        assert!(preview.warnings.is_empty(), "{:?}", preview.warnings);
        assert_eq!(preview.rows.len(), 2);

        // The members table is empty in this DB, so drop the assignee (FK).
        let mut rows = preview.rows.clone();
        assert_eq!(rows[1].assignee_id.as_deref(), Some("m1"));
        for r in &mut rows {
            r.assignee_id = None;
        }
        let outcome = apply_import(&conn, "t1", None, &rows).unwrap();
        assert_eq!(
            outcome,
            ImportOutcome {
                imported: 2,
                labels_created: 2
            }
        );

        let issues = queries::issues_for_team(&conn, "t1").unwrap();
        assert_eq!(issues.len(), 2);
        let second = issues
            .iter()
            .find(|i| i.title.starts_with("Second"))
            .unwrap();
        assert_eq!(second.title, "Second, with \"quotes\"\nand newline");
        assert_eq!(second.priority, 0);
        assert_eq!(second.estimate, Some(3.0));
        assert_eq!(second.lane_id, "l2");
        assert_eq!(second.team_number, Some(2));
        assert_eq!(second.due_date.as_deref(), Some("2026-02-03"));
        let first = issues.iter().find(|i| i.title == "First").unwrap();
        assert_eq!(second.parent_issue_id.as_deref(), Some(first.id.as_str()));
        assert_eq!(
            queries::label_ids_for_issue(&conn, &second.id)
                .unwrap()
                .len(),
            2
        );

        // Sync queue: 2 labels, then parent issue before child.
        let pending = sync_queue::pending_entries(&conn).unwrap();
        assert_eq!(pending.len(), 4);
        assert_eq!(pending[0].entity_type, "label");
        assert_eq!(pending[1].entity_type, "label");
        assert_eq!(pending[2].entity_id, first.id);
        assert_eq!(pending[3].entity_id, second.id);

        // Re-export from the DB round-trips the content.
        let mut label_names: HashMap<String, Vec<String>> = HashMap::new();
        for (iid, l) in queries::issue_labels_for_team(&conn, "t1").unwrap() {
            label_names.entry(iid).or_default().push(l.name);
        }
        let again = export_csv(&ExportData {
            team: &s.0,
            issues: &issues,
            lanes: &s.1,
            projects: &[],
            members: &[],
            labels: &label_names,
        })
        .unwrap();
        assert!(again.contains("PROJ-2"));
        assert!(again.contains("bug, ui"));
        assert!(again.contains("PROJ-1"));
    }
}
