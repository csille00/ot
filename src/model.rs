//! What the board says, from what GitLab and local git said: one move per MR,
//! the MRs as a tree of stacks off the default branch, and the facts the
//! panel lists for one of them. No IO; the clock is handed in.
use crate::claude::{Session, State};
use crate::hc;
use crate::keys::Act;
use crate::write::{At, Write};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const DAY: i64 = 86_400;

/// A merge request as one refresh saw it: open, or yours, merged into the
/// default branch and not live yet.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mr {
    pub iid: u64,
    pub title: String,
    pub draft: bool,
    pub author: String,
    pub source: String,
    pub target: String,
    pub url: String,
    /// GitLab's detailedMergeStatus names nothing for you to do first: it
    /// is mergeable, or waits only on GitLab's own check or the pipeline.
    pub mergeable: bool,
    /// Why GitLab's last try at merging it failed, in its words; empty when
    /// none has. GitLab keeps it until the next try.
    pub merge_error: String,
    /// Set to merge by itself once GitLab waits on nothing: its pipeline,
    /// approvals.
    pub auto_merge: bool,
    pub conflicts: bool,
    /// The conflict is GitLab's answer from before the head or the target
    /// last moved, and it hasn't looked again yet.
    pub rechecking: bool,
    /// Unix seconds.
    pub updated: i64,
    pub open_threads: u32,
    /// Its approval rules are met.
    pub approved: bool,
    pub approved_by: Vec<String>,
    pub reviewers: Vec<String>,
    /// The head pipeline's status in GitLab's words, lowercased; empty for none.
    pub pipeline: String,
    /// The head pipeline's failed jobs, leaving out those allowed to fail.
    pub failed_jobs: Vec<String>,
    pub adds: u64,
    pub dels: u64,
    pub files: u64,
    pub head: String,
    pub base: String,
    pub start: String,
    /// The head pipeline's id; 0 for none.
    pub pipeline_id: u64,
    /// Seconds from where the branch split off its target to the target's
    /// tip, from local git. Only read for your own MRs; None for anyone
    /// else's, or when the head or the target isn't local.
    pub split_age: Option<i64>,
    /// How far it has got since it merged; None while open.
    pub reach: Option<Reach>,
    /// The commit its merge made; empty while open.
    pub merge_commit: String,
    /// Yours, open on the default branch: the merged MR whose own commits
    /// it still has, as it sat on that and hasn't been rebased past it since
    /// (git::carries). None when local git can't say.
    pub carries: Option<u64>,
}

/// How far a merged commit has got toward production (git::reach).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reach {
    Merged,
    Deploying,
    Live,
}

/// What production runs and what is on its way there, as commits on the
/// default branch (GitLab::production).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Production {
    /// The newest successful deploy's; None when GitLab lists none.
    pub live: Option<String>,
    /// Those a deploy is running for or waiting on the lock with, newest first.
    pub deploying: Vec<String>,
}

/// One of your Linear tickets that is open and in a project (linear.rs).
/// Its branch, its MR and the agent the ticket swarm puts on it are each
/// named after its id, which is how the board finds them (key).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ticket {
    /// "AP-899".
    pub id: String,
    pub title: String,
    pub url: String,
    /// Its workflow state, as Linear names it: "Todo", "In Progress".
    pub state: String,
    /// That state is one Linear counts as started.
    pub started: bool,
    pub project: String,
    /// The open tickets that block it, by id.
    pub blocked_by: Vec<String>,
    /// The newest comment on it, and when, unix seconds; empty and 0 for none.
    pub comment: String,
    pub commented: i64,
    /// Linear has an MR for it that isn't closed. The ticket shows by that
    /// MR, so once it is live and off the board, the ticket is too.
    pub has_mr: bool,
    /// Those MRs' web urls, which find its MR when the branch is named
    /// without its id.
    pub mrs: Vec<String>,
}

/// The ticket a branch or an agent is for, by the id its last part starts
/// with: the branch "sam/ap-899-fix-tables" and the agent "ap-899" are
/// both AP-899's, as "ap-899". None when it starts with no id.
pub fn key(name: &str) -> Option<String> {
    let last = name.rsplit('/').next().unwrap_or(name);
    let (team, rest) = last.split_once('-')?;
    let n = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let is_id = team.starts_with(|c: char| c.is_ascii_alphabetic())
        && team.chars().all(|c| c.is_ascii_alphanumeric())
        && n > 0
        && (n == rest.len() || rest[n..].starts_with('-'));
    is_id.then(|| format!("{team}-{}", &rest[..n]).to_lowercase())
}

/// Claude at work on a row: a session in a worktree made for its branch,
/// an agent named after its ticket, as the ticket swarm names them
/// ("ap-899"), or an agent hc runs on its branch.
#[derive(Clone, Debug, PartialEq)]
pub struct Worker {
    /// The session, or the one that started the agent; none for hc's,
    /// whose transcript ot doesn't read.
    pub session: Option<String>,
    /// The agent's name; None for a session itself.
    pub agent: Option<String>,
    /// What the panel calls it: the agent's name, or the session's title.
    pub name: String,
    /// Busy on something, or idle, waiting to be told more.
    pub state: State,
    pub doing: String,
    /// Unix seconds of when it last moved, which also tells its turns
    /// apart: a brief (brief.rs) is of the one it stopped on.
    pub at: i64,
}

/// Claude at work, as running sessions and hc show it: the agents named
/// after a ticket, by its key, and by branch the sessions in a worktree of
/// their own and hc's agents on a turn.
#[derive(Default)]
struct Workers {
    named: HashMap<String, Worker>,
    on: HashMap<String, Worker>,
}

impl Workers {
    /// Of two by one name or on one branch, the one that moved last. hc
    /// says only that an agent is working, so it counts as moving `now`.
    fn of(sessions: &[Session], agents: &[hc::Agent], now: i64) -> Workers {
        fn keep(map: &mut HashMap<String, Worker>, k: String, w: Worker) {
            if map.get(&k).is_none_or(|was| was.at < w.at) {
                map.insert(k, w);
            }
        }
        let mut out = Workers::default();
        for s in sessions.iter().filter(|s| s.state != State::Gone) {
            if let Some(b) = &s.works_on {
                let w = Worker {
                    session: Some(s.id.clone()),
                    agent: None,
                    name: s.title.clone(),
                    state: s.state,
                    doing: s.doing(),
                    at: s.at,
                };
                keep(&mut out.on, b.clone(), w);
            }
            for a in s.turns.iter().filter_map(|t| t.agent.as_ref()).filter(|a| a.running()) {
                let Some(k) = key(&a.name).filter(|k| *k == a.name.to_lowercase()) else {
                    continue;
                };
                let w = Worker {
                    session: Some(s.id.clone()),
                    agent: Some(a.name.clone()),
                    name: a.name.clone(),
                    state: if a.waiting { State::Idle } else { State::Busy },
                    doing: a.doing(),
                    at: a.turns.last().map_or(s.started, |t| t.at),
                };
                keep(&mut out.named, k, w);
            }
        }
        // By branch alone: the swarm names an agent after the ticket it was
        // started for, and sends it on to fix others' MRs.
        for a in agents.iter().filter(|a| a.status == hc::Status::Working && !a.branch.is_empty()) {
            let w = Worker {
                session: None,
                agent: None,
                name: a.name.clone(),
                state: State::Busy,
                doing: format!("hc {}", a.address),
                at: now,
            };
            keep(&mut out.on, a.branch.clone(), w);
        }
        out
    }

    /// Who is at work on an MR from `branch`, for the ticket of `k`: a
    /// session on that very branch, or an agent named after its ticket.
    fn on_mr(&self, branch: &str, k: Option<&String>) -> Option<Worker> {
        let named = k.and_then(|k| self.named.get(k));
        newest([self.on.get(branch), named].into_iter().flatten())
    }

    /// Who is at work on the ticket of `k`, which has no MR: an agent named
    /// after it, or a session on a branch for it.
    fn on_ticket(&self, k: &str) -> Option<Worker> {
        let on = self.on.iter().filter(|(b, _)| key(b).as_deref() == Some(k)).map(|(_, w)| w);
        newest(on.chain(self.named.get(k)))
    }
}

fn newest<'a>(ws: impl Iterator<Item = &'a Worker>) -> Option<Worker> {
    ws.max_by_key(|w| w.at).cloned()
}

impl Mr {
    pub fn pipeline_failed(&self) -> bool {
        self.pipeline == "failed"
    }
}

/// The names in `names` other than yours.
pub fn others<'a>(names: &'a [String], me: &str) -> Vec<&'a str> {
    names
        .iter()
        .map(String::as_str)
        .filter(|n| *n != me)
        .collect()
}

/// Everything one refresh learned; also what the cache keeps.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub me: String,
    pub default_branch: String,
    /// When GitLab answered, unix seconds; 0 for never.
    pub as_of: i64,
    pub mrs: Vec<Mr>,
    /// The branch checked out here; empty when HEAD is detached. Read again
    /// with every refresh, since you switch branches while the board is open.
    pub here: String,
}

impl Snapshot {
    pub fn mr(&self, iid: u64) -> Option<&Mr> {
        self.mrs.iter().find(|m| m.iid == iid)
    }

    /// The MR this one is stacked on: the one whose source branch it
    /// targets, the lowest if two share that branch.
    pub fn parent(&self, mr: &Mr) -> Option<&Mr> {
        self.mrs
            .iter()
            .filter(|p| p.source == mr.target && p.iid != mr.iid)
            .min_by_key(|p| p.iid)
    }
}

/// What one repo's CI and branch names mean to the board (git config ot.*).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rules {
    /// Taken off the front of your own branch names.
    pub prefixes: Vec<String>,
    /// The CI job that fails a branch whose base is too old.
    pub stale_job: Option<String>,
    /// How many days that job allows between the base and the target's tip.
    pub stale_days: Option<i64>,
    /// The CI job that deploys the default branch to production; without
    /// it, your MRs leave the board when they merge.
    pub deploy_job: Option<String>,
}

/// The one thing a row wants next. The first rule that applies wins (move_of).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Draft,
    Rebase,
    Broke,
    Conflicts,
    Answer(u32),
    AskReview,
    Merge,
    /// Set to merge itself once GitLab waits on nothing.
    AutoMerge,
    Running,
    InReview,
    Approved,
    Review,
    Merged,
    Deploying,
}

/// A color by what it means; the UI holds the palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Dim,
    Faint,
    Amber,
    Orange,
    Red,
    Green,
    Pink,
    Teal,
}

