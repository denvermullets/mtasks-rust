//! The open Tabs, saved locally so a later launch can restore them. A local
//! table only: nothing here is ever enqueued for Data sync. The rules for the
//! set live in `crate::tabs`; this module only stores and loads it.

use std::collections::HashMap;

use rusqlite::{params, Connection, Result};

use crate::tabs::{Page, TabKey, TabSet};

/// What a launch reads back: the Tabs and the page each one returns to.
#[derive(Debug, Clone, PartialEq)]
pub struct Stored {
    pub tabs: TabSet,
    pub pages: HashMap<TabKey, Page>,
}

/// Replace the stored Tabs with `tabs`, keeping their order and which one is
/// active. `pages` is each Tab's page by position; a missing entry is the
/// root. One transaction, so a crash leaves the old set or the new one.
pub fn save(conn: &Connection, tabs: &TabSet, pages: &[Page]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM open_tabs", [])?;
    for (position, key) in tabs.tabs().iter().enumerate() {
        let (kind, ref_id) = to_parts(key);
        let page = pages.get(position).and_then(Page::encode);
        tx.execute(
            "INSERT INTO open_tabs (position, kind, ref_id, is_active, page)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![position as i64, kind, ref_id, (position == tabs.active_index()) as i64, page],
        )?;
    }
    tx.commit()
}

/// The stored Tabs in order with their active Tab and pages, or `None` if none are
/// stored. Rows of a kind this build doesn't know are skipped; `home` is the
/// fallback if that leaves nothing.
pub fn load(conn: &Connection, home: &TabKey) -> Result<Option<Stored>> {
    let mut stmt =
        conn.prepare("SELECT kind, ref_id, is_active, page FROM open_tabs ORDER BY position")?;
    let rows: Vec<(String, Option<String>, bool, Option<String>)> = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0, r.get(3)?))
        })?
        .collect::<Result<_>>()?;
    if rows.is_empty() {
        return Ok(None);
    }
    let mut keys = Vec::new();
    let mut pages = HashMap::new();
    let mut active = 0;
    for (kind, ref_id, is_active, page) in rows {
        if let Some(key) = from_parts(&kind, ref_id) {
            if is_active {
                active = keys.len();
            }
            // A duplicate key keeps its first page, as `from_parts` keeps its first Tab.
            pages.entry(key.clone()).or_insert_with(|| Page::decode(page.as_deref()));
            keys.push(key);
        }
    }
    Ok(Some(Stored { tabs: TabSet::from_parts(keys, active, home), pages }))
}

pub(crate) fn to_parts(key: &TabKey) -> (&'static str, Option<&str>) {
    match key {
        TabKey::Team(id) => ("team", Some(id)),
        TabKey::Project(id) => ("project", Some(id)),
        TabKey::MyIssues => ("my_issues", None),
        TabKey::Inbox => ("inbox", None),
        TabKey::Dashboard(id) => ("dashboard", Some(id)),
    }
}

