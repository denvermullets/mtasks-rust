//! The team's Terminals as the strip lists them: names and order, with no
//! engine. A name is the user's, never the program's title (JRST-399).
//! JRST-413 stores one record per entry: team, name, working directory.
//!
//! The roster reports each change to the strip (`take_changes`) and the app
//! stores them (`db::terminal_records::apply`), so the roster itself knows
//! nothing of the database.
use std::path::PathBuf;

use super::TerminalId;
use crate::db::terminal_records::{Change, Stored};

#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    /// The stored record's id: stable across launches and restarts, unlike `id`.
    pub key: String,
    pub id: TerminalId,
    pub team: String,
    pub name: String,
    /// The directory the shell was started in; Restart reuses it.
    pub cwd: Option<PathBuf>,
    /// Whether `id` is a real shell's. False for a restored record that has
    /// not started: it is in the strip but has no engine.
    pub shell: bool,
}

#[derive(Debug)]
pub struct Roster {
    records: Vec<Record>,
    changes: Vec<Change>,
    /// Placeholder ids handed to restored records, counting down from the top
    /// of the id space; the engine counts up from 1, so they never meet.
    next_placeholder: u64,
}

impl Default for Roster {
    fn default() -> Self {
        Roster { records: Vec::new(), changes: Vec::new(), next_placeholder: u64::MAX }
    }
}

/// "Terminal N" with the lowest N no name in `taken` uses.
pub fn default_name(taken: &[&str]) -> String {
    let n = (1..)
        .find(|n| !taken.contains(&format!("Terminal {n}").as_str()))
        .unwrap_or(1);
    format!("Terminal {n}")
}

impl Roster {
    /// Add a Terminal to its team's strip; returns the default name it got.
    pub fn add(&mut self, id: TerminalId, team: &str, cwd: Option<PathBuf>) -> String {
        let name = {
            let taken: Vec<&str> = self.of_team(team).map(|r| r.name.as_str()).collect();
            default_name(&taken)
        };
        let key = uuid::Uuid::new_v4().to_string();
        self.changes.push(Change::Added(Stored {
            key: key.clone(),
            team_id: team.to_string(),
            name: name.clone(),
            cwd: cwd.clone(),
        }));
        self.records.push(Record { key, id, team: team.to_string(), name: name.clone(), cwd, shell: true });
        name
    }

    /// Put stored records back in the strip without a shell. Each gets a
    /// placeholder id until `replace_id` gives it its shell's; nothing is
    /// reported as a change, since it is already stored.
    pub fn restore(&mut self, stored: Vec<Stored>) {
        for s in stored {
            let id = TerminalId(self.next_placeholder);
            self.next_placeholder -= 1;
            self.records.push(Record {
                key: s.key,
                id,
                team: s.team_id,
                name: s.name,
                cwd: s.cwd,
                shell: false,
            });
        }
    }

    /// The record with this id if it has no shell yet (restored and never
    /// shown): the one a pane starting it needs.
    pub fn unstarted(&self, id: TerminalId) -> Option<&Record> {
        self.get(id).filter(|r| !r.shell)
    }

    pub fn remove(&mut self, id: TerminalId) {
        if let Some(r) = self.get(id) {
            self.changes.push(Change::Removed { key: r.key.clone() });
        }
        self.records.retain(|r| r.id != id);
    }

    /// The changes since the last call, in order, for the app to store.
    pub fn take_changes(&mut self) -> Vec<Change> {
        std::mem::take(&mut self.changes)
    }

    pub fn get(&self, id: TerminalId) -> Option<&Record> {
        self.records.iter().find(|r| r.id == id)
    }

    pub fn of_team<'a>(&'a self, team: &'a str) -> impl Iterator<Item = &'a Record> {
        self.records.iter().filter(move |r| r.team == team)
    }

    pub fn ids_of_team(&self, team: &str) -> Vec<TerminalId> {
        self.of_team(team).map(|r| r.id).collect()
    }

    /// Rename; an empty (or blank) name is refused and the old one kept.
    /// Returns whether the name changed.
    pub fn rename(&mut self, id: TerminalId, name: &str) -> bool {
        let name = name.trim();
        match self.records.iter_mut().find(|r| r.id == id) {
            Some(r) if !name.is_empty() && r.name != name => {
                r.name = name.to_string();
                self.changes.push(Change::Renamed { key: r.key.clone(), name: r.name.clone() });
                true
            }
            _ => false,
        }
    }