impl Move {
    pub fn word(self) -> String {
        match self {
            Move::Draft => "draft".into(),
            Move::Rebase => "rebase it".into(),
            Move::Broke => "pipeline broke".into(),
            Move::Conflicts => "fix conflicts".into(),
            Move::Answer(n) => format!("answer {n} {}", plural(n as u64, "thread", "threads")),
            Move::AskReview => "ask for review".into(),
            Move::Merge => "merge it".into(),
            Move::AutoMerge => "merges itself".into(),
            Move::Running => "running".into(),
            Move::InReview => "in review".into(),
            Move::Approved => "you approved".into(),
            Move::Review => "review it".into(),
            Move::Merged => "merged".into(),
            Move::Deploying => "deploying".into(),
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Move::Draft => "·",
            Move::Running | Move::AutoMerge | Move::Deploying => "◐",
            Move::InReview | Move::Merged => "○",
            Move::Approved => "✓",
            _ => "●",
        }
    }

    pub fn tone(self) -> Tone {
        match self {
            Move::Draft => Tone::Faint,
            Move::Rebase | Move::Conflicts | Move::Running => Tone::Amber,
            Move::Broke => Tone::Red,
            Move::Answer(_) | Move::AskReview | Move::Review => Tone::Orange,
            Move::Merge | Move::AutoMerge | Move::Approved => Tone::Green,
            Move::InReview => Tone::Pink,
            Move::Merged | Move::Deploying => Tone::Teal,
        }
    }

    /// It waits on you, not on GitLab or someone else.
    pub fn on_you(self) -> bool {
        matches!(
            self,
            Move::Rebase | Move::Broke | Move::Conflicts | Move::Answer(_) | Move::AskReview | Move::Merge | Move::Review
        )
    }

    /// The board action that does what this move wants, which the footer
    /// offers first.
    pub fn act(self) -> Option<Act> {
        match self {
            Move::Rebase => Some(Act::Rebase),
            Move::Broke => Some(Act::Retry),
            Move::AskReview => Some(Act::Reviewers),
            Move::Merge => Some(Act::Merge),
            Move::Draft => Some(Act::Ready),
            Move::Review => Some(Act::Review),
            _ => None,
        }
    }
}

fn running(pipeline: &str) -> bool {
    matches!(
        pipeline,
        "running" | "pending" | "created" | "preparing" | "waiting_for_resource" | "scheduled"
    )
}

/// Whether anyone but you was asked to review it.
fn sent(mr: &Mr, me: &str) -> bool {
    !others(&mr.reviewers, me).is_empty()
}

/// CI fails a branch whose base is too old, and that is not a break: the
/// fix is a rebase, so red stays for a real one. Either CI already said so
/// (only the stale job failed), or the base is older than it allows, in
/// whole days as CI counts them.
fn wants_rebase(mr: &Mr, rules: &Rules) -> bool {
    let only_stale = mr.pipeline_failed()
        && matches!(mr.failed_jobs.as_slice(), [only] if rules.stale_job.as_ref() == Some(only));
    let too_old =
        matches!((rules.stale_days, mr.split_age), (Some(days), Some(age)) if age / DAY > days);
    only_stale || too_old
}

/// The first rule that applies, in this order.
pub fn move_of(mr: &Mr, me: &str, rules: &Rules) -> Move {
    match mr.reach {
        Some(Reach::Deploying) => return Move::Deploying,
        Some(_) => return Move::Merged,
        None => {}
    }
    if mr.author != me {
        return if mr.approved_by.iter().any(|u| u == me) {
            Move::Approved
        } else {
            Move::Review
        };
    }
    if mr.draft {
        Move::Draft
    } else if wants_rebase(mr, rules) {
        Move::Rebase
    } else if mr.pipeline_failed() {
        Move::Broke
    } else if mr.conflicts {
        Move::Conflicts
    } else if mr.open_threads > 0 {
        Move::Answer(mr.open_threads)
    } else if !sent(mr, me) {
        Move::AskReview
    } else if mr.auto_merge {
        Move::AutoMerge
    } else if mr.approved && mr.mergeable {
        Move::Merge
    } else if running(&mr.pipeline) {
        Move::Running
    } else {
        Move::InReview
    }
}

/// The board: the tree of what wants you, your tickets among it, Claude's
/// sessions on this repo, and the fold of your MRs nobody was asked to
/// review that have gone quiet. Each is drawn top to bottom.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Board {
    pub tree: Vec<Row>,
    pub claude: Vec<Row>,
    pub not_sent: Vec<Row>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// The tree lines before the row's glyph: "│  ├─ ".
    pub rails: String,
    /// The tree lines on a line the row wraps onto, as wide as rails plus
    /// the glyph and its space, so wrapped text sits under the first line's.
    pub wrap_rails: String,
    pub item: Item,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Mr {
        iid: u64,
        branch: String,
        mv: Move,
        /// The id of the ticket it is for, if it is one of yours.
        ticket: Option<String>,
        agent: Option<Worker>,
    },
    /// One of your tickets that has no MR yet, by its title, since it has
    /// no branch either.
    Ticket {
        id: String,
        title: String,
        /// Its state, as Linear names it.
        state: String,
        /// That state is one Linear counts as started.
        started: bool,
        /// The first of the open tickets that block it.
        waits_on: Option<String>,
        agent: Option<Worker>,
    },
    /// The name of a project of yours, over the stacks for its tickets.
    Label(String),
    /// A rail alone, between your MRs, others' and the branch you are on.
    Gap,
    /// The branch you are on, when no listed MR is from it.
    Here(String),
    /// A session of Claude Code: its name, then what it is on under it.
    Claude {
        id: String,
        title: String,
        state: State,
        doing: String,
        /// How long since it last moved: "4m".
        ago: String,
        /// What the panel says of it, one fact each.
        facts: Vec<String>,
        /// hc's address for the terminal it runs in, if hc runs it.
        terminal: Option<String>,
    },
    /// An agent hc runs, and the open MR from the branch it is on.
    Hc {
        /// hc's address for it, which the cursor rests on it by.
        address: String,
        name: String,
        status: hc::Status,
        /// What it says it is on, in its own words.
        said: String,
        mr: Option<u64>,
    },
}

impl State {
    pub fn word(self) -> &'static str {
        match self {
            State::Busy => "working",
            State::Idle => "waiting on you",
            State::Gone => "done",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            State::Busy => "◐",
            State::Idle => "○",
            State::Gone => "·",
        }
    }

    pub fn tone(self) -> Tone {
        match self {
            State::Busy => Tone::Amber,
            State::Idle => Tone::Teal,
            State::Gone => Tone::Dim,
        }
    }
}

/// How far one of your tickets has got on its way from a ticket to
/// production, the track the panel draws under its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Ticket,
    /// Started in Linear, or an agent is on it, and no MR yet.
    Agent,
    Draft,
    Review,
    /// Merged, or deploying.
    Merged,
    Live,
}

impl Stage {
    pub const ALL: [Stage; 6] = [Stage::Ticket, Stage::Agent, Stage::Draft, Stage::Review, Stage::Merged, Stage::Live];

    /// Where a row for one of your tickets is: its MR by its move, else
    /// the ticket by whether it has started. None for any other row.
    pub fn of(item: &Item) -> Option<Stage> {
        match item {
            Item::Mr { ticket: Some(_), mv, .. } => Some(match mv {
                Move::Draft => Stage::Draft,
                Move::Merged | Move::Deploying => Stage::Merged,
                _ => Stage::Review,
            }),
            Item::Ticket { started, agent, .. } if *started || agent.is_some() => Some(Stage::Agent),
            Item::Ticket { .. } => Some(Stage::Ticket),
            _ => None,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Stage::Ticket => "ticket",
            Stage::Agent => "agent",
            Stage::Draft => "draft",
            Stage::Review => "review",
            Stage::Merged => "merged",
            Stage::Live => "live",
        }
    }
}

