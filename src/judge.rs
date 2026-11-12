//! Which MRs of a stack can go out in one deploy, as Claude reads them. A
//! landing (land.rs) waits for the MR below each one to be live in
//! production unless Claude said here that the two can go out together, so
//! its answer shortens a wait, and has ot merge that one without asking you.
use crate::ask;
use crate::git::git_in;
use crate::gitlab::GitLab;
use crate::land::Verdict;
use crate::model::Mr;
use serde_json::Value;
use std::path::Path;

const SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"required":["mrs"],"properties":{"mrs":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["iid","together","why"],"properties":{"iid":{"type":"integer"},"together":{"type":"boolean"},"why":{"type":"string"}}}}}}"#;

/// For each MR of `layers` (a landing's, bottom first) above the bottom:
/// whether it can go out in the same deploy as the one below it, and why.
/// Claude reads each one's title, description and own diff (GitLab's, from
/// its base to its head, so a merged one reads as well as an open one) from
/// `run`, and the rest of the code in `repo`.
pub fn together(repo: &Path, gl: &GitLab, project: &str, main: &str, layers: &[Mr], run: &Path) -> Result<Vec<Verdict>, String> {
    std::fs::create_dir_all(run).map_err(|e| e.to_string())?;
    let heads: Vec<String> = layers.iter().map(|m| format!("refs/merge-requests/{}/head", m.iid)).collect();
    let mut fetch = vec!["fetch", "--quiet", "--no-write-fetch-head", "origin"];
    fetch.extend(heads.iter().map(String::as_str));
    git_in(repo, &fetch).map_err(|e| format!("could not fetch the stack: {e}"))?;
    let mut listed = String::new();
    for m in layers {
        let keep = |name: String, text: &str| std::fs::write(run.join(&name), text).map(|_| name).map_err(|e| e.to_string());
        let about = keep(format!("{}.md", m.iid), &format!("# !{} {}\n\n{}\n", m.iid, m.title, gl.description(project, m.iid)?))?;
        let patch = keep(format!("{}.patch", m.iid), &git_in(repo, &["diff", &m.base, &m.head])?)?;
        listed.push_str(&format!("- !{}: {}/{about} and {}/{patch}\n", m.iid, run.display(), run.display()));
    }
    let said = ask::claude("opus", repo, run, "Read,Grep,Glob", SCHEMA, &prompt(main, &listed))?;
    Ok(verdicts(layers, &said))
}

/// What Claude is asked of a stack going into `main`, its MRs `listed`.
fn prompt(main: &str, listed: &str) -> String {
    format!(
        "Read a stack of merge requests for which can go out in one deploy.\n\n\
ot is landing these merge requests into {main} one at a time, from the bottom, and by default each waits until the one below it is live in production. For each one above the bottom, decide whether it can instead go out in the same deploy as the one below it.\n\n\
Going out in the same deploy means both ship in one build: the upper one is never live without the lower one, so needing the lower one's code is no reason to wait. The only risk is the rollout. For a few minutes old servers and workers, with neither change, run beside new ones with both; work queued, stored or scheduled by one side is picked up by the other; and the data from before the deploy is all the new code finds. So it must wait only when that breaks something, for example when:\n\
- new code requires something old code doesn't give it, like a field in a queued message or job, a column, an index or a setting;\n\
- it turns on a path that relies on data the lower one must already have been producing for a while.\n\
It can go out together when nothing in the rollout, old workers and work already queued included, can reach what it adds: a new option or step someone has to set up after the deploy, a flag that is off, code nothing calls yet. A description saying the lower one ships first may mean only that the upper one needs its code, which shipping together already gives; but if it gives any other reason, like data, a backfill or a setting the lower one must have run first, it waits.\n\
If you are not sure, say it must wait.\n\n\
The stack, bottom first, each with its title and description and its own diff:\n{listed}\n\
The working directory is the rest of the code; read around the diffs where you need to.\n\n\
Answer for each one above the bottom: its iid, together (whether it can go out in the same deploy as the one below it), and why: one plain sentence for its author naming what, during the rollout, it does or doesn't need from the one below.\n"
    )
}

/// What Claude said of each of `layers` above the bottom, on the code it
/// read; one it said nothing of waits.
fn verdicts(layers: &[Mr], said: &Value) -> Vec<Verdict> {
    let mrs = said["mrs"].as_array().map(Vec::as_slice).unwrap_or_default();
    layers
        .windows(2)
        .map(|pair| {
            let (below, m) = (&pair[0], &pair[1]);
            let v = mrs.iter().find(|v| v["iid"].as_u64() == Some(m.iid));
            Verdict {
                iid: m.iid,
                head: m.head.clone(),
                under: below.head.clone(),
                together: v.is_some_and(|v| v["together"] == true),
                why: v
                    .and_then(|v| v["why"].as_str())
                    .unwrap_or("Claude said nothing of it, so it waits")
                    .to_string(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixture::mr;
    use serde_json::json;

    #[test]
    fn what_claude_said_of_each_and_one_it_skipped_waits() {
        let said = json!({ "mrs": [
            { "iid": 2, "together": false, "why": "It reads the stamp !1 starts writing." },
            { "iid": 9, "together": true, "why": "Not in this stack." },
        ] });
        let mut layers = [mr(1, "sam", "sam/stamp", "main"), mr(2, "sam", "sam/step", "sam/stamp"), mr(3, "sam", "sam/retry", "sam/step")];
        for m in &mut layers {
            m.head = format!("a{}", m.iid);
        }
        assert_eq!(
            verdicts(&layers, &said),
            [
                Verdict { iid: 2, head: "a2".into(), under: "a1".into(), together: false, why: "It reads the stamp !1 starts writing.".into() },
                Verdict { iid: 3, head: "a3".into(), under: "a2".into(), together: false, why: "Claude said nothing of it, so it waits".into() },
            ]
        );
    }
}
