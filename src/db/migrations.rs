use rusqlite::{Connection, Result};
use std::collections::HashSet;

struct Migration {
    version: u32,
    name: &'static str,
    up: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial_baseline",
        up: "
            CREATE TABLE IF NOT EXISTS teams (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                identifier TEXT NOT NULL,
                remote_id INTEGER,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS lanes (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                color TEXT,
                position INTEGER NOT NULL,
                team_id TEXT NOT NULL REFERENCES teams(id),
                remote_id INTEGER,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS issues (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                description TEXT,
                priority INTEGER NOT NULL DEFAULT 4,
                team_number INTEGER,
                due_date TEXT,
                estimate REAL,
                started_at TEXT,
                completed_at TEXT,
                canceled_at TEXT,
                lane_id TEXT NOT NULL REFERENCES lanes(id),
                team_id TEXT NOT NULL REFERENCES teams(id),
                remote_id INTEGER,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sync_queue (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                entity_type TEXT NOT NULL,
                entity_id TEXT NOT NULL,
                action TEXT NOT NULL,
                payload TEXT,
                created_at TEXT NOT NULL,
                synced_at TEXT
            );
        ",
    },
    Migration {
        version: 2,
        name: "expand_teams",
        up: "
            ALTER TABLE teams ADD COLUMN description TEXT;
            ALTER TABLE teams ADD COLUMN archived_at TEXT;
            ALTER TABLE teams ADD COLUMN issue_counter INTEGER NOT NULL DEFAULT 0;
        ",
    },
    Migration {
        version: 3,
        name: "create_projects",
        up: "
            CREATE TABLE IF NOT EXISTS projects (
                id TEXT PRIMARY KEY,
                remote_id INTEGER UNIQUE,
                team_id TEXT NOT NULL REFERENCES teams(id),
                name TEXT NOT NULL,
                description TEXT,
                status TEXT NOT NULL DEFAULT 'backlog',
                priority INTEGER NOT NULL DEFAULT 4,
                lead_id TEXT REFERENCES members(id),
                start_date TEXT,
                due_date TEXT,
                roadmap_commitment TEXT,
                velocity_score INTEGER NOT NULL DEFAULT 0,
                completed_issues_count INTEGER NOT NULL DEFAULT 0,
                total_issues_count INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_projects_team_id ON projects(team_id);
            CREATE INDEX IF NOT EXISTS idx_projects_lead_id ON projects(lead_id);
            CREATE INDEX IF NOT EXISTS idx_projects_team_id_roadmap_commitment
                ON projects(team_id, roadmap_commitment);
        ",
    },
    Migration {
        version: 4,
        name: "create_labels",
        up: "
            CREATE TABLE IF NOT EXISTS labels (
                id TEXT PRIMARY KEY,
                remote_id INTEGER UNIQUE,
                team_id TEXT NOT NULL REFERENCES teams(id),
                name TEXT NOT NULL,
                color TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(team_id, name)
            );
            CREATE INDEX IF NOT EXISTS idx_labels_team_id ON labels(team_id);

            CREATE TABLE IF NOT EXISTS issue_labels (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
                label_id TEXT NOT NULL REFERENCES labels(id) ON DELETE CASCADE,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(issue_id, label_id)
            );
            CREATE INDEX IF NOT EXISTS idx_issue_labels_issue_id ON issue_labels(issue_id);
            CREATE INDEX IF NOT EXISTS idx_issue_labels_label_id ON issue_labels(label_id);

            CREATE TABLE IF NOT EXISTS project_labels (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                label_id TEXT NOT NULL REFERENCES labels(id) ON DELETE CASCADE,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(project_id, label_id)
            );
            CREATE INDEX IF NOT EXISTS idx_project_labels_project_id ON project_labels(project_id);
            CREATE INDEX IF NOT EXISTS idx_project_labels_label_id ON project_labels(label_id);
        ",
    },
    Migration {
        version: 5,
        name: "create_members",
        up: "
            CREATE TABLE IF NOT EXISTS members (
                id TEXT PRIMARY KEY,
                remote_id INTEGER UNIQUE,
                team_id TEXT NOT NULL REFERENCES teams(id),
                name TEXT NOT NULL,
                email TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(team_id, email)
            );
            CREATE INDEX IF NOT EXISTS idx_members_team_id ON members(team_id);
        ",
    },
    Migration {
        version: 6,
        name: "expand_issues",
        up: "
            ALTER TABLE issues ADD COLUMN project_id TEXT REFERENCES projects(id);
            ALTER TABLE issues ADD COLUMN parent_issue_id TEXT REFERENCES issues(id);
            ALTER TABLE issues ADD COLUMN assignee_id TEXT REFERENCES members(id);
            ALTER TABLE issues ADD COLUMN creator_id TEXT REFERENCES members(id);
            ALTER TABLE issues ADD COLUMN archived_at TEXT;

            CREATE INDEX IF NOT EXISTS idx_issues_project_id ON issues(project_id);
            CREATE INDEX IF NOT EXISTS idx_issues_parent_issue_id ON issues(parent_issue_id);
            CREATE INDEX IF NOT EXISTS idx_issues_assignee_id ON issues(assignee_id);
            CREATE INDEX IF NOT EXISTS idx_issues_creator_id ON issues(creator_id);
        ",
    },
    Migration {
        version: 7,
        name: "create_issue_dependencies",
        up: "
            CREATE TABLE IF NOT EXISTS issue_dependencies (
                id TEXT PRIMARY KEY,
                remote_id INTEGER UNIQUE,
                blocking_issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
                blocked_issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(blocking_issue_id, blocked_issue_id)
            );
            CREATE INDEX IF NOT EXISTS idx_issue_dependencies_blocking
                ON issue_dependencies(blocking_issue_id);
            CREATE INDEX IF NOT EXISTS idx_issue_dependencies_blocked
                ON issue_dependencies(blocked_issue_id);
        ",
    },
    Migration {
        version: 8,
        name: "create_comments",
        up: "
            CREATE TABLE IF NOT EXISTS comments (
                id TEXT PRIMARY KEY,
                remote_id INTEGER UNIQUE,
                issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
                author_id TEXT REFERENCES members(id),
                body TEXT NOT NULL,
                parent_id TEXT REFERENCES comments(id),
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_comments_issue_id ON comments(issue_id);
        ",
    },
    Migration {
        version: 9,
        name: "project_progress_health",
        up: "
            ALTER TABLE projects ADD COLUMN progress_percentage INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE projects ADD COLUMN behind_schedule INTEGER NOT NULL DEFAULT 0;
        ",
    },
    Migration {
        version: 10,
        name: "create_issue_events",
        up: "
            CREATE TABLE IF NOT EXISTS issue_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
                actor_id TEXT REFERENCES members(id) ON DELETE SET NULL,
                source TEXT NOT NULL DEFAULT 'local',
                kind TEXT NOT NULL,
                old_value TEXT,
                new_value TEXT,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_issue_events_issue_id
                ON issue_events(issue_id, created_at);
            -- Backfill a 'created' entry for issues that already exist.
            INSERT INTO issue_events (issue_id, actor_id, source, kind, created_at)
                SELECT id, creator_id,
                       CASE WHEN remote_id IS NOT NULL THEN 'sync' ELSE 'local' END,
                       'created', created_at
                FROM issues;
        ",
    },
    Migration {
        version: 11,
        name: "issue_events_refs",
        up: "
            -- Ids behind old_value/new_value so names re-resolve at render time.
            ALTER TABLE issue_events ADD COLUMN old_ref TEXT;
            ALTER TABLE issue_events ADD COLUMN new_ref TEXT;
        ",
    },
    Migration {
        version: 12,
        name: "lane_category",
        up: "
            -- Rails workflow category: backlog, unstarted, started, completed, canceled.
            ALTER TABLE lanes ADD COLUMN category TEXT;
        ",
    },
    Migration {
        version: 13,
        name: "issues_deleted_at",
        up: "
            -- Local tombstone: a deleted issue keeps its row (hidden everywhere)
            -- so a later pull can't resurrect it from the server.
            ALTER TABLE issues ADD COLUMN deleted_at TEXT;
        ",
    },
    Migration {
        version: 14,
        name: "list_etags",
        up: "
            -- Last ETag seen per (team, list) so unchanged lists return 304.
            CREATE TABLE IF NOT EXISTS list_etags (
                team_id TEXT NOT NULL,
                list TEXT NOT NULL,
                etag TEXT NOT NULL,
                PRIMARY KEY (team_id, list)
            );
        ",
    },
    Migration {
        version: 15,
        name: "projects_status_nullable",
        // SQLite cannot drop NOT NULL in place, so rebuild the table. Runs with
        // foreign keys off (see FK_OFF_MIGRATIONS) so the DROP doesn't cascade
        // into project_labels.
        up: "
            CREATE TABLE projects_new (
                id TEXT PRIMARY KEY,
                remote_id INTEGER UNIQUE,
                team_id TEXT NOT NULL REFERENCES teams(id),
                name TEXT NOT NULL,
                description TEXT,
                status TEXT,
                priority INTEGER NOT NULL DEFAULT 4,
                lead_id TEXT REFERENCES members(id),
                start_date TEXT,
                due_date TEXT,
                roadmap_commitment TEXT,
                velocity_score INTEGER NOT NULL DEFAULT 0,
                completed_issues_count INTEGER NOT NULL DEFAULT 0,
                total_issues_count INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                progress_percentage INTEGER NOT NULL DEFAULT 0,
                behind_schedule INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO projects_new
                (id, remote_id, team_id, name, description, status, priority, lead_id,
                 start_date, due_date, roadmap_commitment, velocity_score,
                 completed_issues_count, total_issues_count, created_at, updated_at,
                 progress_percentage, behind_schedule)
                SELECT id, remote_id, team_id, name, description, status, priority, lead_id,
                       start_date, due_date, roadmap_commitment, velocity_score,
                       completed_issues_count, total_issues_count, created_at, updated_at,
                       progress_percentage, behind_schedule
                FROM projects;
            DROP TABLE projects;
            ALTER TABLE projects_new RENAME TO projects;
            CREATE INDEX IF NOT EXISTS idx_projects_team_id ON projects(team_id);
            CREATE INDEX IF NOT EXISTS idx_projects_lead_id ON projects(lead_id);
            CREATE INDEX IF NOT EXISTS idx_projects_team_id_roadmap_commitment
                ON projects(team_id, roadmap_commitment);
        ",
    },
    Migration {
        version: 16,
        name: "issue_dependencies_kind_direction",
        up: "
            -- kind mirrors Rails (blocks | relates | duplicates). direction is
            -- stored relative to blocking_issue_id (the link's source side):
            -- blocking | relates | duplicates.
            ALTER TABLE issue_dependencies ADD COLUMN kind TEXT NOT NULL DEFAULT 'blocks';
            ALTER TABLE issue_dependencies ADD COLUMN direction TEXT NOT NULL DEFAULT 'blocking';
        ",
    },
    Migration {
        version: 17,
        name: "team_access_lost",
        up: "
            -- Set when Rails answered a push with a team-access 404 (team archived
            -- or dropped from the token). Cleared when a pull sees the team again.
            ALTER TABLE teams ADD COLUMN access_lost_at TEXT;
        ",
    },
    Migration {
        version: 18,
        name: "sync_queue_attempted_at",
        up: "
            -- When a create's POST was first attempted (set before sending), so a
            -- retry can look for the record the earlier attempt may have made.
            ALTER TABLE sync_queue ADD COLUMN attempted_at TEXT;
        ",
    },
    Migration {
        version: 19,
        name: "pull_cursors",
        up: "
            -- Incremental-pull cursor per (team, resource): the X-Server-Time of
            -- the last fully applied pull. resource: issues | (later) comments,
            -- projects, tombstones, ...
            CREATE TABLE IF NOT EXISTS pull_cursors (
                team_id TEXT NOT NULL,
                resource TEXT NOT NULL,
                cursor TEXT NOT NULL,
                PRIMARY KEY (team_id, resource)
            );
        ",
    },
];

/// Migrations that rebuild a table other tables reference. They must run with
/// `PRAGMA foreign_keys=OFF`, which is a no-op inside a transaction, so `run`
/// toggles it around the transaction.
const FK_OFF_MIGRATIONS: &[u32] = &[15];

pub fn run(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL
        );",
    )?;

    bootstrap_legacy(conn)?;

    let applied: HashSet<u32> = {
        let mut stmt = conn.prepare("SELECT version FROM schema_migrations")?;
        let rows = stmt.query_map([], |row| row.get::<_, i64>(0).map(|v| v as u32))?;
        rows.collect::<Result<_>>()?
    };

    for migration in MIGRATIONS {
        if applied.contains(&migration.version) {
            continue;
        }
        let fk_off = FK_OFF_MIGRATIONS.contains(&migration.version);
        let fk_was_on: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
        let toggle_fk = fk_off && fk_was_on == 1;
        if toggle_fk {
            conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
        }
        let tx = conn.unchecked_transaction()?;
        if let Err(e) = tx.execute_batch(migration.up) {
            drop(tx);
            if toggle_fk {
                conn.execute_batch("PRAGMA foreign_keys=ON;")?;
            }
            return Err(e);
        }
        let now = chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            rusqlite::params![migration.version as i64, &now],
        )?;
        tx.commit()?;
        if toggle_fk {
            conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        }
        eprintln!(
            "Applied migration v{} ({})",
            migration.version, migration.name
        );
    }

    Ok(())
}

fn bootstrap_legacy(conn: &Connection) -> Result<()> {
    let migrations_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })?;
    if migrations_count > 0 {
        return Ok(());
    }

    let teams_exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='teams'",
        [],
        |row| row.get(0),
    )?;
    if teams_exists == 0 {
        return Ok(());
    }

    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    conn.execute(
        "INSERT INTO schema_migrations (version, applied_at) VALUES (1, ?1)",
        rusqlite::params![&now],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        conn
    }