/// Seconds as "now", "4m", "2h" or "3d".
pub fn ago(secs: i64) -> String {
    match secs {
        s if s < 60 => "now".into(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < DAY => format!("{}h", s / 3600),
        s => format!("{}d", s / DAY),
    }
}

fn session_facts(s: &Session, now: i64) -> Vec<String> {
    let mut facts = vec![s.state.word().to_string()];
    if !s.branch.is_empty() {
        facts.push(format!("on {}", s.branch));
    }
    if let Some(dir) = s.cwd.file_name() {
        facts.push(format!("in {}", dir.to_string_lossy()));
    }
    if s.started > 0 {
        facts.push(format!("started {} ago", ago(now - s.started)));
    }
    if s.cost > 0.0 {
        facts.push(format!("${:.2}", s.cost));
    }
    facts
}

/// How long one of yours nobody was asked to review stays on the board
/// before it folds into "not sent".
pub const QUIET: i64 = 14 * DAY;

/// `was_on` is who each MR was stacked on (`remember`), where it is drawn
/// once GitLab has moved it off. Your stacks for a project's tickets are
/// drawn together under its name, with its tickets that have no MR yet
/// (Waiting), and an agent named after a ticket on its row. hc's agents
/// are listed with Claude's sessions, those waiting on you first, then
/// those at work: one here that is a session is that session's row, and
/// one that stopped leaves once it has been quiet for SHOWN_FOR.
pub fn board(
    snap: &Snapshot,
    rules: &Rules,
    was_on: &HashMap<u64, u64>,
    sessions: &[Session],
    agents: &[hc::Agent],
    tickets: &[Ticket],
    now: i64,
) -> Board {
    let me = snap.me.as_str();
    let (roots, children) = stacks(snap, was_on);
    let by_iid: HashMap<u64, &Mr> = snap.mrs.iter().map(|m| (m.iid, m)).collect();
    let tickets: HashMap<String, &Ticket> = tickets.iter().filter_map(|t| Some((key(&t.id)?, t))).collect();
    // The ticket each MR is for, by key: the id its branch starts with,
    // else the ticket of yours Linear links it to.
    let linked: HashMap<&str, &String> = tickets.iter().flat_map(|(k, t)| t.mrs.iter().map(move |u| (u.as_str(), k))).collect();
    let keys: HashMap<u64, String> = snap
        .mrs
        .iter()
        .filter_map(|m| Some((m.iid, key(&m.source).or_else(|| linked.get(m.url.as_str()).map(|k| (*k).clone()))?)))
        .collect();
    // A stack folds only whole, so a stack is never split across the two.
    let quiet = |iid: u64| {
        let m = by_iid[&iid];
        m.author == me && !sent(m, me) && now - m.updated > QUIET
    };
    // Approved by you, it is off your plate: gone, unless the stack still
    // has one waiting on you.
    let done = |iid: u64| by_iid[&iid].approved_by.iter().any(|u| u == me);
    let (mut mine, mut others, mut folded) = (vec![], vec![], vec![]);
    for root in roots {
        let all = stack(root, &children);
        if all.iter().all(|&i| quiet(i)) {
            folded.push(root);
        } else if all.iter().all(|&i| done(i)) {
            continue;
        } else if by_iid[&root].author == me {
            mine.push(root);
        } else {
            others.push(root);
        }
    }
    // A ticket waits under an MR drawn in the tree, never one folded away
    // or gone from the board; one with an MR anywhere is that MR.
    let mut drawn: HashMap<String, u64> = HashMap::new();
    for i in mine.iter().chain(&others).flat_map(|&r| stack(r, &children)) {
        if let Some(k) = keys.get(&i) {
            let iid = drawn.entry(k.clone()).or_insert(i);
            *iid = (*iid).min(i);
        }
    }
    let mut has_mr: HashSet<String> = keys.values().cloned().collect();
    has_mr.extend(tickets.iter().filter(|(_, t)| t.has_mr).map(|(k, _)| k.clone()));
    let waiting = Waiting::of(&tickets, &has_mr, &drawn);
    // Yours by the project of the first ticket in the stack, those for
    // none first.
    let mut projects: BTreeMap<&str, Vec<Top>> = BTreeMap::new();
    let mut loose = vec![];
    for root in mine {
        let project = stack(root, &children)
            .into_iter()
            .find_map(|i| tickets.get(keys.get(&i)?))
            .map(|t| t.project.as_str());
        match project {
            Some(p) => projects.entry(p).or_default().push(Top::Stack(root)),
            None => loose.push(Top::Stack(root)),
        }
    }
    for t in &waiting.top {
        projects.entry(t.project.as_str()).or_default().push(Top::Ticket(t));
    }
    let mut groups = vec![loose];
    groups.extend(projects.into_iter().map(|(p, tops)| {
        let mut group = vec![Top::Label(p.to_string())];
        group.extend(tops);
        group
    }));
    groups.push(others.into_iter().map(Top::Stack).collect());
    let rows = RowMaker {
        by_iid: &by_iid,
        keys: &keys,
        children: &children,
        me,
        rules,
        tickets: &tickets,
        waiting: &waiting,
        workers: &Workers::of(sessions, agents, now),
    };
    // An agent hc runs here is the session of the same process.
    let mut terminals: HashSet<&str> = HashSet::new();
    let mut claude: Vec<((u8, i64), Item)> = sessions
        .iter()
        .map(|s| {
            let terminal = agents.iter().find(|a| !a.address.contains('/') && s.pid.is_some() && a.pid == s.pid);
            terminals.extend(terminal.map(|a| a.address.as_str()));
            let rank = match s.state {
                State::Busy => 1,
                State::Idle => 2,
                State::Gone => 4,
            };
            let item = Item::Claude {
                id: s.id.clone(),
                title: s.title.clone(),
                state: s.state,
                doing: s.doing(),
                ago: ago(now - s.at),
                facts: session_facts(s, now),
                terminal: terminal.map(|a| a.address.clone()),
            };
            ((rank, -s.at), item)
        })
        .collect();
    let shown = agents
        .iter()
        .filter(|a| !terminals.contains(a.address.as_str()))
        .filter(|a| a.status != hc::Status::Stopped || now - a.at <= crate::claude::SHOWN_FOR);
    claude.extend(shown.map(|a| {
        let rank = match a.status {
            hc::Status::Blocked => 0,
            hc::Status::Working => 1,
            hc::Status::Stopped => 3,
        };
        let item = Item::Hc {
            address: a.address.clone(),
            name: a.name.clone(),
            status: a.status,
            said: a.said.clone(),
            mr: snap.mrs.iter().find(|m| !a.branch.is_empty() && m.source == a.branch && m.reach.is_none()).map(|m| m.iid),
        };
        ((rank, -a.at), item)
    }));
    claude.sort_by_key(|(order, _)| *order);
    Board {
        tree: rows.forest(&top_level(snap, groups)),
        claude: claude
            .into_iter()
            .map(|(_, item)| Row {
                rails: String::new(),
                wrap_rails: "  ".into(),
                item,
            })
            .collect(),
        not_sent: rows.forest(&folded.into_iter().map(Top::Stack).collect::<Vec<_>>()),
    }
}

/// Who each MR was stacked on, child to parent: what `snap` stacks now,
/// and what was stacked before on one that has merged since and is still
/// on the board. GitLab moves an MR onto the default branch once the one
/// under it merges and its branch goes, and keeps no word of where it was,
/// so the board remembers it until that one is live.
pub fn remember(was_on: &HashMap<u64, u64>, snap: &Snapshot) -> HashMap<u64, u64> {
    let merged = |i: u64| snap.mr(i).is_some_and(|m| m.reach.is_some());
    let mut now: HashMap<u64, u64> = was_on
        .iter()
        .filter(|&(&c, &p)| snap.mr(c).is_some() && merged(p))
        .map(|(&c, &p)| (c, p))
        .collect();
    now.extend(snap.mrs.iter().filter_map(|m| Some((m.iid, snap.parent(m)?.iid))));
    now
}

/// The MR `mr` is stacked on: the one whose branch it targets, or once that
/// merged and GitLab moved `mr` off it, the one `was_on` remembers it on,
/// or the one whose commits git says it still has.
pub fn under<'a>(snap: &'a Snapshot, was_on: &HashMap<u64, u64>, mr: &Mr) -> Option<&'a Mr> {
    let merged = was_on.get(&mr.iid).copied().or(mr.carries);
    snap.parent(mr).or_else(|| snap.mr(merged?))
}

/// The MRs as stacks: the roots, lowest first, and each MR's children, the
/// MRs `under` puts on it.
fn stacks(snap: &Snapshot, was_on: &HashMap<u64, u64>) -> (Vec<u64>, HashMap<u64, Vec<u64>>) {
    let mut mrs: Vec<&Mr> = snap.mrs.iter().collect();
    mrs.sort_by_key(|m| m.iid);
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut roots = vec![];
    for m in &mrs {
        match under(snap, was_on, m).map(|p| p.iid) {
            Some(p) => children.entry(p).or_default().push(m.iid),
            None => roots.push(m.iid),
        }
    }
    // GitLab allows two MRs to target each other's branches. Such a ring has
    // no root, so its lowest MR becomes one, and the ring's way back to it is
    // cut so walking the stack ends.
    let mut reached: HashSet<u64> = roots.iter().flat_map(|&r| stack(r, &children)).collect();
    for m in &mrs {
        if !reached.contains(&m.iid) {
            roots.push(m.iid);
            for v in children.values_mut() {
                v.retain(|&c| c != m.iid);
            }
            reached.extend(stack(m.iid, &children));
        }
    }
    (roots, children)
}

/// What a rebase of the open MR `iid` moves, bottom first: the lowest open
/// MR under it, then every open MR stacked on that, each after the one it
/// is on; and the merged MR that lowest one sits on, if it sits on one,
/// whose commits the rebase leaves out. A merged MR ends a stack here, so
/// each MR on it is rebased on its own and one that can't be holds up none
/// of the others. Nothing for a merged `iid`.
pub fn layers<'a>(snap: &'a Snapshot, was_on: &HashMap<u64, u64>, iid: u64) -> (Vec<&'a Mr>, Option<u64>) {
    let Some(mut root) = snap.mr(iid).filter(|m| m.reach.is_none()) else {
        return (vec![], None);
    };
    let mut merged = None;
    let mut seen = vec![root.iid];
    while let Some(p) = under(snap, was_on, root).filter(|p| !seen.contains(&p.iid)) {
        if p.reach.is_some() {
            merged = Some(p.iid);
            break;
        }
        seen.push(p.iid);
        root = p;
    }
    let (_, children) = stacks(snap, was_on);
    let open = stack(root.iid, &children)
        .into_iter()
        .filter_map(|i| snap.mr(i))
        .filter(|m| m.reach.is_none())
        .collect();
    (open, merged)
}

/// A root and every MR stacked on it, top down.
fn stack(root: u64, children: &HashMap<u64, Vec<u64>>) -> Vec<u64> {
    let mut all = vec![root];
    let mut i = 0;
    while i < all.len() {
        all.extend(children.get(&all[i]).into_iter().flatten());
        i += 1;
    }
    all
}

/// What hangs off the default branch, top to bottom: `groups` (yours, each
/// project's, others'), then the branch you are on, a gap between each
/// group there is.
fn top_level<'a>(snap: &Snapshot, groups: Vec<Vec<Top<'a>>>) -> Vec<Top<'a>> {
    let mut tops: Vec<Top> = vec![];
    for group in groups {
        if group.is_empty() {
            continue;
        }
        if !tops.is_empty() {
            tops.push(Top::Gap);
        }
        tops.extend(group);
    }
    let here = snap.here.as_str();
    let has_mr = snap.mrs.iter().any(|m| m.source == here);
    if !here.is_empty() && here != snap.default_branch && !has_mr {
        if !tops.is_empty() {
            tops.push(Top::Gap);
        }
        tops.push(Top::Here(here.to_string()));
    }
    tops
}

enum Top<'a> {
    Stack(u64),
    /// One of your tickets with no MR yet that waits on nothing on the board.
    Ticket(&'a Ticket),
    Label(String),
    Gap,
    Here(String),
}

/// What a row of the tree is of: an MR, or a ticket that has no MR yet.
#[derive(Clone, Copy)]
enum Node<'a> {
    Mr(u64),
    Ticket(&'a Ticket),
}

/// Where your tickets with no MR yet go: under the first of its blockers
/// drawn in the tree, an MR or another of these, else at the top of its
/// project. One that hasn't started shows only under something shown,
/// so a project's backlog stays off the board.
struct Waiting<'a> {
    under_mr: HashMap<u64, Vec<&'a Ticket>>,
    under_ticket: HashMap<String, Vec<&'a Ticket>>,
    top: Vec<&'a Ticket>,
}

