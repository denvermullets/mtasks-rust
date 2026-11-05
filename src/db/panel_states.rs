//! Each Tab's Terminal panel, saved locally so a relaunch can bring it back
//! (JRST-415, JRST-422). A local table only: nothing here is ever enqueued for
//! Data sync.
//!
//! Panes are stored by the Terminal record's `key` (see `terminal_records`),
//! never by the engine's `TerminalId`, which is new on every spawn.
//!
//! A row belongs to a Tab, or to `None`: the one implicit Tab, which is how
//! panel state was saved before Tabs had their own. A launch moves it onto a
//! real Tab and the next save drops it.

use rusqlite::{params, Connection, Result};

use crate::db::open_tabs::{from_parts, to_parts};
use crate::tabs::TabKey;

/// Panes a layout can have, as in `terminal::panel::MAX_PANES`.
pub const PANES: usize = 4;

/// One panel's state, with no engine ids.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StoredPanel {
    pub visible: bool,
    /// `None` means the default height.
    pub height: Option<f32>,
    /// "single", "columns" or "grid"; anything else reads as single.
    pub layout: String,
    /// The Terminal record key in each pane, by slot.
    pub panes: [Option<String>; PANES],
    pub focused_pane: usize,
}

/// Replace every stored panel with `rows`. One transaction, so a crash leaves
/// the old set or the new one. A Tab appears once; a repeat keeps the first.
pub fn save(conn: &Connection, rows: &[(Option<TabKey>, StoredPanel)]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM panel_states", [])?;
    for (tab, p) in rows {
        let (kind, tab_ref) = match tab {
            Some(key) => {
                let (kind, r) = to_parts(key);
                (kind, r.unwrap_or(""))
            }
            None => ("implicit", ""),
        };
        let panes = serde_json::to_string(&p.panes).unwrap_or_else(|_| "[]".into());
        tx.execute(
            "INSERT OR IGNORE INTO panel_states
                 (tab_kind, tab_ref, visible, height, layout, panes, focused_pane)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                kind,
                tab_ref,
                p.visible as i64,
                p.height.map(f64::from),
                p.layout,
                panes,
                p.focused_pane as i64
            ],
        )?;
    }
    tx.commit()
}

/// Every stored panel. A row of a Tab kind this build doesn't know is skipped,
/// and a damaged `panes` value reads as empty panes.
pub fn load(conn: &Connection) -> Result<Vec<(Option<TabKey>, StoredPanel)>> {
    let mut stmt = conn.prepare(
        "SELECT tab_kind, tab_ref, visible, height, layout, panes, focused_pane FROM panel_states",
    )?;
    let rows = stmt.query_map([], |r| {
        let kind: String = r.get(0)?;
        let tab_ref: String = r.get(1)?;
        let panes: String = r.get(5)?;
        let panes: Vec<Option<String>> = serde_json::from_str(&panes).unwrap_or_default();
        let mut slots: [Option<String>; PANES] = Default::default();
        for (slot, key) in panes.into_iter().take(PANES).enumerate() {
            slots[slot] = key;
        }
        Ok((
            kind,
            tab_ref,
            StoredPanel {
                visible: r.get::<_, i64>(2)? != 0,
                height: r.get::<_, Option<f64>>(3)?.map(|h| h as f32),
                layout: r.get(4)?,
                panes: slots,
                focused_pane: r.get::<_, i64>(6)?.max(0) as usize,
            },
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (kind, tab_ref, panel) = row?;
        if kind == "implicit" {
            out.push((None, panel));
        } else if let Some(key) = from_parts(&kind, Some(tab_ref).filter(|r| !r.is_empty())) {
            out.push((Some(key), panel));
        }
    }
    Ok(out)
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

    fn panel(layout: &str, panes: [Option<&str>; PANES]) -> StoredPanel {
        StoredPanel {
            visible: true,
            height: Some(310.5),
            layout: layout.into(),
            panes: panes.map(|k| k.map(str::to_string)),
            focused_pane: 1,
        }
    }

    #[test]
    fn nothing_stored_loads_empty() {
        assert!(load(&mem()).unwrap().is_empty());
    }

    #[test]
    fn the_implicit_panel_round_trips() {
        let conn = mem();
        let p = panel("columns", [Some("k1"), None, Some("k2"), None]);
        save(&conn, &[(None, p.clone())]).unwrap();
        assert_eq!(load(&conn).unwrap(), vec![(None, p)]);
    }

    #[test]
    fn a_default_height_round_trips_as_none() {
        let conn = mem();
        let p = StoredPanel { layout: "single".into(), ..StoredPanel::default() };
        save(&conn, &[(None, p.clone())]).unwrap();
        assert_eq!(load(&conn).unwrap()[0].1, p);
    }

    #[test]
    fn each_tabs_panel_round_trips_separately() {
        let conn = mem();
        let rows = vec![
            (Some(TabKey::Team("t1".into())), panel("grid", [Some("a"), Some("b"), None, None])),
            (Some(TabKey::Project("p1".into())), panel("single", [Some("a"), None, None, None])),
            (Some(TabKey::Team("t2".into())), StoredPanel { layout: "single".into(), ..Default::default() }),
        ];
        save(&conn, &rows).unwrap();
        let mut loaded = load(&conn).unwrap();
        let mut want = rows;
        loaded.sort_by_key(|(k, _)| format!("{k:?}"));
        want.sort_by_key(|(k, _)| format!("{k:?}"));
        assert_eq!(loaded, want);
    }

    #[test]
    fn saving_again_replaces_what_was_stored() {
        let conn = mem();
        save(&conn, &[(None, panel("grid", [None, None, None, None]))]).unwrap();
        let second = vec![(Some(TabKey::Team("t".into())), panel("columns", [Some("x"), None, None, None]))];
        save(&conn, &second).unwrap();
        assert_eq!(load(&conn).unwrap(), second);
    }

    #[test]
    fn a_damaged_panes_value_reads_as_empty_panes() {
        let conn = mem();
        conn.execute(
            "INSERT INTO panel_states (tab_kind, tab_ref, visible, layout, panes, focused_pane)
             VALUES ('implicit', '', 1, 'grid', 'not json', 0)",
            [],
        )
        .unwrap();
        let loaded = load(&conn).unwrap();
        assert_eq!(loaded[0].1.panes, <[Option<String>; PANES]>::default());
    }

    #[test]
    fn saving_enqueues_nothing_for_data_sync() {
        let conn = mem();
        save(&conn, &[(None, panel("grid", [Some("a"), None, None, None]))]).unwrap();
        let queued: i64 = conn.query_row("SELECT COUNT(*) FROM sync_queue", [], |r| r.get(0)).unwrap();
        assert_eq!(queued, 0);
    }
}
