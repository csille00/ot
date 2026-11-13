//! GitLab over HTTPS: finding your token; one GraphQL request for the open
//! MRs the board shows (`snapshot`), and two for yours that merged and how
//! far production has got (`merged`, `production`); one for the sizes it
//! doesn't know yet (`size`); what has been said on an MR (`talk`); and the
//! writes, as `Live`. Answers come back as plain structs.
use crate::model::{Mr, Production, Reach, Snapshot};
use crate::review::{Draft, Note, Talk, Thread};
use crate::write::{self, Call, Method, Write, Writer};
use serde::Deserialize;
use serde_json::json;
use std::io::Read;
use std::process::Command;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The client, made the first time a thread needs it and then shared:
/// finding the token runs glab, which is worth doing once.
#[derive(Clone)]
pub struct Shared {
    host: String,
    made: Arc<Mutex<Option<GitLab>>>,
}

impl Shared {
    pub fn new(host: &str) -> Shared {
        Shared {
            host: host.to_string(),
            made: Arc::default(),
        }
    }

    pub fn get(&self) -> Result<GitLab, String> {
        let mut made = self.made.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(g) = made.as_ref() {
            return Ok(g.clone());
        }
        let g = GitLab::connect(&self.host)?;
        *made = Some(g.clone());
        Ok(g)
    }
}

#[derive(Clone)]
pub struct GitLab {
    agent: ureq::Agent,
    host: String,
    /// One cell for every clone, so a token renewed by one thread serves all.
    token: Arc<Mutex<String>>,
    /// Each MR's base and start, by iid and head, as REST gave them.
    refs: Arc<Mutex<HashMap<(u64, String), (String, String)>>>,
}