impl<'a> Waiting<'a> {
    /// `tickets`, `has_mr` and `drawn` (the MR in the tree) by key.
    fn of(tickets: &HashMap<String, &'a Ticket>, has_mr: &HashSet<String>, drawn: &HashMap<String, u64>) -> Waiting<'a> {
        let mut bare: Vec<&'a Ticket> = tickets.iter().filter(|(k, _)| !has_mr.contains(*k)).map(|(_, t)| *t).collect();
        bare.sort_by_key(|t| made(&t.id));
        let bare_keys: HashSet<String> = bare.iter().filter_map(|t| key(&t.id)).collect();
        // What waits on each MR and each ticket, and what waits on nothing.
        let (mut on_mr, mut on_ticket, mut roots) = (BTreeMap::<u64, Vec<_>>::new(), HashMap::<String, Vec<_>>::new(), vec![]);
        for &t in &bare {
            let first = t.blocked_by.iter().filter_map(|b| key(b)).find(|b| drawn.contains_key(b) || bare_keys.contains(b));
            match first.map(|b| (drawn.get(&b).copied(), b)) {
                Some((Some(iid), _)) => on_mr.entry(iid).or_default().push(t),
                Some((None, b)) => on_ticket.entry(b).or_default().push(t),
                None => roots.push(t),
            }
        }
        let mut w = Waiting { under_mr: HashMap::new(), under_ticket: HashMap::new(), top: vec![] };
        let mut reached = HashSet::new();
        for (iid, ts) in on_mr {
            for t in ts {
                w.place(t, Some(Node::Mr(iid)), &on_ticket, &mut reached);
            }
        }
        // Linear lets tickets block each other in a ring, which nothing
        // above reaches; the first of it made is its root, as in stacks().
        for t in roots.into_iter().chain(bare) {
            w.place(t, None, &on_ticket, &mut reached);
        }
        w.top.sort_by_key(|t| made(&t.id));
        w
    }

    /// `t` under `up` when that is shown (None when it isn't, or `t` waits
    /// on nothing), else at the top if `t` has started; then what waits on it.
    fn place(
        &mut self,
        t: &'a Ticket,
        up: Option<Node<'a>>,
        on_ticket: &HashMap<String, Vec<&'a Ticket>>,
        reached: &mut HashSet<String>,
    ) {
        let Some(k) = key(&t.id).filter(|k| reached.insert(k.clone())) else {
            return;
        };
        let spot = match up {
            Some(Node::Mr(iid)) => Some(self.under_mr.entry(iid).or_default()),
            Some(Node::Ticket(u)) => Some(self.under_ticket.entry(u.id.clone()).or_default()),
            None if t.started => Some(&mut self.top),
            None => None,
        };
        let shown = spot.map(|list| list.push(t)).is_some();
        for &kid in on_ticket.get(&k).into_iter().flatten() {
            self.place(kid, shown.then_some(Node::Ticket(t)), on_ticket, reached);
        }
    }
}

/// A ticket's place in the order its team made them: "AP-99" before "AP-100".
fn made(id: &str) -> (&str, u64) {
    let (team, n) = id.rsplit_once('-').unwrap_or((id, ""));
    (team, n.parse().unwrap_or(0))
}

/// Turns stacks into rows, each with the rails that show where it hangs.
struct RowMaker<'a> {
    by_iid: &'a HashMap<u64, &'a Mr>,
    /// The key of the ticket each MR is for.
    keys: &'a HashMap<u64, String>,
    children: &'a HashMap<u64, Vec<u64>>,
    me: &'a str,
    rules: &'a Rules,
    /// Your tickets, by key.
    tickets: &'a HashMap<String, &'a Ticket>,
    waiting: &'a Waiting<'a>,
    workers: &'a Workers,
}

impl<'a> RowMaker<'a> {
    fn forest(&self, tops: &[Top<'a>]) -> Vec<Row> {
        let last = tops.iter().rposition(|t| !matches!(t, Top::Gap | Top::Label(_)));
        let mut rows = vec![];
        for (i, top) in tops.iter().enumerate() {
            let is_last = Some(i) == last;
            match top {
                Top::Stack(iid) => self.node(Node::Mr(*iid), "", is_last, &mut rows),
                Top::Ticket(t) => self.node(Node::Ticket(t), "", is_last, &mut rows),
                Top::Gap => rows.push(Row {
                    rails: "│".into(),
                    wrap_rails: "│".into(),
                    item: Item::Gap,
                }),
                Top::Label(name) => rows.push(Row {
                    rails: "│  ".into(),
                    wrap_rails: "│  ".into(),
                    item: Item::Label(name.clone()),
                }),
                Top::Here(b) => rows.push(Row {
                    rails: branch_rail(is_last),
                    wrap_rails: format!("{}  ", under_rail(is_last)),
                    item: Item::Here(b.clone()),
                }),
            }
        }
        rows
    }

    /// The node's row, then what hangs off it: the MRs stacked on an MR,
    /// then the tickets waiting on it.
    fn node(&self, n: Node<'a>, prefix: &str, last: bool, rows: &mut Vec<Row>) {
        let (item, mut kids, waiting) = match n {
            Node::Mr(iid) => {
                let m = self.by_iid[&iid];
                let k = self.keys.get(&iid);
                let item = Item::Mr {
                    iid,
                    branch: if m.author == self.me { self.short(&m.source) } else { m.source.clone() },
                    mv: move_of(m, self.me, self.rules),
                    ticket: k.and_then(|k| self.tickets.get(k)).map(|t| t.id.clone()),
                    agent: self.workers.on_mr(&m.source, k),
                };
                let kids: Vec<Node> = self.children.get(&iid).into_iter().flatten().map(|&c| Node::Mr(c)).collect();
                (item, kids, self.waiting.under_mr.get(&iid))
            }
            Node::Ticket(t) => {
                let k = key(&t.id);
                let item = Item::Ticket {
                    id: t.id.clone(),
                    title: t.title.clone(),
                    state: t.state.clone(),
                    started: t.started,
                    waits_on: t.blocked_by.first().cloned(),
                    agent: k.and_then(|k| self.workers.on_ticket(&k)),
                };
                (item, vec![], self.waiting.under_ticket.get(&t.id))
            }
        };
        kids.extend(waiting.into_iter().flatten().map(|t| Node::Ticket(t)));
        let under = format!("{prefix}{}", under_rail(last));
        rows.push(Row {
            rails: format!("{prefix}{}", branch_rail(last)),
            wrap_rails: format!("{under}{}", if kids.is_empty() { "  " } else { "│ " }),
            item,
        });
        let n = kids.len();
        for (i, k) in kids.into_iter().enumerate() {
            self.node(k, &under, i + 1 == n, rows);
        }
    }

    /// A branch of yours without your prefix.
    fn short(&self, branch: &str) -> String {
        self.rules
            .prefixes
            .iter()
            .find_map(|p| branch.strip_prefix(p.as_str()))
            .unwrap_or(branch)
            .to_string()
    }
}

fn branch_rail(last: bool) -> String {
    if last { "└─ " } else { "├─ " }.into()
}

/// What runs under a branch: the rail on to its next sibling, if it has one.
fn under_rail(last: bool) -> &'static str {
    if last {
        "   "
    } else {
        "│  "
    }
}

/// What the panel under the board says of one MR.
#[derive(Clone, Debug, PartialEq)]
pub struct Panel {
    pub title: String,
    pub mv: Move,
    /// One fact each, in reading order.
    pub facts: Vec<String>,
}

/// The panel of `mr`, which `agent` may be at work on. While one is, the
/// panel doesn't say nobody was asked to review: it isn't ready to ask.
pub fn panel(mr: &Mr, snap: &Snapshot, rules: &Rules, agent: Option<&Worker>) -> Panel {
    let me = snap.me.as_str();
    let mine = mr.author == me;
    let mut facts = vec![];
    if mr.reach.is_none() && !mr.merge_error.is_empty() {
        facts.push(format!("GitLab's last merge failed: {}", mr.merge_error));
    }
    if mine {
        if let Some(age) = mr.split_age.filter(|a| *a >= DAY) {
            let d = (age / DAY) as u64;
            facts.push(format!(
                "split from {} {d} {} ago",
                mr.target,
                plural(d, "day", "days")
            ));
        }
    }
    match mr.pipeline.as_str() {
        "" => {}
        "success" => facts.push("pipeline passed".into()),
        "failed" if mr.failed_jobs.is_empty() => facts.push("pipeline failed".into()),
        "failed" => facts.push(format!("{} failed", mr.failed_jobs.join(", "))),
        p if running(p) => facts.push("pipeline running".into()),
        p => facts.push(format!("pipeline {}", p.replace('_', " "))),
    }
    if mr.auto_merge {
        facts.push("merges itself".into());
    }
    if mr.open_threads > 0 {
        facts.push(format!(
            "{} open {}",
            mr.open_threads,
            plural(mr.open_threads as u64, "thread", "threads")
        ));
    }
    if mine {
        let asked = others(&mr.reviewers, me);
        let approved = others(&mr.approved_by, me);
        let waiting: Vec<&str> = asked
            .iter()
            .copied()
            .filter(|r| !approved.contains(r))
            .collect();
        let working = agent.is_some_and(|w| w.state == State::Busy);
        if asked.is_empty() && approved.is_empty() && !working {
            facts.push("nobody asked to review".into());
        }
        if !approved.is_empty() {
            facts.push(format!("approved by {}", approved.join(", ")));
        }
        if !waiting.is_empty() {
            facts.push(format!("waiting on {}", waiting.join(", ")));
        }
    } else {
        facts.push(format!("by {}", mr.author));
    }
    match snap.parent(mr) {
        Some(p) => facts.push(format!("on {}", bang(p.iid))),
        None if mr.target != snap.default_branch => facts.push(format!("onto {}", mr.target)),
        None => {}
    }
    facts.push(format!(
        "{} in {} {}",
        size(mr.adds, mr.dels),
        with_commas(mr.files),
        plural(mr.files, "file", "files")
    ));
    Panel {
        title: plain_title(&mr.title),
        mv: move_of(mr, me, rules),
        facts,
    }
}

/// The title without GitLab's draft marker, which the move already says.
fn plain_title(t: &str) -> String {
    for p in ["Draft:", "[Draft]", "(Draft)", "WIP:"] {
        if t.len() >= p.len() && t[..p.len()].eq_ignore_ascii_case(p) {
            return t[p.len()..].trim_start().to_string();
        }
    }
    t.to_string()
}

fn plural<'a>(n: u64, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 {
        one
    } else {
        many
    }
}

/// What a board key does to an MR: its writes, in order; the word its row
/// says while they run; and the question asked first, if it asks one.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub writes: Vec<Write>,
    pub doing: &'static str,
    pub question: Option<String>,
}

/// None for a key that does nothing to this MR. Those that can't be taken
/// back with a key (merge, rebase, close, ready) ask first.
pub fn plan(act: Act, mr: &Mr) -> Option<Plan> {
    let iid = mr.iid;
    let (writes, doing, question) = match act {
        // Already set to, it merges itself: nothing to ask.
        Act::Merge if mr.auto_merge => return None,
        // While GitLab still waits on something, m sets it to merge once
        // that's done; the question says what.
        Act::Merge => {
            let head = mr.head.clone();
            let waits: Vec<&str> = [(mr.draft, "it's marked ready"), (running(&mr.pipeline), "its pipeline passes"), (!mr.approved, "it's approved")]
                .into_iter()
                .filter_map(|(waits, what)| waits.then_some(what))
                .collect();
            if waits.is_empty() {
                (
                    vec![Write::Merge { iid, head }],
                    "merging",
                    Some(format!("merge {} into {}?", bang(iid), mr.target)),
                )
            } else {
                (
                    vec![Write::AutoMerge { iid, head }],
                    "setting auto-merge",
                    Some(format!("merge {} into {} once {}?", bang(iid), mr.target, waits.join(" and "))),
                )
            }
        }
        Act::Close => (
            vec![Write::Close { iid }],
            "closing",
            Some(format!("close {}?", bang(iid))),
        ),
        // Never back to draft: out of it, and a pipeline, always together.
        Act::Ready if mr.draft => (
            vec![Write::Ready { iid }, Write::Pipeline { iid }],
            "marking ready",
            Some(format!("mark {} ready and start a pipeline?", bang(iid))),
        ),
        Act::Pipeline => (vec![Write::Pipeline { iid }], "starting a pipeline", None),
        Act::Retry if mr.pipeline_id > 0 => (
            vec![Write::Retry {
                pipeline: mr.pipeline_id,
            }],
            "retrying",
            None,
        ),
        _ => return None,
    };
    Some(Plan {
        writes,
        doing,
        question,
    })
}