pub(crate) fn from_parts(kind: &str, ref_id: Option<String>) -> Option<TabKey> {
    match (kind, ref_id) {
        ("team", Some(id)) => Some(TabKey::Team(id)),
        ("project", Some(id)) => Some(TabKey::Project(id)),
        ("my_issues", _) => Some(TabKey::MyIssues),
        ("inbox", _) => Some(TabKey::Inbox),
        ("dashboard", Some(id)) => Some(TabKey::Dashboard(id)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        conn
    }

    fn home() -> TabKey {
        TabKey::Team("home".into())
    }

    fn all_kinds() -> Vec<TabKey> {
        vec![
            TabKey::Team("t1".into()),
            TabKey::Project("p1".into()),
            TabKey::MyIssues,
            TabKey::Inbox,
            TabKey::Dashboard("d1".into()),
        ]
    }

    #[test]
    fn a_moved_tab_comes_back_in_its_new_place_with_the_same_active_tab() {
        let conn = mem();
        let mut tabs = TabSet::from_parts(all_kinds(), 1, &home());
        tabs.move_tab(1, 4);
        save(&conn, &tabs, &[]).unwrap();
        let back = load(&conn, &home()).unwrap().unwrap().tabs;
        assert_eq!(back.tabs()[4], TabKey::Project("p1".into()));
        assert_eq!(back.active(), &TabKey::Project("p1".into()));
        assert_eq!(back, tabs);
    }

    #[test]
    fn nothing_stored_loads_as_none() {
        assert_eq!(load(&mem(), &home()).unwrap(), None);
    }

    #[test]
    fn tabs_round_trip_keeping_order_and_the_active_tab() {
        let conn = mem();
        let tabs = TabSet::from_parts(all_kinds(), 3, &home());
        save(&conn, &tabs, &[]).unwrap();
        let loaded = load(&conn, &home()).unwrap().unwrap().tabs;
        assert_eq!(loaded, tabs);
        assert_eq!(loaded.active(), &TabKey::Inbox);
    }

    #[test]
    fn saving_again_replaces_the_stored_tabs() {
        let conn = mem();
        save(&conn, &TabSet::from_parts(all_kinds(), 4, &home()), &[]).unwrap();
        let second = TabSet::from_parts(vec![TabKey::MyIssues, TabKey::Team("t2".into())], 0, &home());
        save(&conn, &second, &[]).unwrap();
        assert_eq!(load(&conn, &home()).unwrap().unwrap().tabs, second);
    }

    #[test]
    fn saving_enqueues_nothing_for_data_sync() {
        let conn = mem();
        save(&conn, &TabSet::from_parts(all_kinds(), 0, &home()), &[]).unwrap();
        let queued: i64 = conn
            .query_row("SELECT COUNT(*) FROM sync_queue", [], |r| r.get(0))
            .unwrap();
        assert_eq!(queued, 0);
    }

    #[test]
    fn rows_of_an_unknown_kind_are_skipped() {
        let conn = mem();
        conn.execute_batch(
            "INSERT INTO open_tabs (position, kind, ref_id, is_active) VALUES
               (0, 'team', 't1', 0), (1, 'hologram', 'x', 1), (2, 'inbox', NULL, 0);",
        )
        .unwrap();
        let loaded = load(&conn, &home()).unwrap().unwrap().tabs;
        assert_eq!(loaded.tabs(), [TabKey::Team("t1".into()), TabKey::Inbox]);
        assert_eq!(loaded.active_index(), 0);
    }

    #[test]
    fn each_tabs_page_round_trips_and_a_missing_page_is_the_root() {
        let conn = mem();
        let keys = vec![
            TabKey::Team("t1".into()),
            TabKey::Team("t2".into()),
            TabKey::Team("t3".into()),
            TabKey::Project("p1".into()),
        ];
        let tabs = TabSet::from_parts(keys.clone(), 0, &home());
        let pages = [Page::Roadmap, Page::SavedView("v1".into()), Page::Projects];
        save(&conn, &tabs, &pages).unwrap();
        let stored = load(&conn, &home()).unwrap().unwrap();
        assert_eq!(stored.pages[&keys[0]], Page::Roadmap);
        assert_eq!(stored.pages[&keys[1]], Page::SavedView("v1".into()));
        assert_eq!(stored.pages[&keys[2]], Page::Projects);
        assert_eq!(stored.pages[&keys[3]], Page::Root);
    }

    #[test]
    fn rows_stored_before_pages_existed_load_as_the_root() {
        let conn = mem();
        conn.execute_batch(
            "INSERT INTO open_tabs (position, kind, ref_id, is_active) VALUES (0, 'team', 't1', 1);",
        )
        .unwrap();
        let stored = load(&conn, &home()).unwrap().unwrap();
        assert_eq!(stored.pages[&TabKey::Team("t1".into())], Page::Root);
    }
}