    /// A restarted shell has a new id but keeps its name, place and directory.
    pub fn replace_id(&mut self, old: TerminalId, new: TerminalId) {
        if let Some(r) = self.records.iter_mut().find(|r| r.id == old) {
            r.id = new;
            r.shell = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u64) -> TerminalId {
        TerminalId(n)
    }

    #[test]
    fn names_count_up_per_team() {
        let mut r = Roster::default();
        assert_eq!(r.add(id(1), "a", None), "Terminal 1");
        assert_eq!(r.add(id(2), "a", None), "Terminal 2");
        assert_eq!(r.add(id(3), "b", None), "Terminal 1");
    }

    #[test]
    fn a_freed_number_is_reused_lowest_first() {
        let mut r = Roster::default();
        for n in 1..=3 {
            r.add(id(n), "a", None);
        }
        r.remove(id(1));
        r.remove(id(2));
        assert_eq!(r.add(id(4), "a", None), "Terminal 1");
        assert_eq!(r.add(id(5), "a", None), "Terminal 2");
        assert_eq!(r.add(id(6), "a", None), "Terminal 4");
    }

    #[test]
    fn a_renamed_terminal_frees_its_number_and_keeps_the_new_name() {
        let mut r = Roster::default();
        r.add(id(1), "a", None);
        assert!(r.rename(id(1), "  claude "));
        assert_eq!(r.get(id(1)).unwrap().name, "claude");
        assert_eq!(r.add(id(2), "a", None), "Terminal 1");
    }

    #[test]
    fn an_empty_name_is_refused() {
        let mut r = Roster::default();
        r.add(id(1), "a", None);
        assert!(!r.rename(id(1), "   "));
        assert_eq!(r.get(id(1)).unwrap().name, "Terminal 1");
    }

    #[test]
    fn a_restarted_terminal_keeps_name_place_and_directory() {
        let mut r = Roster::default();
        r.add(id(1), "a", Some("/tmp".into()));
        r.add(id(2), "a", None);
        r.rename(id(1), "api");
        r.replace_id(id(1), id(9));
        assert_eq!(r.ids_of_team("a"), vec![id(9), id(2)]);
        let rec = r.get(id(9)).unwrap();
        assert_eq!((rec.name.as_str(), rec.cwd.clone()), ("api", Some("/tmp".into())));
    }

    #[test]
    fn adding_renaming_and_removing_are_reported_once_in_order() {
        let mut r = Roster::default();
        r.add(id(1), "a", Some("/tmp".into()));
        r.rename(id(1), "api");
        r.rename(id(1), "api"); // no change, nothing reported
        r.remove(id(1));
        let key = match &r.take_changes()[..] {
            [Change::Added(s), Change::Renamed { key: k1, name }, Change::Removed { key: k2 }] => {
                assert_eq!((s.team_id.as_str(), s.name.as_str(), s.cwd.clone()), ("a", "Terminal 1", Some("/tmp".into())));
                assert_eq!(name, "api");
                assert_eq!((k1, k2), (&s.key, &s.key));
                s.key.clone()
            }
            other => panic!("unexpected changes {other:?}"),
        };
        assert!(!key.is_empty());
        assert!(r.take_changes().is_empty());
    }

    #[test]
    fn a_restart_keeps_the_stored_record() {
        let mut r = Roster::default();
        r.add(id(1), "a", None);
        let key = r.get(id(1)).unwrap().key.clone();
        r.take_changes();
        r.replace_id(id(1), id(2));
        assert_eq!(r.get(id(2)).unwrap().key, key);
        assert!(r.take_changes().is_empty());
    }

    fn stored(key: &str, team: &str, name: &str, cwd: Option<&str>) -> Stored {
        Stored { key: key.into(), team_id: team.into(), name: name.into(), cwd: cwd.map(Into::into) }
    }

    #[test]
    fn restored_records_are_listed_by_name_with_no_shell_and_nothing_to_store() {
        let mut r = Roster::default();
        r.restore(vec![
            stored("k1", "a", "claude", Some("/work")),
            stored("k2", "a", "Terminal 2", None),
            stored("k3", "b", "Terminal 1", None),
        ]);
        let names: Vec<_> = r.of_team("a").map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["claude", "Terminal 2"]);
        assert!(r.of_team("a").all(|x| !x.shell));
        assert!(r.take_changes().is_empty());
        let ids = r.ids_of_team("a");
        assert_ne!(ids[0], ids[1]);
    }

    #[test]
    fn a_restored_record_waits_to_start_until_given_a_shell_and_keeps_its_key() {
        let mut r = Roster::default();
        r.restore(vec![stored("k1", "a", "claude", Some("/work"))]);
        let placeholder = r.ids_of_team("a")[0];
        let rec = r.unstarted(placeholder).unwrap();
        assert_eq!(rec.cwd, Some("/work".into()));
        r.replace_id(placeholder, id(7));
        assert!(r.unstarted(id(7)).is_none());
        assert_eq!(r.get(id(7)).unwrap().key, "k1");
        assert!(r.take_changes().is_empty());
    }

    #[test]
    fn a_new_terminal_after_a_restore_takes_the_lowest_free_name_and_never_a_restored_id() {
        let mut r = Roster::default();
        r.restore(vec![stored("k1", "a", "Terminal 1", None)]);
        let placeholder = r.ids_of_team("a")[0];
        assert_eq!(r.add(id(1), "a", None), "Terminal 2");
        assert_ne!(placeholder, id(1));
    }

    #[test]
    fn closing_a_restored_record_removes_its_stored_record() {
        let mut r = Roster::default();
        r.restore(vec![stored("k1", "a", "Terminal 1", None)]);
        r.remove(r.ids_of_team("a")[0]);
        assert_eq!(r.take_changes(), vec![Change::Removed { key: "k1".into() }]);
    }
}