/// The reviewers you typed, as a plan.
pub fn reviewers(mr: &Mr, typed: &str) -> Plan {
    Plan {
        writes: vec![Write::Reviewers { iid: mr.iid, names: names(typed) }],
        doing: "asking reviewers",
        question: None,
    }
}

/// The names in a list typed or written with commas, spaces or both.
fn names(list: &str) -> Vec<String> {
    list.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|n| !n.is_empty())
        .map(String::from)
        .collect()
}

/// Who a description's cc line names ("cc: kim@team.example alex@team.example").
pub fn ccs(description: &str) -> Vec<String> {
    description.lines().filter_map(cc_of).last().map(names).unwrap_or_default()
}

/// What a line of a description names, if it is the cc line.
fn cc_of(line: &str) -> Option<&str> {
    let l = line.trim_start();
    l.get(..3).filter(|s| s.eq_ignore_ascii_case("cc:")).map(|_| &l[3..])
}

/// The description, as read, with its cc line naming who you typed: in
/// place of the one there, else at the end; none when you typed no one.
pub fn cc(mr: &Mr, description: &str, typed: &str) -> Plan {
    let typed = names(typed);
    let line = (!typed.is_empty()).then(|| format!("cc: {}", typed.join(" ")));
    let mut lines: Vec<&str> = description.trim_end().split('\n').collect();
    match (lines.iter().rposition(|l| cc_of(l).is_some()), &line) {
        (Some(i), Some(l)) => lines[i] = l,
        (Some(i), None) => drop(lines.remove(i)),
        (None, Some(l)) => lines.extend(["", l]),
        (None, None) => {}
    }
    Plan {
        writes: vec![Write::Description { iid: mr.iid, text: lines.join("\n").trim().to_string() }],
        doing: "setting cc",
        question: None,
    }
}

/// Approving, with `note` said on the MR first when there is one.
pub fn approve(mr: &Mr, note: &str) -> Plan {
    let mut writes = vec![];
    if !note.trim().is_empty() {
        writes.push(Write::Comment {
            iid: mr.iid,
            at: At::Mr,
            body: note.trim().to_string(),
        });
    }
    writes.push(Write::Approve {
        iid: mr.iid,
        head: mr.head.clone(),
    });
    Plan {
        writes,
        doing: "approving",
        question: None,
    }
}

/// Who could review, but you: the project's `members`, and everyone the
/// board has seen, which brings in the bots that review, like a review bot.
pub fn people(snap: &Snapshot, members: &[String]) -> Vec<String> {
    let mut all: Vec<String> = snap
        .mrs
        .iter()
        .flat_map(|m| {
            std::iter::once(&m.author)
                .chain(&m.reviewers)
                .chain(&m.approved_by)
        })
        .chain(members)
        .filter(|n| **n != snap.me)
        .cloned()
        .collect();
    all.sort();
    all.dedup();
    all
}

/// `typed` split before the name being typed, and that name.
pub fn split_at_typing(typed: &str) -> (&str, &str) {
    typed.split_at(typed.rfind([',', ' ']).map_or(0, |i| i + 1))
}

/// The people whose names start with the one being typed.
pub fn matches<'a>(typed: &str, people: &'a [String]) -> Vec<&'a str> {
    let part = split_at_typing(typed).1.trim().to_lowercase();
    people
        .iter()
        .filter(|p| p.to_lowercase().starts_with(&part))
        .map(String::as_str)
        .collect()
}

/// The name being typed, completed to the `nth` (cycling) of those that fit.
pub fn complete(typed: &str, people: &[String], nth: usize) -> String {
    let done = split_at_typing(typed).0;
    let fits = matches(typed, people);
    match fits.get(nth % fits.len().max(1)) {
        Some(p) => format!("{done}{}{p}", if done.ends_with(',') { " " } else { "" }),
        None => typed.to_string(),
    }
}

