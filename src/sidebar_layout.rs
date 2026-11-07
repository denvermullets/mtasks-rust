//! The sidebar's teams as sections: the pure counterpart of Rails
//! `User#sidebar_layout` and `shared/_sidebar_teams.html.erb`. No Iced, no
//! database; the caller passes the mirrored teams and the stored layout.
//!
//! Rails rules (all mirrored):
//! 1. Groups come in stored order; each lists its `team_ids` that match a
//!    known team, in stored order. A team shows in the first group that claims
//!    it only.
//! 2. Ungrouped teams split by role: owner -> `Owned`, admin/member -> `Joined`.
//! 3. Each of those is sorted by the stored order for its scope; teams not in
//!    the list follow, keeping their incoming order.
//! 4. `Owned` is hidden when empty and some group holds a team.
//! 5. `Joined` is hidden when empty.
//! 6. Empty groups still appear (the view says "No teams in this group").
//!
//! Desktop additions:
//! - No stored layout (`None`: old server, never pulled): one `All` section
//!   with every shown team in incoming order, i.e. the sidebar as it was.
//! - Archived teams are not in the server's layout. They are left out unless
//!   `show_archived`, then form a trailing `Archived` section (with a stored
//!   layout; the `All` section just includes them inline).
//! - Teams without a `remote_id` (local-only) can't be matched to a group or
//!   an order and count as owned.

#![allow(dead_code)] // consumed by the sidebar view (JRST-427)

use std::collections::HashSet;

use crate::db::models::Team;
use crate::db::sidebar_layout::StoredSidebar;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionKind {
    /// A user-defined group. `id` is Rails' opaque group id (what a collapse
    /// push and `db::sidebar_layout::set_collapsed` take).
    Group { id: String, name: String, collapsed: bool },
    /// "Teams you own". An empty one still renders (with an empty state).
    Owned,
    /// "Teams you've joined". Never present when empty.
    Joined,
    /// Archived teams, shown only with "Show archived".
    Archived,
    /// No stored layout: every shown team in one list, as before groups.
    All,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Section<'a> {
    pub kind: SectionKind,
    /// The section's teams, in display order.
    pub teams: Vec<&'a Team>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SidebarLayout<'a> {
    /// In display order: groups, then Owned, Joined, Archived (or just `All`).
    pub sections: Vec<Section<'a>>,
}

/// Lay out `teams` (in their incoming order, e.g. by name) for the sidebar.
pub fn sidebar_layout<'a>(
    teams: &'a [Team],
    stored: Option<&StoredSidebar>,
    show_archived: bool,
) -> SidebarLayout<'a> {
    let Some(stored) = stored else {
        let all = teams.iter().filter(|t| show_archived || t.archived_at.is_none()).collect();
        return SidebarLayout { sections: vec![Section { kind: SectionKind::All, teams: all }] };
    };

    let (archived, live): (Vec<&Team>, Vec<&Team>) =
        teams.iter().partition(|t| t.archived_at.is_some());

    let mut claimed: HashSet<i64> = HashSet::new();
    let mut sections: Vec<Section<'a>> = Vec::new();
    for g in &stored.groups {
        let members = g
            .team_ids
            .iter()
            .filter_map(|id| {
                let team = live.iter().find(|t| t.remote_id == Some(*id))?;
                claimed.insert(*id).then_some(*team)
            })
            .collect();
        sections.push(Section {
            kind: SectionKind::Group {
                id: g.id.clone(),
                name: g.name.clone(),
                collapsed: g.collapsed,
            },
            teams: members,
        });
    }
    let has_grouped_teams = sections.iter().any(|s| !s.teams.is_empty());

    let (owned, joined): (Vec<&Team>, Vec<&Team>) = live
        .into_iter()
        .filter(|t| !t.remote_id.is_some_and(|id| claimed.contains(&id)))
        .partition(|t| t.remote_id.is_none() || t.is_owner());
    let owned = ordered(owned, &stored.owned_order);
    let joined = ordered(joined, &stored.joined_order);

    if !owned.is_empty() || !has_grouped_teams {
        sections.push(Section { kind: SectionKind::Owned, teams: owned });
    }
    if !joined.is_empty() {
        sections.push(Section { kind: SectionKind::Joined, teams: joined });
    }
    if show_archived && !archived.is_empty() {
        sections.push(Section { kind: SectionKind::Archived, teams: archived });
    }
    SidebarLayout { sections }
}