/// GITLAB_TOKEN, else the token glab keeps for the host.
fn token(host: &str) -> Result<String, String> {
    let token = match std::env::var("GITLAB_TOKEN") {
        Ok(t) if !t.is_empty() => t,
        _ => {
            let out = Command::new("glab")
                .args(["config", "get", "token", "--host", host])
                .output()
                .map_err(|e| format!("no GITLAB_TOKEN, and glab: {e}"))?;
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
    };
    if token.is_empty() {
        return Err(format!(
            "no GitLab token: set GITLAB_TOKEN or run glab auth login --hostname {host}"
        ));
    }
    Ok(token)
}

type Sent = Result<ureq::http::Response<ureq::Body>, ureq::Error>;

impl GitLab {
    fn connect(host: &str) -> Result<GitLab, String> {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();
        Ok(GitLab {
            agent,
            host: host.to_string(),
            token: Arc::new(Mutex::new(token(host)?)),
            refs: Arc::default(),
        })
    }

    /// Sends `request` with the token as its Authorization header. glab's
    /// OAuth token dies after two hours, or sooner when glab renews it, so
    /// on a 401 this asks glab for a live one and sends once more. GitLab
    /// also answers 401 to a request it refuses, like approving twice, so a
    /// 401 on a token glab has just used is that refusal.
    fn send(&self, request: impl Fn(&str) -> Sent) -> Result<ureq::http::Response<ureq::Body>, String> {
        let held = self.token.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match request(&format!("Bearer {held}")) {
            Err(ureq::Error::StatusCode(401)) => {}
            sent => return sent.map_err(|e| self.failure(e)),
        }
        let refused = "GitLab turned that down (401), though your login works: it's done already, or not yours to do";
        let live = {
            let mut cell = self.token.lock().unwrap_or_else(|e| e.into_inner());
            if *cell == held {
                // Any API call makes glab renew a token that has run out.
                let glab = Command::new("glab")
                    .args(["api", "user", "--hostname", &self.host])
                    .output();
                if !glab.is_ok_and(|o| o.status.success()) {
                    return Err(format!(
                        "GitLab turned down your login: set GITLAB_TOKEN or run glab auth login --hostname {}",
                        self.host
                    ));
                }
                *cell = token(&self.host)?;
            }
            cell.clone()
        };
        if live == held {
            return Err(refused.into());
        }
        request(&format!("Bearer {live}")).map_err(|e| match e {
            ureq::Error::StatusCode(401) => refused.into(),
            e => self.failure(e),
        })
    }

    /// A request that went wrong, in words. A dropped network, which ureq
    /// calls an io error like "No route to host", says the host is out of reach.
    fn failure(&self, e: ureq::Error) -> String {
        match e {
            ureq::Error::Io(_) | ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => {
                format!("can't reach {}", self.host)
            }
            ureq::Error::Timeout(_) => format!("no answer from {}", self.host),
            e => format!("GitLab: {e}"),
        }
    }

    fn graphql<T: for<'de> Deserialize<'de>>(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<T, String> {
        let url = format!("https://{}/api/graphql", self.host);
        let body = json!({ "query": query, "variables": variables });
        let mut resp =
            self.send(|auth| self.agent.post(&url).header("Authorization", auth).send_json(&body))?;
        let body: Answer<T> = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("GitLab's answer: {e}"))?;
        if let Some(e) = body.errors.and_then(|e| e.into_iter().next()) {
            return Err(format!("GitLab: {}", e.message));
        }
        body.data
            .ok_or_else(|| "GitLab answered with no data".into())
    }

    /// A REST call; its answer, or null when it answers with nothing, as
    /// some writes do.
    fn rest(
        &self,
        method: Method,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let url = format!("https://{}/api/v4{path}", self.host);
        let mut resp = self.send(|auth| match method {
            Method::Get => self.agent.get(&url).header("Authorization", auth).call(),
            Method::Delete => self.agent.delete(&url).header("Authorization", auth).call(),
            Method::Put => self.agent.put(&url).header("Authorization", auth).send_json(body),
            Method::Post => self.agent.post(&url).header("Authorization", auth).send_json(body),
        })?;
        let text = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("GitLab's answer: {e}"))?;
        Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
    }

    /// Your open MRs and the open ones that wait on your review: those you
    /// were asked to review, and another's you were made the assignee of.
    /// One in several of those lists is listed once. Unsized until `size`.
    pub fn snapshot(&self, project: &str) -> Result<Snapshot, String> {
        let (d, assigned) = std::thread::scope(|s| {
            let assigned = s.spawn(|| self.graphql::<AssignedData>(&format!("{ASSIGNED}{FRAGMENTS}"), json!({ "path": project })));
            let d = self.graphql::<Data>(&format!("{BOARD}{FRAGMENTS}"), json!({ "path": project, "id": project }));
            (d, assigned.join().unwrap())
        });
        let (d, assigned) = (d?, assigned?.current_user.assigned);
        let as_of = jiff::Timestamp::now().as_second();
        let User {
            username: me,
            authored,
            review,
        } = d.current_user;
        let mut mrs: Vec<Mr> = vec![];
        for node in authored.nodes.into_iter().chain(review.nodes).chain(assigned.nodes) {
            let mr = node.into_mr();
            if !mrs.iter().any(|m| m.iid == mr.iid) {
                mrs.push(mr);
            }
        }
        // GitLab looks again at an MR whose head or target moved only when
        // asked, as opening its page does; the next board reads its answer.
        for mr in mrs.iter().filter(|m| m.rechecking) {
            let path = format!("{}?with_merge_status_recheck=true", write::mr_path(project, mr.iid));
            let _ = self.rest(Method::Get, &path, &json!({}));
        }
        self.fill_refs(project, &mut mrs);
        let default_branch = d
            .project
            .and_then(|p| p.repository)
            .and_then(|r| r.root_ref)
            .unwrap_or_else(|| "main".into());
        Ok(Snapshot {
            me,
            default_branch,
            as_of,
            mrs,
            here: String::new(),
        })
    }

    /// Your newest MRs merged into `main`, as merged (Reach::Merged). One
    /// merged into a stack's branch isn't listed: it goes out with that
    /// branch's MR. Unsized until `size`.
    pub fn merged(&self, project: &str, main: &str) -> Result<Vec<Mr>, String> {
        let d: MergedData = self.graphql(&format!("{MERGED}{FRAGMENTS}"), json!({ "path": project }))?;
        let mut mrs: Vec<Mr> = d.current_user
            .merged
            .nodes
            .into_iter()
            .filter(|n| n.target_branch == main && n.merge_commit_sha.is_some())
            .map(|n| Mr {
                reach: Some(Reach::Merged),
                ..n.into_mr()
            })
            .collect();
        self.fill_refs(project, &mut mrs);
        Ok(mrs)
    }

    /// The base and start of each MR GraphQL gave none for: its diffRefs is
    /// null on gitlab.com, though REST's diff_refs has them. Asked once per head.
    fn fill_refs(&self, project: &str, mrs: &mut [Mr]) {
        let need: Vec<(u64, String)> = {
            let known = self.refs.lock().unwrap_or_else(|e| e.into_inner());
            mrs.iter().filter(|m| m.base.is_empty() && !known.contains_key(&(m.iid, m.head.clone()))).map(|m| (m.iid, m.head.clone())).collect()
        };
        let asked: Vec<(u64, String, String, String)> = std::thread::scope(|s| {
            let chunks: Vec<_> = need
                .chunks(need.len().div_ceil(8).max(1))
                .map(|part| {
                    s.spawn(move || {
                        part.iter()
                            .filter_map(|(iid, _)| {
                                let v = self.rest(Method::Get, &write::mr_path(project, *iid), &serde_json::Value::Null).ok()?;
                                let r = &v["diff_refs"];
                                let get = |k: &str| r[k].as_str().unwrap_or_default().to_string();
                                Some((*iid, get("head_sha"), get("base_sha"), get("start_sha")))
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            chunks.into_iter().flat_map(|c| c.join().unwrap_or_default()).collect()
        });
        let mut known = self.refs.lock().unwrap_or_else(|e| e.into_inner());
        for (iid, head, base, start) in asked {
            if !base.is_empty() {
                known.insert((iid, head), (base, start));
            }
        }
        for m in mrs.iter_mut().filter(|m| m.base.is_empty()) {
            if let Some((base, start)) = known.get(&(m.iid, m.head.clone())) {
                (m.base, m.start) = (base.clone(), start.clone());
            }
        }
    }

    /// Fills in each MR's size (adds, dels, files). A diff's size is fixed
    /// by its base and head, so one in `known` with both the same lends its
    /// size, and only the rest are asked for: GitLab takes seconds to size
    /// a whole board. No files means never sized, so it is asked again.
    pub fn size(&self, project: &str, mrs: &mut [Mr], known: &[Mr]) -> Result<(), String> {
        let mut ask = vec![];
        for mr in mrs.iter_mut() {
            match known.iter().find(|k| k.files > 0 && k.head == mr.head && k.base == mr.base) {
                Some(k) => (mr.adds, mr.dels, mr.files) = (k.adds, k.dels, k.files),
                None => ask.push(mr.iid.to_string()),
            }
        }
        if ask.is_empty() {
            return Ok(());
        }
        let d: SizeData = self.graphql(SIZES, json!({ "id": project, "iids": ask }))?;
        for s in d.project.map(|p| p.merge_requests.nodes).unwrap_or_default() {
            let (Some(stats), Some(mr)) = (s.diff_stats_summary, mrs.iter_mut().find(|m| s.iid.parse() == Ok(m.iid))) else {
                continue;
            };
            (mr.adds, mr.dels, mr.files) = (stats.additions, stats.deletions, stats.files);
        }
        Ok(())
    }

    /// What production runs, and what is on its way there: the newest
    /// successful deployment of `job` to the production environment
    /// (migrations deploy there too, hence the job), and the commits of
    /// `main` whose `job` runs or waits on the deploy lock.
    pub fn production(&self, project: &str, main: &str, job: &str) -> Result<Production, String> {
        let d: ProductionData =
            self.graphql(PRODUCTION, json!({ "id": project, "main": main, "job": job }))?;
        let p = d.project.ok_or("no such project")?;
        let live = p
            .environment
            .into_iter()
            .flat_map(|e| e.deployments.nodes)
            .find(|d| d.job.as_ref().is_some_and(|j| j.name == job))
            .and_then(|d| d.sha);
        let deploying = p
            .pipelines
            .nodes
            .into_iter()
            .filter(|p| {
                p.job.as_ref().is_some_and(|j| {
                    matches!(j.status.as_str(), "RUNNING" | "PENDING" | "PREPARING" | "WAITING_FOR_RESOURCE")
                })
            })
            .filter_map(|p| p.sha)
            .collect();
        Ok(Production { live, deploying })
    }

    /// A file uploaded to the project, by the path a body links it at
    /// ("/uploads/<secret>/<name>"), which is its REST path under the
    /// project too. Read whole, past ureq's 10MB cap on a body: a screen
    /// recording can be bigger.
    pub fn upload(&self, project: &str, path: &str) -> Result<Vec<u8>, String> {
        let url = format!("https://{}/api/v4{}{path}", self.host, write::project_path(project));
        let mut resp = self.send(|auth| self.agent.get(&url).header("Authorization", auth).call())?;
        let mut bytes = vec![];
        resp.body_mut()
            .as_reader()
            .read_to_end(&mut bytes)
            .map_err(|e| format!("GitLab's upload: {e}"))?;
        Ok(bytes)
    }

    /// What the MR's author wrote of it, alone.
    pub fn description(&self, project: &str, iid: u64) -> Result<String, String> {
        let mr = self.rest(Method::Get, &write::mr_path(project, iid), &json!({}))?;
        Ok(mr["description"].as_str().unwrap_or_default().to_string())
    }

    /// Who is in the project, by username: its people, not the bots its
    /// access tokens make (often more than half). GitLab hands out 100 a page.
    pub fn members(&self, project: &str) -> Result<Vec<String>, String> {
        let mut all = vec![];
        let mut after: Option<String> = None;
        loop {
            let d: MembersData = self.graphql(MEMBERS, json!({ "id": project, "after": after }))?;
            let Some(p) = d.project else {
                break;
            };
            let people = p.project_members.nodes.into_iter().filter_map(|m| m.user);
            all.extend(people.filter(|u| !u.bot).map(|u| u.username));
            match p.project_members.page_info {
                PageInfo {
                    has_next_page: true,
                    end_cursor: Some(c),
                } => after = Some(c),
                _ => break,
            }
        }
        Ok(all)
    }

    /// What has been said on an MR: its description, its threads (on lines
    /// of its current diff, or on the MR itself), and your drafts.
    pub fn talk(&self, project: &str, iid: u64, head: &str) -> Result<Talk, String> {
        let (description, discussions) = self.discussions(project, iid)?;
        let threads = discussions
            .into_iter()
            .filter_map(|d| d.into_thread(head))
            .collect();
        let drafts = self.drafts(project, iid)?;
        Ok(Talk {
            description,
            threads,
            drafts,
        })
    }

    /// The MR's description and every discussion on it. Every push and
    /// approval is a discussion too, oldest first, so a busy MR's newest
    /// threads are pages in.
    fn discussions(&self, project: &str, iid: u64) -> Result<(String, Vec<Discussion>), String> {
        let mut all = vec![];
        let mut description = String::new();
        let mut after: Option<String> = None;
        loop {
            let vars = json!({ "id": project, "iid": iid.to_string(), "after": after });
            let d: ThreadsData = self.graphql(THREADS, vars)?;
            let Some(mr) = d.project.and_then(|p| p.merge_request) else {
                break;
            };
            description = mr.description.unwrap_or_default();
            all.extend(mr.discussions.nodes);
            match mr.discussions.page_info {
                PageInfo {
                    has_next_page: true,
                    end_cursor: Some(c),
                } => after = Some(c),
                _ => break,
            }
        }
        Ok((description, all))
    }

    /// Your drafts on the MR, not sent yet.
    fn drafts(&self, project: &str, iid: u64) -> Result<Vec<Draft>, String> {
        let path = write::mr_path(project, iid) + "/draft_notes";
        let answer = self.rest(Method::Get, &path, &serde_json::Value::Null)?;
        let nodes: Vec<DraftNode> =
            serde_json::from_value(answer).map_err(|e| format!("GitLab's drafts: {e}"))?;
        Ok(nodes.into_iter().map(DraftNode::into_draft).collect())
    }
}

impl Discussion {
    /// The thread as the review shows it. None for GitLab's own notes
    /// (pushes, approvals), and for one left on an earlier push whose line
    /// has since changed: GitLab's "outdated", which has no line here.
    fn into_thread(self, head: &str) -> Option<Thread> {
        let first = self.notes.nodes.first()?;
        let at = first.position.as_ref();
        let outdated = at.is_some_and(|at| {
            at.diff_refs.as_ref().and_then(|r| r.head_sha.as_deref()) != Some(head)
        });
        if first.system || outdated {
            return None;
        }
        Some(Thread {
            // GitLab's id is "gid://gitlab/Discussion/<id>"; REST wants the id.
            id: self.id.rsplit('/').next().unwrap_or_default().to_string(),
            path: at
                .and_then(|a| a.new_path.clone().or_else(|| a.old_path.clone()))
                .unwrap_or_default(),
            new_line: at.and_then(|a| a.new_line),
            old_line: at.and_then(|a| a.old_line),
            resolved: self.resolved,
            resolvable: self.resolvable,
            notes: self
                .notes
                .nodes
                .iter()
                .filter(|n| !n.system)
                .map(|n| Note {
                    author: n.author.username.clone(),
                    body: n.body.clone(),
                })
                .collect(),
        })
    }
}

impl DraftNode {
    fn into_draft(self) -> Draft {
        let at = self.position.as_ref();
        Draft {
            id: self.id,
            path: at
                .and_then(|p| p.new_path.clone().or_else(|| p.old_path.clone()))
                .unwrap_or_default(),
            new_line: at.and_then(|p| p.new_line),
            old_line: at.and_then(|p| p.old_line),
            reply_to: self.discussion_id,
            body: self.note,
        }
    }
}

/// GitLab, for writes, as the one interface has them.
pub struct Live {
    pub gitlab: Shared,
    pub project: String,
}

impl Writer for Live {
    fn write(&self, w: &Write) -> Result<(), String> {
        let g = self.gitlab.get()?;
        match write::call(&self.project, w) {
            Call::Rest { method, path, body } => g.rest(method, &path, &body).map(|_| ()),
            Call::Graphql { query, variables } => {
                // A mutation that fails says so in its own errors field, not
                // in GraphQL's.
                let answer: serde_json::Value = g.graphql(query, variables)?;
                let refused = answer
                    .as_object()
                    .into_iter()
                    .flat_map(|o| o.values())
                    .filter_map(|v| v["errors"].as_array())
                    .flatten()
                    .find_map(|e| e.as_str());
                match refused {
                    Some(e) => Err(format!("GitLab: {e}")),
                    None => Ok(()),
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct DraftNode {
    id: u64,
    note: String,
    discussion_id: Option<String>,
    position: Option<DraftPosition>,
}

#[derive(Deserialize)]
struct DraftPosition {
    new_path: Option<String>,
    old_path: Option<String>,
    new_line: Option<u32>,
    old_line: Option<u32>,
}

const THREADS: &str = "query($id: ID!, $iid: String!, $after: String) { project(fullPath: $id) { mergeRequest(iid: $iid) { description
  discussions(first: 100, after: $after) { pageInfo { hasNextPage endCursor } nodes { id resolved resolvable notes { nodes { body system author { username }
    position { newLine oldLine newPath oldPath diffRefs { headSha } } } } } } } } }";

const MEMBERS: &str = "query($id: ID!, $after: String) { project(fullPath: $id) {
  projectMembers(first: 100, after: $after) { pageInfo { hasNextPage endCursor } nodes { user { username bot } } } } }";

#[derive(Deserialize)]
struct MembersData {
    project: Option<MembersProject>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MembersProject {
    project_members: Page<Member>,
}

/// GitLab's schema lets a member have no user.
#[derive(Deserialize)]
struct Member {
    user: Option<Person>,
}

#[derive(Deserialize)]
struct Person {
    username: String,
    bot: bool,
}

#[derive(Deserialize)]
struct ThreadsData {
    project: Option<ThreadsProject>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsProject {
    merge_request: Option<ThreadsMr>,
}

#[derive(Deserialize)]
struct ThreadsMr {
    description: Option<String>,
    discussions: Page<Discussion>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page<T> {
    nodes: Vec<T>,
    page_info: PageInfo,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
struct Discussion {
    id: String,
    resolved: bool,
    resolvable: bool,
    notes: Nodes<NoteNode>,
}

#[derive(Deserialize)]
struct NoteNode {
    body: String,
    system: bool,
    author: Name,
    position: Option<NotePosition>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotePosition {
    new_line: Option<u32>,
    old_line: Option<u32>,
    new_path: Option<String>,
    old_path: Option<String>,
    diff_refs: Option<DiffRefs>,
}

const BOARD: &str = "query($path: String!, $id: ID!) {
  project(fullPath: $id) { repository { rootRef } }
  currentUser { username
    authoredMergeRequests(state: opened, first: 100, projectPath: $path) { nodes { ...mr } }
    reviewRequestedMergeRequests(state: opened, first: 100, projectPath: $path) { nodes { ...mr } } } }";

/// Its own request, as MERGED is: a third list puts the board's at 270 of
/// the 250 GitLab allows a query.
const ASSIGNED: &str = "query($path: String!) { currentUser {
    assigned: assignedMergeRequests(state: opened, first: 100, projectPath: $path) { nodes { ...mr } } } }";

/// Its own request: one more list puts the board's over GitLab's
/// complexity limit.
const MERGED: &str = "query($path: String!) { currentUser {
    merged: authoredMergeRequests(state: merged, first: 20, projectPath: $path, sort: MERGED_AT_DESC) { nodes { ...mr mergeCommitSha } } } }";

/// Not in the mr fragment: in the same list as detailedMergeStatus,
/// diffStatsSummary turns a clean stacked MR's status to CONFLICT (seen
/// on two stacked MRs at once).
const SIZES: &str = "query($id: ID!, $iids: [String!]) { project(fullPath: $id) {
  mergeRequests(iids: $iids, first: 100) { nodes { iid diffStatsSummary { additions deletions fileCount } } } } }";

const FRAGMENTS: &str = "
fragment mr on MergeRequest { iid title draft webUrl author { username } sourceBranch targetBranch detailedMergeStatus mergeError autoMergeEnabled conflicts updatedAt
  resolvableDiscussionsCount resolvedDiscussionsCount approved diffHeadSha diffRefs { headSha baseSha startSha }
  approvedBy { nodes { username } } reviewers { nodes { username } }
  headPipeline { id status jobs(statuses: [FAILED], retried: false, first: 50) { nodes { name allowFailure } } } }";

// GitLab allows one deployments list per request, so what is running
// comes from main's pipelines, as each one's deploy job.
const PRODUCTION: &str = "query($id: ID!, $main: String!, $job: String!) { project(fullPath: $id) {
  environment(name: \"production\") { deployments(statuses: [SUCCESS], orderBy: {finishedAt: DESC}, first: 20) { nodes { sha job { name } } } }
  pipelines(ref: $main, first: 20) { nodes { sha job(name: $job) { status } } } } }";

#[derive(Deserialize)]
struct ProductionData {
    project: Option<ProductionProject>,
}

#[derive(Deserialize)]
struct ProductionProject {
    environment: Option<Environment>,
    pipelines: Nodes<MainPipeline>,
}

#[derive(Deserialize)]
struct Environment {
    deployments: Nodes<Deployment>,
}

#[derive(Deserialize)]
struct Deployment {
    sha: Option<String>,
    job: Option<JobName>,
}

#[derive(Deserialize)]
struct JobName {
    name: String,
}

#[derive(Deserialize)]
struct MainPipeline {
    sha: Option<String>,
    job: Option<JobStatus>,
}

#[derive(Deserialize)]
struct JobStatus {
    status: String,
}

#[derive(Deserialize)]
struct Answer<T> {
    data: Option<T>,
    errors: Option<Vec<Error>>,
}

#[derive(Deserialize)]
struct Error {
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Data {
    project: Option<Project>,
    current_user: User,
}

#[derive(Deserialize)]
struct Project {
    repository: Option<Repository>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository {
    root_ref: Option<String>,
}

#[derive(Deserialize)]
struct User {
    username: String,
    #[serde(rename = "authoredMergeRequests")]
    authored: Nodes<Node>,
    #[serde(rename = "reviewRequestedMergeRequests")]
    review: Nodes<Node>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssignedData {
    current_user: AssignedUser,
}

#[derive(Deserialize)]
struct AssignedUser {
    assigned: Nodes<Node>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MergedData {
    current_user: MergedUser,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MergedUser {
    merged: Nodes<Node>,
}

#[derive(Deserialize)]
struct SizeData {
    project: Option<SizeProject>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SizeProject {
    merge_requests: Nodes<Size>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Size {
    iid: String,
    diff_stats_summary: Option<Stats>,
}

#[derive(Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Deserialize)]
struct Name {
    username: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Node {
    iid: String,
    title: String,
    draft: bool,
    web_url: String,
    author: Name,
    source_branch: String,
    target_branch: String,
    detailed_merge_status: Option<String>,
    merge_error: Option<String>,
    auto_merge_enabled: bool,
    conflicts: bool,
    updated_at: String,
    resolvable_discussions_count: Option<u32>,
    resolved_discussions_count: Option<u32>,
    approved: bool,
    diff_refs: Option<DiffRefs>,
    diff_head_sha: Option<String>,
    merge_commit_sha: Option<String>,
    approved_by: Option<Nodes<Name>>,
    reviewers: Option<Nodes<Name>>,
    head_pipeline: Option<Pipeline>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiffRefs {
    head_sha: Option<String>,
    base_sha: Option<String>,
    start_sha: Option<String>,
}

#[derive(Clone, Copy, Default, Deserialize)]
struct Stats {
    additions: u64,
    deletions: u64,
    #[serde(rename = "fileCount")]
    files: u64,
}

#[derive(Deserialize)]
struct Pipeline {
    id: Option<String>,
    status: String,
    jobs: Option<Nodes<Job>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Job {
    name: String,
    allow_failure: bool,
}

impl Node {
    /// Sized 0 until `GitLab::size`.
    fn into_mr(self) -> Mr {
        let names = |n: Option<Nodes<Name>>| {
            n.map(|n| n.nodes.into_iter().map(|u| u.username).collect())
                .unwrap_or_default()
        };
        let refs = self.diff_refs.unwrap_or(DiffRefs {
            head_sha: None,
            base_sha: None,
            start_sha: None,
        });
        let pipeline_id = self
            .head_pipeline
            .as_ref()
            .and_then(|p| p.id.as_ref())
            .and_then(|id| id.rsplit('/').next()?.parse().ok())
            .unwrap_or(0);
        let (pipeline, failed_jobs) = match self.head_pipeline {
            Some(p) => (
                p.status.to_lowercase(),
                p.jobs
                    .map(|j| {
                        j.nodes
                            .into_iter()
                            .filter(|j| !j.allow_failure)
                            .map(|j| j.name)
                            .collect()
                    })
                    .unwrap_or_default(),
            ),
            None => (String::new(), vec![]),
        };
        let rechecking = !self.conflicts && self.detailed_merge_status.as_deref() == Some("CONFLICT");
        Mr {
            iid: self.iid.parse().unwrap_or(0),
            title: self.title,
            draft: self.draft,
            author: self.author.username,
            source: self.source_branch,
            target: self.target_branch,
            url: self.web_url,
            // GitLab marks every MR UNCHECKED when its target moves and looks
            // again only when someone opens it, so that blocks nothing here.
            mergeable: matches!(
                self.detailed_merge_status.as_deref(),
                Some("MERGEABLE" | "UNCHECKED" | "CHECKING" | "CI_STILL_RUNNING")
            ),
            merge_error: self.merge_error.unwrap_or_default(),
            auto_merge: self.auto_merge_enabled,
            // conflicts reads false while GitLab rechecks an MR whose head or
            // target moved (CANNOT_BE_MERGED_RECHECK); this status keeps CONFLICT.
            conflicts: self.conflicts || rechecking,
            rechecking,
            updated: self
                .updated_at
                .parse::<jiff::Timestamp>()
                .map(|t| t.as_second())
                .unwrap_or(0),
            open_threads: self
                .resolvable_discussions_count
                .unwrap_or(0)
                .saturating_sub(self.resolved_discussions_count.unwrap_or(0)),
            approved: self.approved,
            approved_by: names(self.approved_by),
            reviewers: names(self.reviewers),
            pipeline,
            failed_jobs,
            adds: 0,
            dels: 0,
            files: 0,
            head: refs.head_sha.or(self.diff_head_sha).unwrap_or_default(),
            base: refs.base_sha.unwrap_or_default(),
            start: refs.start_sha.unwrap_or_default(),
            pipeline_id,
            split_age: None,
            reach: None,
            merge_commit: self.merge_commit_sha.unwrap_or_default(),
            carries: None,
        }
    }
}