/// 1354 as "1,354".
pub fn with_commas(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// An MR's number as ot shows it, "!12_345", grouped so long ones read at a
/// glance. GitLab only links the plain "!12345", so text it reads keeps that.
pub fn bang(iid: u64) -> String {
    format!("!{}", with_commas(iid).replace(',', "_"))
}

/// "+1,354 −514", with a real minus.
pub fn size(adds: u64, dels: u64) -> String {
    format!("+{} \u{2212}{}", with_commas(adds), with_commas(dels))
}

#[cfg(test)]
pub mod fixture {
    //! A made-up team: sam is you, alex and kim ask for reviews.
    use super::*;
    use crate::claude::{Agent, Turn, Who};

    pub const NOW: i64 = 1_800_000_000;

    pub fn rules() -> Rules {
        Rules {
            prefixes: vec!["sam/".into(), "old/".into()],
            stale_job: Some("stale-check".into()),
            stale_days: Some(4),
            deploy_job: None,
        }
    }

    pub fn mr(iid: u64, author: &str, source: &str, target: &str) -> Mr {
        Mr {
            iid,
            title: format!("Change {iid}"),
            author: author.into(),
            source: source.into(),
            target: target.into(),
            url: format!("https://git.example/team/app/-/merge_requests/{iid}"),
            updated: NOW - DAY,
            reviewers: vec!["kim".into()],
            pipeline: "success".into(),
            adds: 12,
            dels: 3,
            files: 2,
            ..Default::default()
        }
    }

    fn turn(who: Who, text: &str, at: i64) -> Turn {
        Turn {
            who,
            text: text.into(),
            at,
            times: 1,
            agent: None,
        }
    }

    pub fn sessions() -> Vec<Session> {
        vec![
            Session {
                id: "s1".into(),
                title: "A fresh board".into(),
                branch: "sam/cache-keys".into(),
                cwd: "/home/sam/app".into(),
                pid: None,
                works_on: None,
                state: State::Busy,
                at: NOW - 90,
                started: NOW - 2 * 3600,
                turns: vec![
                    turn(Who::You, "make the board fresh", NOW - 2 * 3600),
                    turn(Who::Tool, "reading src/main.rs", NOW - 2 * 3600 + 20),
                    turn(
                        Who::Claude,
                        "The fetcher waits on the UI thread. Moving it.",
                        NOW - 2 * 3600 + 60,
                    ),
                    turn(Who::Tool, "editing src/main.rs", NOW - 200),
                    Turn {
                        agent: Some(Agent {
                            tool_id: "toolu_1".into(),
                            turns: vec![
                                turn(Who::You, "Review the diff for design complexity.", NOW - 180),
                                turn(Who::Tool, "reading src/main.rs", NOW - 170),
                                turn(Who::Claude, "The fetcher is fine. One note on the writer.", NOW - 90),
                            ],
                            ..Default::default()
                        }),
                        ..turn(Who::Tool, "an agent: Review the fetcher", NOW - 180)
                    },
                ],
                cost: 1.25,
            },
            Session {
                id: "s2".into(),
                title: "Why the parser's error paths read better apart".into(),
                branch: "sam/parser-errors".into(),
                cwd: "/home/sam/app".into(),
                pid: None,
                works_on: None,
                state: State::Idle,
                at: NOW - 25 * 60,
                started: NOW - 3 * 3600,
                turns: vec![
                    turn(Who::You, "Split the parser's error paths from its happy path. Each should read alone, and the tests should not care which is which.", NOW - 3 * 3600),
                    turn(Who::Tool, "reading src/parser.rs", NOW - 3 * 3600 + 5),
                    turn(Who::Tool, "searching for ParseError", NOW - 3 * 3600 + 9),
                    turn(Who::Claude, "Three error paths share one match arm with the happy path. I'll move them to errors.rs first.\n\nThe tests stay where they are.", NOW - 3 * 3600 + 40),
                    turn(Who::Tool, "editing src/errors.rs", NOW - 3 * 3600 + 90),
                    Turn {
                        times: 3,
                        ..turn(Who::Tool, "editing src/parser.rs", NOW - 3 * 3600 + 120)
                    },
                    turn(Who::Tool, "Run the tests", NOW - 3 * 3600 + 130),
                    turn(Who::Claude, "The errors module is done; the tests pass. Want the happy path next?", NOW - 25 * 60),
                ],
                cost: 0.4,
            },
        ]
    }

    pub fn snapshot() -> Snapshot {
        let mut mrs = vec![];
        let mut m = mr(101, "sam", "sam/parser-cleanup", "main");
        m.title =
            "Draft: Split the parser's error paths from its happy path so each reads alone".into();
        m.draft = false;
        m.pipeline = "failed".into();
        m.failed_jobs = vec!["stale-check".into()];
        m.split_age = Some(6 * DAY);
        m.reviewers = vec![];
        m.adds = 1083;
        m.dels = 72;
        m.files = 12;
        mrs.push(m);
        let mut m = mr(102, "sam", "sam/parser-errors", "sam/parser-cleanup");
        m.reviewers = vec![];
        mrs.push(m);
        let mut m = mr(103, "sam", "sam/parser-ab", "sam/parser-errors");
        m.draft = true;
        mrs.push(m);
        let mut m = mr(104, "sam", "sam/cache-keys", "main");
        m.pipeline = "failed".into();
        m.failed_jobs = vec!["unit-tests".into(), "lint".into()];
        mrs.push(m);
        let mut m = mr(105, "sam", "sam/retry-budget", "main");
        m.approved = true;
        m.mergeable = true;
        m.approved_by = vec!["kim".into()];
        mrs.push(m);
        let mut m = mr(
            106,
            "sam",
            "sam/a-branch-name-long-enough-that-it-has-to-wrap-under-its-own-column",
            "main",
        );
        m.open_threads = 3;
        mrs.push(m);
        let mut m = mr(107, "sam", "sam/warm-cache", "main");
        m.pipeline = "running".into();
        mrs.push(m);
        mrs.push(mr(108, "sam", "old/quiet-but-sent", "main"));
        // Approved, so mergeable once green, and already set to be.
        let mut m = mr(109, "sam", "sam/lands-itself", "main");
        m.pipeline = "running".into();
        m.auto_merge = true;
        m.approved = true;
        m.mergeable = true;
        mrs.push(m);
        mrs.push(mr(201, "alex", "alex/route-table", "main"));
        let mut m = mr(202, "kim", "kim/flag-cleanup", "main");
        m.approved_by = vec!["sam".into()];
        m.reviewers = vec!["sam".into()];
        mrs.push(m);
        mrs.push(mr(203, "kim", "kim/flag-cleanup-two", "kim/flag-cleanup"));
        // Approved by you and nothing under it waiting: off the board.
        let mut m = mr(204, "kim", "kim/done-and-dusted", "main");
        m.approved_by = vec!["sam".into()];
        mrs.push(m);
        for (iid, source, target) in [
            (110, "sam/old-idea", "main"),
            (111, "sam/old-idea-two", "sam/old-idea"),
            (112, "sam/forgotten", "main"),
        ] {
            let mut m = mr(iid, "sam", source, target);
            m.reviewers = vec!["sam".into()];
            m.updated = NOW - 59 * DAY;
            mrs.push(m);
        }
        Snapshot {
            me: "sam".into(),
            default_branch: "main".into(),
            as_of: NOW - 60,
            mrs,
            here: "sam/scratch".into(),
        }
    }

    /// A ticket swarm on top of snapshot(): AP-1's MR !301 with AP-2's
    /// stacked on it, AP-3 waiting on AP-2 and AP-6 on AP-3 with no MR
    /// yet, AP-4 started without one, and AP-5 in the backlog.
    pub fn swarm() -> (Snapshot, Vec<Ticket>, Vec<Session>) {
        let mut snap = snapshot();
        snap.mrs.push(mr(301, "sam", "sam/ap-1-grammar-file", "main"));
        snap.mrs.push(mr(302, "sam", "sam/ap-2-recovery", "sam/ap-1-grammar-file"));
        let ticket = |n: u32, title: &str, started: bool, blocked_by: &[&str]| Ticket {
            id: format!("AP-{n}"),
            title: title.into(),
            url: format!("https://linear.example/AP-{n}"),
            state: if started { "In Progress" } else { "Todo" }.into(),
            started,
            project: "Parser migration".into(),
            blocked_by: blocked_by.iter().map(|b| b.to_string()).collect(),
            ..Default::default()
        };
        let mut tickets = vec![
            ticket(1, "Read the grammar from one file", true, &[]),
            ticket(2, "Error recovery in the parser", true, &["AP-1"]),
            ticket(3, "Parser lookup tables", false, &["AP-2"]),
            ticket(4, "Line numbers in parse errors", true, &[]),
            ticket(5, "Backfill line numbers", false, &[]),
            ticket(6, "Alert on parser slowdowns", false, &["AP-3"]),
            Ticket { has_mr: true, ..ticket(7, "Already live", true, &[]) },
        ];
        tickets[1].comment = "Pushed b927a3d: recovery skips to the next statement.\n\nCheck: types pass.".into();
        tickets[1].commented = NOW - 41 * 60;
        let agent = |name: &str, waiting: bool, doing: &str| Turn {
            agent: Some(Agent {
                name: name.into(),
                waiting,
                turns: vec![turn(Who::You, "Build it.", NOW - 3600), turn(Who::Tool, doing, NOW - 6 * 60)],
                ..Default::default()
            }),
            ..turn(Who::Tool, &format!("an agent: Build {name}"), NOW - 3600)
        };
        let lead = Session {
            id: "s3".into(),
            title: "Parser migration agents".into(),
            cwd: "/home/sam/app".into(),
            state: State::Idle,
            at: NOW - 120,
            started: NOW - 4 * 3600,
            turns: vec![agent("ap-2", false, "editing src/recover.rs"), agent("ap-4", true, "Run the tests")],
            ..Default::default()
        };
        (snap, tickets, vec![lead])
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;

    #[test]
    fn a_stack_stays_whole_until_its_merged_ones_are_live() {
        let mut snap = Snapshot {
            me: "sam".into(),
            default_branch: "main".into(),
            mrs: vec![mr(1, "sam", "sam/a", "main"), mr(2, "sam", "sam/b", "sam/a"), mr(3, "sam", "sam/c", "sam/b")],
            ..Default::default()
        };
        let rows = |snap: &Snapshot, was: &HashMap<u64, u64>| -> Vec<String> {
            board(snap, &rules(), was, &[], &[], &[], NOW)
                .tree
                .iter()
                .filter_map(|r| match &r.item {
                    Item::Mr { iid, .. } => Some(format!("{}{iid}", r.rails)),
                    _ => None,
                })
                .collect()
        };
        let was = remember(&HashMap::new(), &snap);
        assert_eq!(was, HashMap::from([(2, 1), (3, 2)]));
        // !1 merges, and GitLab moves !2 onto main.
        snap.mrs[0].reach = Some(Reach::Merged);
        snap.mrs[1].target = "main".into();
        let was = remember(&was, &snap);
        assert_eq!(rows(&snap, &was), ["└─ 1", "   └─ 2", "      └─ 3"]);
        // !1 is live, so off the board, and !2 is a stack of its own.
        snap.mrs.remove(0);
        let was = remember(&was, &snap);
        assert_eq!(was, HashMap::from([(3, 2)]));
        assert_eq!(rows(&snap, &was), ["└─ 2", "   └─ 3"]);
        // !3 moved onto main by hand while !2 is open: drawn where GitLab has it.
        snap.mrs[1].target = "main".into();
        assert_eq!(remember(&was, &snap), HashMap::new());
    }

    fn moves(snap: &Snapshot) -> Vec<(u64, String)> {
        let mut v: Vec<(u64, String)> = snap
            .mrs
            .iter()
            .map(|m| (m.iid, move_of(m, &snap.me, &rules()).word()))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn each_row_gets_the_first_move_that_applies() {
        let got = moves(&snapshot());
        let want = [
            (101, "rebase it"),
            (102, "ask for review"),
            (103, "draft"),
            (104, "pipeline broke"),
            (105, "merge it"),
            (106, "answer 3 threads"),
            (107, "running"),
            (108, "in review"),
            (109, "merges itself"),
            (110, "ask for review"),
            (111, "ask for review"),
            (112, "ask for review"),
            (201, "review it"),
            (202, "you approved"),
            (203, "review it"),
            (204, "you approved"),
        ];
        assert_eq!(got, want.map(|(i, w)| (i, w.to_string())));
    }

    #[test]
    fn a_merged_mr_says_how_far_it_has_got() {
        let mut m = mr(1, "sam", "sam/x", "main");
        m.reach = Some(Reach::Merged);
        assert_eq!(move_of(&m, "sam", &rules()).word(), "merged");
        m.reach = Some(Reach::Deploying);
        assert_eq!(move_of(&m, "sam", &rules()).word(), "deploying");
    }

    #[test]
    fn each_mr_on_one_that_merged_is_rebased_on_its_own_past_it() {
        // !1 with two on it, !2 and !3, and !4 on !3.
        let mut snap = Snapshot {
            me: "sam".into(),
            default_branch: "main".into(),
            mrs: vec![
                mr(1, "sam", "sam/base", "main"),
                mr(2, "sam", "sam/left", "sam/base"),
                mr(3, "sam", "sam/right", "sam/base"),
                mr(4, "sam", "sam/top", "sam/right"),
            ],
            ..Default::default()
        };
        let of = |snap: &Snapshot, was: &HashMap<u64, u64>, iid| {
            let (layers, merged) = layers(snap, was, iid);
            (layers.iter().map(|m| m.iid).collect::<Vec<_>>(), merged)
        };
        let was = remember(&HashMap::new(), &snap);
        assert_eq!(of(&snap, &was, 4), (vec![1, 2, 3, 4], None), "all of it, while it is all open");
        // !1 merges, and GitLab moves !2 and !3 onto main.
        snap.mrs[0].reach = Some(Reach::Merged);
        snap.mrs[1].target = "main".into();
        snap.mrs[2].target = "main".into();
        let was = remember(&was, &snap);
        assert_eq!(of(&snap, &was, 4), (vec![3, 4], Some(1)));
        assert_eq!(of(&snap, &was, 2), (vec![2], Some(1)));
        assert_eq!(of(&snap, &was, 1), (vec![], None), "a merged one has nothing to rebase");
        // ot never saw !3 on !1, but git says it still has !1's commits.
        snap.mrs[2].carries = Some(1);
        assert_eq!(of(&snap, &HashMap::new(), 4), (vec![3, 4], Some(1)));
    }

    #[test]
    fn a_stale_job_failing_alongside_another_is_a_break() {
        let mut m = mr(1, "sam", "sam/x", "main");
        m.pipeline = "failed".into();
        m.failed_jobs = vec!["stale-check".into(), "unit-tests".into()];
        assert_eq!(move_of(&m, "sam", &rules()), Move::Broke);
    }

    #[test]
    fn an_old_base_is_a_rebase_even_before_ci_says_so() {
        let mut m = mr(1, "sam", "sam/x", "main");
        m.split_age = Some(5 * DAY);
        assert_eq!(move_of(&m, "sam", &rules()), Move::Rebase);
        // CI counts whole days, so four and a half is still fine.
        m.split_age = Some(4 * DAY + DAY / 2);
        assert_ne!(move_of(&m, "sam", &rules()), Move::Rebase);
    }

    #[test]
    fn without_a_configured_stale_job_its_failure_is_a_break() {
        let mut m = mr(1, "sam", "sam/x", "main");
        m.pipeline = "failed".into();
        m.failed_jobs = vec!["stale-check".into()];
        assert_eq!(move_of(&m, "sam", &Rules::default()), Move::Broke);
    }

    #[test]
    fn asking_only_yourself_is_not_asking_for_review() {
        let mut m = mr(1, "sam", "sam/x", "main");
        m.reviewers = vec!["sam".into()];
        assert_eq!(move_of(&m, "sam", &rules()), Move::AskReview);
    }

    fn lines(rows: &[Row]) -> Vec<String> {
        let agent = |a: &Option<Worker>| a.as_ref().map_or(String::new(), |w| format!(" [{} {}]", w.name, w.state.word()));
        rows.iter()
            .map(|r| match &r.item {
                Item::Mr { iid, branch, mv, ticket, agent: a } => {
                    let ticket = ticket.as_ref().map_or(String::new(), |t| format!(" {t}"));
                    format!("{}{} {branch} {} !{iid}{ticket}{}", r.rails, mv.glyph(), mv.word(), agent(a))
                }
                Item::Ticket { id, title, state, waits_on, agent: a, .. } => {
                    let waits = waits_on.as_ref().map_or(String::new(), |w| format!(" waits on {w}"));
                    format!("{}{title} {id} {state}{waits}{}", r.rails, agent(a))
                }
                Item::Label(name) => format!("{}{name}", r.rails),
                Item::Gap => r.rails.clone(),
                Item::Here(b) => format!("{}◆ {b}", r.rails),
                Item::Claude {
                    title,
                    state,
                    doing,
                    ago,
                    ..
                } => format!("{} {title} {} {ago} / {doing}", state.glyph(), state.word()),
                Item::Hc { name, status, mr, .. } => {
                    let mr = mr.map_or(String::new(), |i| format!(" !{i}"));
                    format!("{name} {status:?}{mr}")
                }
            })
            .collect()
    }

    #[test]
    fn claudes_sessions_are_rows_of_their_own() {
        let b = board(&snapshot(), &rules(), &HashMap::new(), &sessions(), &[], &[], NOW);
        assert_eq!(
            lines(&b.claude),
            [
                "◐ A fresh board working 1m / The fetcher is fine. One note on the writer.",
                "○ Why the parser's error paths read better apart waiting on you 25m / The errors module is done; the tests pass. Want the happy path next?",
            ]
        );
    }

    #[test]
    fn the_board_is_yours_then_others_then_where_you_are() {
        let b = board(&snapshot(), &rules(), &HashMap::new(), &[], &[], &[], NOW);
        assert_eq!(
            lines(&b.tree),
            [
                "├─ ● parser-cleanup rebase it !101",
                "│  └─ ● parser-errors ask for review !102",
                "│     └─ · parser-ab draft !103",
                "├─ ● cache-keys pipeline broke !104",
                "├─ ● retry-budget merge it !105",
                "├─ ● a-branch-name-long-enough-that-it-has-to-wrap-under-its-own-column answer 3 threads !106",
                "├─ ◐ warm-cache running !107",
                "├─ ○ quiet-but-sent in review !108",
                "├─ ◐ lands-itself merges itself !109",
                "│",
                "├─ ● alex/route-table review it !201",
                "├─ ✓ kim/flag-cleanup you approved !202",
                "│  └─ ● kim/flag-cleanup-two review it !203",
                "│",
                "└─ ◆ sam/scratch",
            ]
        );
        assert_eq!(
            lines(&b.not_sent),
            [
                "├─ ● old-idea ask for review !110",
                "│  └─ ● old-idea-two ask for review !111",
                "└─ ● forgotten ask for review !112"
            ]
        );
    }

    #[test]
    fn a_wrapped_line_continues_the_rails_under_it() {
        let b = board(&snapshot(), &rules(), &HashMap::new(), &[], &[], &[], NOW);
        let parent = &b.tree[0];
        assert_eq!(
            (parent.rails.as_str(), parent.wrap_rails.as_str()),
            ("├─ ", "│  │ ")
        );
        let leaf = &b.tree[2];
        assert_eq!(
            (leaf.rails.as_str(), leaf.wrap_rails.as_str()),
            ("│     └─ ", "│          ")
        );
    }

    #[test]
    fn a_quiet_stack_stays_up_while_any_of_it_is_sent() {
        let mut snap = snapshot();
        snap.mrs
            .iter_mut()
            .find(|m| m.iid == 111)
            .unwrap()
            .reviewers = vec!["kim".into()];
        let b = board(&snap, &rules(), &HashMap::new(), &[], &[], &[], NOW);
        assert_eq!(lines(&b.not_sent), ["└─ ● forgotten ask for review !112"]);
        assert!(lines(&b.tree).iter().any(|l| l.contains("!110")));
    }

    #[test]
    fn no_here_row_on_the_default_branch_or_a_listed_one() {
        let mut snap = snapshot();
        for here in ["main", "sam/cache-keys", ""] {
            snap.here = here.into();
            let b = board(&snap, &rules(), &HashMap::new(), &[], &[], &[], NOW);
            assert!(
                !b.tree.iter().any(|r| matches!(r.item, Item::Here(_))),
                "{here}"
            );
        }
    }

    #[test]
    fn two_mrs_targeting_each_other_still_show() {
        let snap = Snapshot {
            me: "sam".into(),
            mrs: vec![mr(1, "sam", "a", "b"), mr(2, "sam", "b", "a")],
            ..Default::default()
        };
        let b = board(&snap, &Rules::default(), &HashMap::new(), &[], &[], &[], NOW);
        assert_eq!(
            lines(&b.tree),
            ["└─ ○ a in review !1", "   └─ ○ b in review !2"]
        );
    }

    #[test]
    fn the_tree_and_the_panel_agree_on_what_an_mr_sits_on() {
        let mut snap = snapshot();
        // GitLab allows two open MRs from one branch when their targets differ.
        snap.mrs
            .push(mr(230, "alex", "alex/route-table", "release"));
        snap.mrs
            .push(mr(240, "kim", "kim/routes-on-top", "alex/route-table"));
        let b = board(&snap, &rules(), &HashMap::new(), &[], &[], &[], NOW);
        let under = lines(&b.tree);
        let at = under.iter().position(|l| l.contains("!240")).unwrap();
        assert!(under[at - 1].contains("!201"), "{under:#?}");
        assert!(panel(snap.mr(240).unwrap(), &snap, &rules(), None)
            .facts
            .contains(&"on !201".to_string()));
    }

    #[test]
    fn the_panel_leads_with_why_gitlab_could_not_merge_it() {
        let mut snap = snapshot();
        snap.mrs.iter_mut().find(|m| m.iid == 105).unwrap().merge_error = "Merge failed: another merge is in progress".into();
        let p = panel(snap.mr(105).unwrap(), &snap, &rules(), None);
        assert_eq!(p.facts[0], "GitLab's last merge failed: Merge failed: another merge is in progress");
    }

    #[test]
    fn the_panel_lists_one_fact_each() {
        let snap = snapshot();
        let p = panel(snap.mr(101).unwrap(), &snap, &rules(), None);
        assert_eq!(
            p.title,
            "Split the parser's error paths from its happy path so each reads alone"
        );
        assert_eq!(p.mv, Move::Rebase);
        assert_eq!(
            p.facts,
            [
                "split from main 6 days ago",
                "stale-check failed",
                "nobody asked to review",
                "+1,083 \u{2212}72 in 12 files"
            ]
        );
        let p = panel(snap.mr(203).unwrap(), &snap, &rules(), None);
        assert_eq!(
            p.facts,
            [
                "pipeline passed",
                "by kim",
                "on !202",
                "+12 \u{2212}3 in 2 files"
            ]
        );
        let p = panel(snap.mr(105).unwrap(), &snap, &rules(), None);
        assert_eq!(
            p.facts,
            [
                "pipeline passed",
                "approved by kim",
                "+12 \u{2212}3 in 2 files"
            ]
        );
    }

    #[test]
    fn keys_plan_their_writes_and_the_lasting_ones_ask() {
        let snap = snapshot();
        let p = plan(Act::Merge, snap.mr(105).unwrap()).unwrap();
        assert_eq!(
            p.writes,
            [Write::Merge {
                iid: 105,
                head: String::new()
            }]
        );
        assert_eq!(p.question.as_deref(), Some("merge !105 into main?"));
        let p = plan(Act::Merge, snap.mr(107).unwrap()).unwrap();
        assert_eq!(
            p.writes,
            [Write::AutoMerge {
                iid: 107,
                head: String::new()
            }],
            "m on a running pipeline sets auto-merge"
        );
        assert_eq!(
            p.question.as_deref(),
            Some("merge !107 into main once its pipeline passes and it's approved?")
        );
        let p = plan(Act::Merge, snap.mr(108).unwrap()).unwrap();
        assert!(matches!(p.writes[..], [Write::AutoMerge { iid: 108, .. }]), "and on one still in review");
        assert_eq!(p.question.as_deref(), Some("merge !108 into main once it's approved?"));
        let p = plan(Act::Ready, snap.mr(103).unwrap()).unwrap();
        assert_eq!(
            p.writes,
            [Write::Ready { iid: 103 }, Write::Pipeline { iid: 103 }],
            "ready and a pipeline, always both"
        );
        assert!(
            plan(Act::Ready, snap.mr(105).unwrap()).is_none(),
            "R never puts one back into draft"
        );
        assert!(
            plan(Act::Merge, snap.mr(109).unwrap()).is_none(),
            "m leaves one that merges itself alone"
        );
        assert!(
            plan(Act::Retry, snap.mr(104).unwrap()).is_none(),
            "nothing to retry without a pipeline's id"
        );
        assert!(
            plan(Act::Approve, snap.mr(201).unwrap()).is_none(),
            "A asks how to approve instead of planning on its own"
        );
        let p = approve(snap.mr(201).unwrap(), " nice ");
        assert_eq!(
            p.writes,
            [
                Write::Comment {
                    iid: 201,
                    at: At::Mr,
                    body: "nice".into()
                },
                Write::Approve {
                    iid: 201,
                    head: String::new()
                }
            ]
        );
        assert_eq!(approve(snap.mr(201).unwrap(), "  ").writes.len(), 1);
    }

    #[test]
    fn reviewer_names_complete_from_the_project_and_the_board() {
        let all = people(&snapshot(), &["zoe".into(), "kim".into(), "sam".into()]);
        assert_eq!(all, ["alex", "kim", "zoe"]);
        assert_eq!(complete("k", &all, 0), "kim");
        assert_eq!(complete("kim, a", &all, 0), "kim, alex");
        assert_eq!(complete("", &all, 1), "kim");
        assert_eq!(complete("zed", &all, 0), "zed");
        assert_eq!(
            reviewers(snapshot().mr(105).unwrap(), "kim, alex ").writes,
            [Write::Reviewers {
                iid: 105,
                names: vec!["kim".into(), "alex".into()]
            }]
        );
    }

    #[test]
    fn the_cc_line_is_read_and_replaced_added_or_dropped() {
        assert_eq!(ccs("Fixes it.\r\n\r\nCC: kim@team.example, alex@team.example\r\n"), ["kim@team.example", "alex@team.example"]);
        assert!(ccs("Fixes it.\n\nThe ccs of a description.").is_empty());
        let mr = snapshot().mr(105).cloned().unwrap();
        let text = |description: &str, typed: &str| match &cc(&mr, description, typed).writes[..] {
            [Write::Description { iid: 105, text }] => text.clone(),
            w => panic!("{w:?}"),
        };
        assert_eq!(text("Fixes it.\n", "kim@team.example "), "Fixes it.\n\ncc: kim@team.example");
        assert_eq!(text("", "kim@team.example"), "cc: kim@team.example");
        assert_eq!(
            text("Fixes it.\n\ncc: kim@team.example\n\n<!-- bot -->", "kim@team.example alex@team.example"),
            "Fixes it.\n\ncc: kim@team.example alex@team.example\n\n<!-- bot -->"
        );
        assert_eq!(text("Fixes it.\n\ncc: kim@team.example", " "), "Fixes it.");
    }

    #[test]
    fn numbers_get_commas_and_a_real_minus() {
        assert_eq!(size(1354, 514), "+1,354 \u{2212}514");
        assert_eq!(with_commas(0), "0");
        assert_eq!(with_commas(999), "999");
        assert_eq!(with_commas(1_000_000), "1,000,000");
    }

    #[test]
    fn mr_numbers_group_in_threes() {
        assert_eq!(bang(7), "!7");
        assert_eq!(bang(102), "!102");
        assert_eq!(bang(1234), "!1_234");
        assert_eq!(bang(12345), "!12_345");
        assert_eq!(bang(1234567), "!1_234_567");
    }

    #[test]
    fn a_branch_or_an_agent_is_for_the_ticket_its_name_starts_with() {
        assert_eq!(key("sam/ap-899-parser-lookup-tables").as_deref(), Some("ap-899"));
        assert_eq!(key("ap-899").as_deref(), Some("ap-899"));
        assert_eq!(key("AP-899").as_deref(), Some("ap-899"));
        assert_eq!(key("sam/ap-8990-other"), Some("ap-8990".into()), "ap-899 and ap-8990 are two");
        for none in ["sam/parser-cleanup", "sql-audit", "sam/ap-", "sam/-12-x"] {
            assert_eq!(key(none), None, "{none}");
        }
    }

    #[test]
    fn a_project_draws_its_stacks_and_waiting_tickets_together_with_their_agents() {
        let (snap, tickets, sessions) = swarm();
        let b = board(&snap, &rules(), &HashMap::new(), &sessions, &[], &tickets, NOW);
        let tree = lines(&b.tree);
        let from = tree.iter().position(|l| l.ends_with("Parser migration")).unwrap();
        let to = from + tree[from..].iter().position(|l| l == "│").unwrap();
        assert_eq!(
            tree[from..to],
            [
                "│  Parser migration",
                "├─ ○ ap-1-grammar-file in review !301 AP-1",
                "│  └─ ○ ap-2-recovery in review !302 AP-2 [ap-2 working]",
                "│     └─ Parser lookup tables AP-3 Todo waits on AP-2",
                "│        └─ Alert on parser slowdowns AP-6 Todo waits on AP-3",
                "├─ Line numbers in parse errors AP-4 In Progress [ap-4 waiting on you]",
            ],
            "AP-5 is in the backlog and waits on nothing on the board, so it stays off, and AP-7's MR is live"
        );
    }

    #[test]
    fn an_agent_hc_runs_on_an_mrs_branch_is_at_work_on_it() {
        let (mut snap, tickets, sessions) = swarm();
        snap.mrs.push(Mr { reviewers: vec![], ..mr(303, "sam", "sam/lookup-keys", "main") });
        // Named after another ticket, as the swarm sends its agents on.
        let agents = [
            hc_agent("mini/t1q", "ap-4", hc::Status::Working, "sam/lookup-keys"),
            hc_agent("mini/t9", "ap-1", hc::Status::Stopped, "sam/ap-1-grammar-file"),
        ];
        let b = board(&snap, &rules(), &HashMap::new(), &sessions, &agents, &tickets, NOW);
        let tree = lines(&b.tree);
        assert!(tree.iter().any(|l| l.ends_with("lookup-keys ask for review !303 [ap-4 working]")), "{tree:#?}");
        assert!(tree.iter().any(|l| l.ends_with("ap-1-grammar-file in review !301 AP-1")), "a done agent isn't at work: {tree:#?}");
        let claude = lines(&b.claude);
        assert!(claude.contains(&"ap-4 Working !303".to_string()) && claude.contains(&"ap-1 Stopped !301".to_string()), "{claude:#?}");
        let on = b.tree.iter().find_map(|r| match &r.item {
            Item::Mr { iid: 303, agent, .. } => agent.clone(),
            _ => None,
        });
        let nobody = |agent: Option<&Worker>| panel(snap.mr(303).unwrap(), &snap, &rules(), agent).facts.contains(&"nobody asked to review".into());
        assert!(nobody(None));
        assert!(!nobody(on.as_ref()), "it isn't ready to ask for review while the agent works");
    }

    /// An agent of hc's that came to `status` a minute ago, in no checkout.
    fn hc_agent(address: &str, name: &str, status: hc::Status, branch: &str) -> hc::Agent {
        hc::Agent {
            address: address.into(),
            name: name.into(),
            status,
            said: String::new(),
            at: NOW - 60,
            pid: None,
            branch: branch.into(),
        }
    }

    #[test]
    fn hcs_agents_sort_with_the_sessions_and_a_quiet_one_leaves() {
        let mut quiet = hc_agent("mini/t3", "ap-3", hc::Status::Stopped, "");
        quiet.at = NOW - crate::claude::SHOWN_FOR - 1;
        // One hc runs here, the process of the session at work, and one on
        // another machine whose process there has the same number.
        let mut sessions = sessions();
        sessions[0].pid = Some(4242);
        let mut here = hc_agent("t9", "ap-9", hc::Status::Working, "");
        here.pid = Some(4242);
        let mut there = hc_agent("mini/t4", "ap-4", hc::Status::Working, "");
        there.pid = Some(4242);
        let agents = [
            hc_agent("mini/t1", "ap-1", hc::Status::Stopped, ""),
            quiet,
            hc_agent("mini/t2", "ap-2", hc::Status::Blocked, ""),
            here,
            there,
        ];
        let b = board(&snapshot(), &rules(), &HashMap::new(), &sessions, &agents, &[], NOW);
        let claude = lines(&b.claude);
        assert_eq!(claude[0], "ap-2 Blocked", "what waits on you is first");
        assert_eq!(claude[1], "ap-4 Working", "then what is at work, the newest first");
        assert!(claude[2].starts_with("◐ A fresh board working"), "{claude:#?}");
        assert!(claude.iter().position(|l| l == "ap-1 Stopped") > claude.iter().position(|l| l.contains("waiting on you")), "{claude:#?}");
        assert!(!claude.iter().any(|l| l.starts_with("ap-3") || l.starts_with("ap-9")), "the quiet one left, and the one here is its session's row: {claude:#?}");
        let terminal = b.claude.iter().find_map(|r| match &r.item {
            Item::Claude { id, terminal, .. } if id == "s1" => terminal.clone(),
            _ => None,
        });
        assert_eq!(terminal.as_deref(), Some("t9"));
    }

    #[test]
    fn an_mr_on_a_branch_named_without_its_ticket_is_found_by_linears_link() {
        let (mut snap, mut tickets, sessions) = swarm();
        let price = mr(303, "sam", "sam/batch-size", "main");
        tickets.push(Ticket {
            id: "AP-8".into(),
            title: "Land batch-size on main".into(),
            started: true,
            project: "Parser migration".into(),
            has_mr: true,
            mrs: vec![price.url.clone()],
            ..Default::default()
        });
        snap.mrs.push(price);
        let b = board(&snap, &rules(), &HashMap::new(), &sessions, &[], &tickets, NOW);
        let tree = lines(&b.tree);
        let from = tree.iter().position(|l| l.ends_with("Parser migration")).unwrap();
        let to = from + tree[from..].iter().position(|l| l == "│").unwrap();
        assert_eq!(
            tree[from + 1..to].iter().filter(|l| l.contains("AP-8")).collect::<Vec<_>>(),
            ["├─ ○ batch-size in review !303 AP-8"],
            "in its ticket's project, by its MR and not as a ticket row of its own"
        );
    }

    #[test]
    fn a_ticket_is_on_its_track_by_its_mr_else_by_whether_it_has_started() {
        let (mut snap, tickets, sessions) = swarm();
        snap.mrs.iter_mut().find(|m| m.iid == 302).unwrap().draft = true;
        let b = board(&snap, &rules(), &HashMap::new(), &sessions, &[], &tickets, NOW);
        let stages: Vec<Option<Stage>> = b.tree.iter().filter(|r| matches!(r.item, Item::Mr { .. } | Item::Ticket { .. })).map(|r| Stage::of(&r.item)).collect();
        let from = stages.iter().position(Option::is_some).unwrap();
        assert_eq!(stages[from - 1], None, "an MR for no ticket of yours has no track");
        assert_eq!(
            stages[from..from + 5],
            [Some(Stage::Review), Some(Stage::Draft), Some(Stage::Ticket), Some(Stage::Ticket), Some(Stage::Agent)],
            "!301, !302, then AP-3 and AP-6 waiting, then AP-4 started"
        );
    }

    #[test]
    fn a_ticket_waiting_on_an_mr_not_drawn_stands_at_the_top_of_its_project() {
        let (mut snap, mut tickets, sessions) = swarm();
        // AP-1 and AP-2's stack has gone quiet into the fold.
        for m in snap.mrs.iter_mut().filter(|m| m.iid >= 301) {
            m.reviewers = vec![];
            m.updated = NOW - 30 * DAY;
        }
        (tickets[2].started, tickets[2].state) = (true, "In Progress".into());
        let b = board(&snap, &rules(), &HashMap::new(), &sessions, &[], &tickets, NOW);
        let tree = lines(&b.tree);
        let from = tree.iter().position(|l| l.ends_with("Parser migration")).unwrap();
        assert_eq!(
            tree[from + 1..from + 4],
            [
                "├─ Parser lookup tables AP-3 In Progress waits on AP-2",
                "│  └─ Alert on parser slowdowns AP-6 Todo waits on AP-3",
                "├─ Line numbers in parse errors AP-4 In Progress [ap-4 waiting on you]",
            ]
        );
    }

    #[test]
    fn a_session_is_on_a_branch_only_from_a_worktree_of_its_own() {
        let (snap, tickets, mut sessions) = swarm();
        // "A fresh board" sits in the main checkout, which is on cache-keys.
        let mut s = fixture::sessions()[0].clone();
        sessions.push(s.clone());
        (s.id, s.title, s.works_on) = ("s4".into(), "Fix the grammar read".into(), Some("sam/ap-1-grammar-file".into()));
        sessions.push(s);
        let b = board(&snap, &rules(), &HashMap::new(), &sessions, &[], &tickets, NOW);
        let on = |iid: u64| {
            b.tree.iter().find_map(|r| match &r.item {
                Item::Mr { iid: i, agent, .. } if *i == iid => Some(agent.as_ref().map(|w| w.name.clone())),
                _ => None,
            })
        };
        assert_eq!(on(104), Some(None), "a session's branch alone puts it on no row; its worktree has to be made for it");
        assert_eq!(on(301), Some(Some("Fix the grammar read".into())));
    }
}