/// Stable sort by position in `order` (remote ids); unlisted teams go last,
/// in incoming order.
fn ordered<'a>(mut teams: Vec<&'a Team>, order: &[i64]) -> Vec<&'a Team> {
    let fallback = order.len();
    teams.sort_by_key(|t| {
        t.remote_id.and_then(|id| order.iter().position(|o| *o == id)).unwrap_or(fallback)
    });
    teams
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sidebar_layout::SidebarGroup;

    fn team(remote: i64, role: &str) -> Team {
        Team {
            id: format!("local-{remote}"),
            name: format!("Team {remote}"),
            identifier: format!("T{remote}"),
            remote_id: Some(remote),
            archived_at: None,
            role: Some(role.into()),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn group(id: &str, teams: &[i64], collapsed: bool) -> SidebarGroup {
        SidebarGroup {
            id: id.into(),
            name: format!("Group {id}"),
            team_ids: teams.to_vec(),
            collapsed,
        }
    }

    fn stored(groups: Vec<SidebarGroup>, owned: &[i64], joined: &[i64]) -> StoredSidebar {
        StoredSidebar { groups, owned_order: owned.to_vec(), joined_order: joined.to_vec() }
    }

    fn remote_ids(s: &Section) -> Vec<i64> {
        s.teams.iter().map(|t| t.remote_id.unwrap()).collect()
    }

    fn kinds(l: &SidebarLayout) -> Vec<&'static str> {
        l.sections
            .iter()
            .map(|s| match s.kind {
                SectionKind::Group { .. } => "group",
                SectionKind::Owned => "owned",
                SectionKind::Joined => "joined",
                SectionKind::Archived => "archived",
                SectionKind::All => "all",
            })
            .collect()
    }

    #[test]
    fn groups_keep_stored_order_and_their_teams_in_stored_order() {
        let teams = vec![team(1, "owner"), team(2, "member"), team(3, "owner")];
        let s = stored(vec![group("b", &[3, 1], false), group("a", &[2], true)], &[], &[]);
        let l = sidebar_layout(&teams, Some(&s), false);
        assert_eq!(kinds(&l), ["group", "group"]);
        assert_eq!(remote_ids(&l.sections[0]), [3, 1]);
        assert_eq!(remote_ids(&l.sections[1]), [2]);
        assert_eq!(
            l.sections[1].kind,
            SectionKind::Group { id: "a".into(), name: "Group a".into(), collapsed: true }
        );
    }

    #[test]
    fn group_ids_that_match_no_known_team_are_dropped() {
        let teams = vec![team(1, "owner")];
        let s = stored(vec![group("a", &[99, 1, 98], false)], &[], &[]);
        let l = sidebar_layout(&teams, Some(&s), false);
        assert_eq!(remote_ids(&l.sections[0]), [1]);
    }

    #[test]
    fn a_team_shows_only_in_the_first_group_that_claims_it() {
        let teams = vec![team(1, "owner"), team(2, "owner")];
        let s = stored(vec![group("a", &[1, 1], false), group("b", &[2, 1], false)], &[], &[]);
        let l = sidebar_layout(&teams, Some(&s), false);
        assert_eq!(remote_ids(&l.sections[0]), [1]);
        assert_eq!(remote_ids(&l.sections[1]), [2]);
    }

    #[test]
    fn ungrouped_teams_split_by_role_owner_vs_admin_and_member() {
        let teams = vec![
            team(1, "owner"),
            team(2, "admin"),
            team(3, "member"),
            team(4, "owner"),
        ];
        let l = sidebar_layout(&teams, Some(&stored(vec![], &[], &[])), false);
        assert_eq!(kinds(&l), ["owned", "joined"]);
        assert_eq!(remote_ids(&l.sections[0]), [1, 4]);
        assert_eq!(remote_ids(&l.sections[1]), [2, 3]);
    }

    #[test]
    fn a_missing_or_unknown_role_is_joined() {
        let mut t = team(1, "owner");
        t.role = None;
        let u = team(2, "wizard");
        let teams = vec![t, u];
        let l = sidebar_layout(&teams, Some(&stored(vec![], &[], &[])), false);
        // Owned is shown empty (no group holds a team); both are joined.
        assert_eq!(kinds(&l), ["owned", "joined"]);
        assert!(l.sections[0].teams.is_empty());
        assert_eq!(remote_ids(&l.sections[1]), [1, 2]);
    }

    #[test]
    fn sections_follow_the_stored_order_and_unlisted_teams_follow_in_incoming_order() {
        let teams = vec![
            team(1, "owner"),
            team(2, "owner"),
            team(3, "owner"),
            team(4, "owner"),
            team(5, "member"),
            team(6, "member"),
            team(7, "member"),
        ];
        let s = stored(vec![], &[3, 99, 1], &[7, 5]);
        let l = sidebar_layout(&teams, Some(&s), false);
        assert_eq!(remote_ids(&l.sections[0]), [3, 1, 2, 4]);
        assert_eq!(remote_ids(&l.sections[1]), [7, 5, 6]);
    }

    #[test]
    fn owned_is_hidden_when_empty_and_a_group_holds_a_team() {
        let teams = vec![team(1, "owner"), team(2, "member")];
        let s = stored(vec![group("a", &[1], false)], &[], &[]);
        let l = sidebar_layout(&teams, Some(&s), false);
        assert_eq!(kinds(&l), ["group", "joined"]);
    }

    #[test]
    fn owned_shows_empty_when_no_group_holds_a_team() {
        let teams = vec![team(2, "member")];
        // No groups at all, and a group that holds nothing.
        for groups in [vec![], vec![group("a", &[], false)]] {
            let l = sidebar_layout(&teams, Some(&stored(groups.clone(), &[], &[])), false);
            let owned = l.sections.iter().find(|s| s.kind == SectionKind::Owned).unwrap();
            assert!(owned.teams.is_empty());
        }
    }

    #[test]
    fn joined_is_hidden_when_empty() {
        let teams = vec![team(1, "owner")];
        let l = sidebar_layout(&teams, Some(&stored(vec![], &[], &[])), false);
        assert_eq!(kinds(&l), ["owned"]);
    }

    #[test]
    fn empty_groups_still_appear() {
        let teams = vec![team(1, "owner")];
        let s = stored(vec![group("a", &[], false), group("b", &[404], false)], &[], &[]);
        let l = sidebar_layout(&teams, Some(&s), false);
        assert_eq!(kinds(&l), ["group", "group", "owned"]);
        assert!(l.sections[0].teams.is_empty());
        assert!(l.sections[1].teams.is_empty());
        assert_eq!(remote_ids(&l.sections[2]), [1]);
    }

    #[test]
    fn every_team_grouped_leaves_no_owned_or_joined_section() {
        // Three groups holding all teams (owners and members alike).
        let teams = vec![
            team(1, "owner"),
            team(2, "owner"),
            team(3, "member"),
            team(4, "admin"),
            team(5, "owner"),
        ];
        let s = stored(
            vec![
                group("bozo", &[1, 3], false),
                group("turbo", &[4], true),
                group("vektis", &[5, 2], false),
            ],
            &[1, 2, 5],
            &[3, 4],
        );
        let l = sidebar_layout(&teams, Some(&s), false);
        assert_eq!(kinds(&l), ["group", "group", "group"]);
        assert_eq!(remote_ids(&l.sections[0]), [1, 3]);
        assert_eq!(remote_ids(&l.sections[1]), [4]);
        assert_eq!(remote_ids(&l.sections[2]), [5, 2]);
    }

    #[test]
    fn no_stored_layout_is_todays_single_list() {
        let mut archived = team(3, "owner");
        archived.archived_at = Some("2026-01-01".into());
        let teams = vec![team(2, "member"), archived, team(1, "owner")];
        let l = sidebar_layout(&teams, None, false);
        assert_eq!(l.sections.len(), 1);
        assert_eq!(l.sections[0].kind, SectionKind::All);
        assert_eq!(remote_ids(&l.sections[0]), [2, 1]);
        let shown = sidebar_layout(&teams, None, true);
        assert_eq!(remote_ids(&shown.sections[0]), [2, 3, 1]);
    }

    #[test]
    fn archived_teams_are_hidden_unless_shown_then_trail_in_their_own_section() {
        let mut gone = team(2, "owner");
        gone.archived_at = Some("2026-01-01".into());
        let mut gone_grouped = team(3, "owner");
        gone_grouped.archived_at = Some("2026-01-01".into());
        let teams = vec![team(1, "owner"), gone, gone_grouped];
        let s = stored(vec![group("a", &[3], false)], &[], &[]);

        let hidden = sidebar_layout(&teams, Some(&s), false);
        assert_eq!(kinds(&hidden), ["group", "owned"]);
        assert!(hidden.sections[0].teams.is_empty());
        assert_eq!(remote_ids(&hidden.sections[1]), [1]);

        let shown = sidebar_layout(&teams, Some(&s), true);
        assert_eq!(kinds(&shown), ["group", "owned", "archived"]);
        assert_eq!(remote_ids(&shown.sections[2]), [2, 3]);
    }

    #[test]
    fn a_team_without_a_remote_id_is_owned_even_without_a_role() {
        let mut local = team(1, "member");
        local.remote_id = None;
        local.role = None;
        let teams = vec![local, team(2, "member")];
        let s = stored(vec![group("a", &[2], false)], &[], &[]);
        let l = sidebar_layout(&teams, Some(&s), false);
        assert_eq!(kinds(&l), ["group", "owned"]);
        assert_eq!(l.sections[1].teams.len(), 1);
        assert_eq!(l.sections[1].teams[0].id, "local-1");
    }
}
