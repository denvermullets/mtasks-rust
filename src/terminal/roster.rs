//! The team's Terminals as the strip lists them: names and order, with no
//! engine. A name is the user's, never the program's title (JRST-399).
//! JRST-413 stores one record per entry: team, name, working directory.
use std::path::PathBuf;

use super::TerminalId;

#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub id: TerminalId,
    pub team: String,
    pub name: String,
    /// The directory the shell was started in; Restart reuses it.
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct Roster {
    records: Vec<Record>,
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
        self.records.push(Record { id, team: team.to_string(), name: name.clone(), cwd });
        name
    }

    pub fn remove(&mut self, id: TerminalId) {
        self.records.retain(|r| r.id != id);
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
                true
            }
            _ => false,
        }
    }

    /// A restarted shell has a new id but keeps its name, place and directory.
    pub fn replace_id(&mut self, old: TerminalId, new: TerminalId) {
        if let Some(r) = self.records.iter_mut().find(|r| r.id == old) {
            r.id = new;
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
}
