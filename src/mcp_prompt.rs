//! The one-line prompt that hands an issue to Claude through the jait MCP
//! server. Mirrors Rails' `IssuesHelper#mcp_prompt_for`: the ids are Rails ids,
//! and the project part is left out when the issue has no project.

pub fn for_issue(team_remote_id: i64, project_remote_id: Option<i64>, identifier: &str) -> String {
    let mut scope = format!("team {team_remote_id}");
    if let Some(p) = project_remote_id {
        scope.push_str(&format!(" and project {p}"));
    }
    format!("using jait mcp for {scope} lets execute issue {identifier}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_rails_wording_with_a_project() {
        assert_eq!(
            for_issue(47, Some(277), "JRST-12"),
            "using jait mcp for team 47 and project 277 lets execute issue JRST-12"
        );
    }

    #[test]
    fn leaves_the_project_out_when_there_is_none() {
        assert_eq!(for_issue(4, None, "JAIT-9"), "using jait mcp for team 4 lets execute issue JAIT-9");
    }
}