    fn table_exists(conn: &Connection, name: &str) -> bool {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                rusqlite::params![name],
                |row| row.get(0),
            )
            .unwrap();
        count > 0
    }

    fn applied_versions(conn: &Connection) -> Vec<u32> {
        let mut stmt = conn
            .prepare("SELECT version FROM schema_migrations ORDER BY version")
            .unwrap();
        stmt.query_map([], |row| row.get::<_, i64>(0).map(|v| v as u32))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn fresh_install_applies_all_migrations() {
        let conn = open_mem();
        run(&conn).unwrap();

        for table in [
            "schema_migrations",
            "teams",
            "lanes",
            "issues",
            "sync_queue",
            "projects",
            "labels",
            "issue_labels",
            "project_labels",
            "members",
            "issue_dependencies",
            "comments",
            "issue_events",
        ] {
            assert!(table_exists(&conn, table), "missing table: {}", table);
        }

        assert_eq!(
            applied_versions(&conn),
            MIGRATIONS.iter().map(|m| m.version).collect::<Vec<_>>()
        );
    }

    #[test]
    fn idempotent_when_run_twice() {
        let conn = open_mem();
        run(&conn).unwrap();
        run(&conn).unwrap();
        assert_eq!(
            applied_versions(&conn),
            MIGRATIONS.iter().map(|m| m.version).collect::<Vec<_>>()
        );
    }

    #[test]
    fn legacy_install_marks_v1_applied() {
        let conn = open_mem();
        // Simulate a pre-migration-runner DB by running v1 SQL only.
        conn.execute_batch(MIGRATIONS[0].up).unwrap();
        assert!(table_exists(&conn, "teams"));
        assert!(!table_exists(&conn, "schema_migrations"));

        run(&conn).unwrap();

        // v1 should be marked applied without re-running, and v2..v8 newly applied.
        assert_eq!(
            applied_versions(&conn),
            MIGRATIONS.iter().map(|m| m.version).collect::<Vec<_>>()
        );
        assert!(table_exists(&conn, "projects"));
        assert!(table_exists(&conn, "issue_dependencies"));
        assert!(table_exists(&conn, "comments"));
    }

    #[test]
    fn legacy_data_preserved_through_upgrade() {
        let conn = open_mem();
        conn.execute_batch(MIGRATIONS[0].up).unwrap();

        let now = "2026-01-01T00:00:00.000Z";
        conn.execute(
            "INSERT INTO teams (id, name, identifier, created_at, updated_at)
             VALUES ('t1', 'Old Team', 'OLD', ?1, ?1)",
            rusqlite::params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO lanes (id, name, position, team_id, created_at, updated_at)
             VALUES ('l1', 'Backlog', 0, 't1', ?1, ?1)",
            rusqlite::params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at)
             VALUES ('i1', 'Old Issue', 2, 'l1', 't1', ?1, ?1)",
            rusqlite::params![now],
        )
        .unwrap();

        run(&conn).unwrap();

        let team_name: String = conn
            .query_row("SELECT name FROM teams WHERE id='t1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(team_name, "Old Team");

        let issue_title: String = conn
            .query_row("SELECT title FROM issues WHERE id='i1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(issue_title, "Old Issue");

        // v10 backfills a 'created' event for pre-existing issues.
        let (kind, at): (String, String) = conn
            .query_row(
                "SELECT kind, created_at FROM issue_events WHERE issue_id='i1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((kind.as_str(), at.as_str()), ("created", now));

        // New columns exist and default to NULL on legacy rows.
        let archived: Option<String> = conn
            .query_row("SELECT archived_at FROM issues WHERE id='i1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(archived.is_none());

        let counter: i64 = conn
            .query_row("SELECT issue_counter FROM teams WHERE id='t1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(counter, 0);
    }

    #[test]
    fn projects_status_nullable_keeps_rows_and_join_rows() {
        let conn = open_mem();
        // Stop before v12 to seed data under the old NOT NULL schema.
        conn.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);",
        )
        .unwrap();
        for m in MIGRATIONS.iter().filter(|m| m.version < 15) {
            conn.execute_batch(m.up).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations VALUES (?1, 'x')",
                rusqlite::params![m.version],
            )
            .unwrap();
        }
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute(
            "INSERT INTO teams (id, name, identifier, created_at, updated_at)
             VALUES ('t1', 'T', 'T', ?1, ?1)",
            rusqlite::params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO projects (id, team_id, name, status, created_at, updated_at)
             VALUES ('p1', 't1', 'P', 'started', ?1, ?1)",
            rusqlite::params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO labels (id, team_id, name, created_at, updated_at)
             VALUES ('lb', 't1', 'L', ?1, ?1)",
            rusqlite::params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project_labels (project_id, label_id, created_at, updated_at)
             VALUES ('p1', 'lb', ?1, ?1)",
            rusqlite::params![now],
        )
        .unwrap();

        run(&conn).unwrap();

        let status: Option<String> = conn
            .query_row("SELECT status FROM projects WHERE id='p1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status.as_deref(), Some("started"));
        let joins: i64 = conn
            .query_row("SELECT COUNT(*) FROM project_labels", [], |r| r.get(0))
            .unwrap();
        assert_eq!(joins, 1, "rebuild must not cascade into project_labels");
        conn.execute(
            "INSERT INTO projects (id, team_id, name, status, created_at, updated_at)
             VALUES ('p2', 't1', 'Q', NULL, ?1, ?1)",
            rusqlite::params![now],
        )
        .unwrap();
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1, "foreign_keys restored");
    }

    #[test]
    fn issue_dependencies_unique_constraint() {
        let conn = open_mem();
        run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute(
            "INSERT INTO teams (id, name, identifier, created_at, updated_at)
             VALUES ('t1', 'T', 'T', ?1, ?1)",
            rusqlite::params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO lanes (id, name, position, team_id, created_at, updated_at)
             VALUES ('l1', 'B', 0, 't1', ?1, ?1)",
            rusqlite::params![now],
        )
        .unwrap();
        for id in ["a", "b"] {
            conn.execute(
                "INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at)
                 VALUES (?1, 'i', 4, 'l1', 't1', ?2, ?2)",
                rusqlite::params![id, now],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO issue_dependencies (id, blocking_issue_id, blocked_issue_id, created_at, updated_at)
             VALUES ('d1', 'a', 'b', ?1, ?1)",
            rusqlite::params![now],
        )
        .unwrap();
        let dup = conn.execute(
            "INSERT INTO issue_dependencies (id, blocking_issue_id, blocked_issue_id, created_at, updated_at)
             VALUES ('d2', 'a', 'b', ?1, ?1)",
            rusqlite::params![now],
        );
        assert!(
            dup.is_err(),
            "expected UNIQUE violation on duplicate dependency"
        );
    }
}
