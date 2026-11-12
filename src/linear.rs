//! Your Linear tickets, read with a personal API key (git config
//! ot.linearKey): the open ones assigned to you that are in a project, or
//! in one made since ot.linearSince, with the open tickets that block each,
//! the newest comment on it, and whether it has an MR.
//! Read only; the board shows them among your MRs (model::board).
use crate::model::Ticket;
use serde_json::{json, Value};
use std::time::Duration;

/// A ticket's blockers are its inverse "blocks" relations. Comments come
/// a page at a time in an order the API doesn't promise, so the newest
/// of a page is picked here.
const QUERY: &str = r#"
query($filter: IssueFilter) {
  viewer {
    assignedIssues(first: 50, filter: $filter) {
      nodes {
        identifier title url
        state { name type }
        project { name }
        inverseRelations(first: 10) { nodes { type issue { identifier state { type } } } }
        comments(first: 50) { nodes { body createdAt } }
        attachments(first: 10) { nodes { url metadata } }
      }
    }
  }
}"#;

/// The state types Linear counts as closed.
const CLOSED: [&str; 3] = ["completed", "canceled", "duplicate"];

pub fn tickets(key: &str, since: Option<jiff::civil::Date>) -> Result<Vec<Ticket>, String> {
    let mut project = json!({ "null": false });
    if let Some(since) = since {
        project["createdAt"] = json!({ "gt": since.to_string() });
    }
    let filter = json!({
        "state": { "type": { "nin": CLOSED } },
        "project": project,
    });
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .build()
        .into();
    let answer: Value = agent
        .post("https://api.linear.app/graphql")
        .header("Authorization", key)
        .send_json(json!({ "query": QUERY, "variables": { "filter": filter } }))
        .and_then(|mut r| r.body_mut().read_json())
        .map_err(|e| format!("Linear: {e}"))?;
    if let Some(e) = answer["errors"].as_array().and_then(|es| es.first()) {
        return Err(format!(
            "Linear: {}",
            e["message"].as_str().unwrap_or("refused")
        ));
    }
    Ok(of(&answer))
}

/// The tickets an answer names.
fn of(answer: &Value) -> Vec<Ticket> {
    let nodes = answer["data"]["viewer"]["assignedIssues"]["nodes"]
        .as_array()
        .into_iter()
        .flatten();
    nodes.map(ticket).collect()
}

fn ticket(n: &Value) -> Ticket {
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let open = |state: &Value| !state["type"].as_str().is_some_and(|t| CLOSED.contains(&t));
    let blocked_by = n["inverseRelations"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["type"] == "blocks" && open(&r["issue"]["state"]))
        .map(|r| s(&r["issue"]["identifier"]))
        .collect();
    let newest = n["comments"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| Some((seconds(c["createdAt"].as_str()?)?, s(&c["body"]))))
        .max_by_key(|(at, _)| *at);
    let (commented, comment) = newest.unwrap_or_default();
    // Linear's GitLab integration attaches each MR it sees for a ticket.
    let mrs: Vec<String> = n["attachments"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| s(&a["url"]).contains("/-/merge_requests/") && a["metadata"]["status"] != "closed")
        .map(|a| s(&a["url"]))
        .collect();
    Ticket {
        id: s(&n["identifier"]),
        title: s(&n["title"]),
        url: s(&n["url"]),
        state: s(&n["state"]["name"]),
        started: n["state"]["type"] == "started",
        project: s(&n["project"]["name"]),
        blocked_by,
        comment,
        commented,
        has_mr: !mrs.is_empty(),
        mrs,
    }
}

fn seconds(iso: &str) -> Option<i64> {
    iso.parse::<jiff::Timestamp>().ok().map(|t| t.as_second())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ticket_is_what_linear_says_with_only_open_blockers_and_its_newest_comment() {
        let answer = json!({ "data": { "viewer": { "assignedIssues": { "nodes": [{
            "identifier": "AP-917",
            "title": "Filter parse errors by line",
            "url": "https://linear.app/acme/issue/AP-917",
            "state": { "name": "Todo", "type": "unstarted" },
            "project": { "name": "Parser migration agents" },
            "inverseRelations": { "nodes": [
                { "type": "blocks", "issue": { "identifier": "AP-899", "state": { "type": "started" } } },
                { "type": "blocks", "issue": { "identifier": "AP-896", "state": { "type": "completed" } } },
                { "type": "related", "issue": { "identifier": "AP-900", "state": { "type": "started" } } }
            ] },
            "comments": { "nodes": [
                { "body": "Pushed b927a3d.", "createdAt": "2027-01-15T10:00:00Z" },
                { "body": "Branch and worktree ready.", "createdAt": "2027-01-15T09:00:00Z" }
            ] },
            "attachments": { "nodes": [
                { "url": "https://git.example/team/app/-/merge_requests/9", "metadata": { "status": "closed" } }
            ] }
        }] } } } });
        let t = &of(&answer)[0];
        assert_eq!(
            (t.id.as_str(), t.state.as_str(), t.started),
            ("AP-917", "Todo", false)
        );
        assert_eq!(
            t.blocked_by,
            ["AP-899"],
            "a done blocker and a related ticket don't block"
        );
        assert_eq!(
            (t.comment.as_str(), t.commented),
            ("Pushed b927a3d.", 1_800_007_200)
        );
        assert!(!t.has_mr && t.mrs.is_empty(), "a closed MR is no MR");
        let mut answer = answer;
        answer["data"]["viewer"]["assignedIssues"]["nodes"][0]["attachments"]["nodes"][0]
            ["metadata"]["status"] = json!("merged");
        let t = &of(&answer)[0];
        assert!(t.has_mr);
        assert_eq!(t.mrs, ["https://git.example/team/app/-/merge_requests/9"]);
    }
}
