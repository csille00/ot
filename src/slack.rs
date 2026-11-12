//! Your Slack workspace, for who can be cc'd: everyone in it but bots,
//! guests and those gone, by email.
use serde_json::Value;
use std::time::Duration;

/// Their emails, sorted. Slack hands members out 200 at a time, so this is
/// several requests in a large workspace.
pub fn emails(token: &str) -> Result<Vec<String>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into();
    let mut all = vec![];
    let mut cursor = String::new();
    loop {
        let page: Value = agent
            .get("https://slack.com/api/users.list")
            .query("limit", "200")
            .query("cursor", &cursor)
            .header("Authorization", &format!("Bearer {token}"))
            .call()
            .and_then(|mut r| r.body_mut().read_json())
            .map_err(|e| format!("Slack: {e}"))?;
        if page["ok"] != true {
            return Err(format!("Slack: {}", page["error"].as_str().unwrap_or("refused")));
        }
        all.extend(people(&page));
        cursor = page["response_metadata"]["next_cursor"].as_str().unwrap_or_default().to_string();
        if cursor.is_empty() {
            break;
        }
    }
    all.sort();
    Ok(all)
}

/// The emails of a users.list page's people.
fn people(page: &Value) -> impl Iterator<Item = String> + '_ {
    let members = page["members"].as_array().into_iter().flatten();
    members
        .filter(|m| {
            let not = |k: &str| m[k] != true;
            not("deleted") && not("is_bot") && not("is_restricted") && not("is_ultra_restricted") && m["id"] != "USLACKBOT"
        })
        .filter_map(|m| m["profile"]["email"].as_str().map(String::from))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_people_who_work_here_can_be_ccd() {
        let page = json!({ "members": [
            { "id": "U1", "profile": { "email": "kim@team.example" } },
            { "id": "U2", "deleted": true, "profile": { "email": "gone@team.example" } },
            { "id": "U3", "is_bot": true, "profile": {} },
            { "id": "USLACKBOT", "profile": {} },
            { "id": "U4", "is_restricted": true, "profile": { "email": "guest@shop.example" } },
        ] });
        assert_eq!(people(&page).collect::<Vec<_>>(), ["kim@team.example"]);
    }
}
