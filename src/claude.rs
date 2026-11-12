//! What Claude Code is doing in this repo, read from what it writes for
//! itself and nothing else: ~/.claude/sessions/<pid>.json while a session
//! runs, and its transcript, ~/.claude/projects/<cwd as a slug>/<id>.jsonl,
//! read as it grows. An agent a session starts has a transcript of its own
//! beside it (<id>/subagents/agent-<agent>.jsonl, with a .meta.json naming
//! the tool call that started it, or the name a named one was given), read
//! the same way under that call. ot never starts or owns a session here.
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{Read as _, Seek, SeekFrom};
use crate::git::Worktree;
use std::path::{Path, PathBuf};

/// One session of Claude Code on one of this repo's checkouts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Session {
    pub id: String,
    /// The name Claude gave it, else the first line you typed.
    pub title: String,
    pub branch: String,
    pub cwd: PathBuf,
    /// Its process, while it runs.
    pub pid: Option<u32>,
    /// The branch it is at work on, if it runs in a worktree made for one
    /// (works_on).
    pub works_on: Option<String>,
    pub state: State,
    /// Unix seconds of its newest record.
    pub at: i64,
    pub started: i64,
    /// What was said and done, in order.
    pub turns: Vec<Turn>,
    pub cost: f64,
}

/// One thing that happened: what you typed, what Claude said, or a tool
/// it used, in a few words.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub who: Who,
    pub text: String,
    pub at: i64,
    /// The same tool on the same thing this many times in a row.
    pub times: u32,
    /// When the tool was an agent: what that agent said and did.
    pub agent: Option<Agent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who {
    You,
    Claude,
    Tool,
}

/// An agent a session started, as its own transcript tells it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Agent {
    /// The id of the tool call that started it, which its transcript's
    /// meta file names.
    pub tool_id: String,
    /// The name it was started with, empty for none. The ticket swarm
    /// names each of its agents after its ticket ("ap-899"), and the meta
    /// file of a named agent names it by this, not by its call.
    pub name: String,
    pub turns: Vec<Turn>,
    pub finished: bool,
    /// When the session stopped it by name with TaskStop, which Claude Code
    /// never marks finished.
    pub stopped: Option<i64>,
    /// Unix seconds of its transcript's newest record.
    pub at: i64,
    /// It has said its piece and waits to be told more: its last turn
    /// ended, and nothing has come to it since.
    pub waiting: bool,
    /// Its transcript once found, and how far into it has been read.
    pub transcript: Option<PathBuf>,
    pub read_to: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    /// Running and on something.
    Busy,
    /// Running and waiting on you.
    Idle,
    /// Its process is gone.
    #[default]
    Gone,
}

impl Session {
    /// The last thing that happened, in one line: what you asked, what it
    /// said, or the tool it is on; inside an agent still running, what
    /// that agent is on.
    pub fn doing(&self) -> String {
        doing(&self.turns)
    }

    /// Its agent named `name`, the last it started by that name.
    pub fn agent(&self, name: &str) -> Option<&Agent> {
        self.turns.iter().rev().filter_map(|t| t.agent.as_ref()).find(|a| a.name == name)
    }
}

impl Agent {
    /// The same of the agent alone.
    pub fn doing(&self) -> String {
        doing(&self.turns)
    }

    /// Not finished, and not stopped since it last wrote: a message to a
    /// stopped agent starts it again.
    pub fn running(&self) -> bool {
        !self.finished && self.stopped.is_none_or(|stop| self.at > stop)
    }
}

fn doing(turns: &[Turn]) -> String {
    match turns.last() {
        Some(Turn { agent: Some(a), .. }) if a.running() && !a.turns.is_empty() => doing(&a.turns),
        Some(t) if t.who == Who::You => format!("you: {}", first_line(&t.text)),
        Some(t) => first_line(&t.text),
        None => String::new(),
    }
}

/// How long a session whose process is gone stays listed.
pub const SHOWN_FOR: i64 = 2 * 3600;

/// Reads the sessions on `worktrees`, remembering how far into each
/// transcript it has read so a long one is read once.
#[derive(Default)]
pub struct Reader {
    read: HashMap<PathBuf, (u64, Session)>,
}

impl Reader {
    /// Every session running on one of `worktrees`, and the ones gone
    /// quiet within SHOWN_FOR of `now`; the running ones first, newest
    /// first in each.
    pub fn sessions(&mut self, worktrees: &[Worktree], now: i64) -> Vec<Session> {
        let live = live();
        let mut out = vec![];
        for (i, checkout) in worktrees.iter().enumerate() {
            let wt = &checkout.path;
            let dir = home().join("projects").join(slug(wt));
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let path = e.path();
                if path.extension().is_none_or(|x| x != "jsonl") {
                    continue;
                }
                let id = path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let state = live.get(&id).copied();
                let Ok(meta) = e.metadata() else { continue };
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs() as i64);
                // A transcript nobody runs and nobody touched lately is not
                // read at all, so a repo with hundreds costs nothing.
                if state.is_none() && now - modified > SHOWN_FOR {
                    self.read.remove(&path);
                    continue;
                }
                let (read_to, s) = self.read.entry(path.clone()).or_insert_with(|| {
                    let mut s = Session::default();
                    s.id = id.clone();
                    s.cwd = wt.clone();
                    (0, s)
                });
                if meta.len() > *read_to {
                    let (records, to) = lines_from(&path, *read_to);
                    for v in &records {
                        fold(s, v);
                    }
                    *read_to = to;
                }
                read_agents(&path.with_extension(""), s);
                s.state = state.map_or(State::Gone, |(state, _)| state);
                s.pid = state.and_then(|(_, pid)| pid);
                s.works_on = works_on(checkout, i == 0, &s.branch);
                if s.title.is_empty() || (s.state == State::Gone && now - s.at > SHOWN_FOR) {
                    continue;
                }
                out.push(s.clone());
            }
        }
        out.sort_by_key(|s| (s.state == State::Gone, -s.at));
        out
    }
}

/// The branch a session in `checkout` is at work on: the one git says the
/// worktree is on now, when it isn't the main checkout (`main`), where
/// sessions on anything sit, and the session's own newest record says so
/// too, so one left open in a worktree used since for another branch stays
/// with its own.
fn works_on(checkout: &Worktree, main: bool, branch: &str) -> Option<String> {
    (!main && !checkout.branch.is_empty() && checkout.branch == branch).then(|| branch.to_string())
}

/// The agents the session started that haven't finished, read on from
/// where they were. `dir` is the session's own folder.
fn read_agents(dir: &Path, s: &mut Session) {
    let cwd = s.cwd.clone();
    // The folder is listed once a scan, and only when an agent is new.
    let mut found: Option<HashMap<String, PathBuf>> = None;
    // A name finds the newest transcript by it, so only the last agent
    // started by a name looks it up, and never takes one another agent
    // reads: started again, its new transcript may not be written yet.
    let mut held: HashSet<PathBuf> = s.turns.iter().filter_map(|t| t.agent.as_ref()?.transcript.clone()).collect();
    let last: HashMap<String, usize> = s
        .turns
        .iter()
        .enumerate()
        .filter_map(|(i, t)| Some((t.agent.as_ref()?.name.clone(), i)))
        .collect();
    for (i, t) in s.turns.iter_mut().enumerate() {
        let Some(a) = t.agent.as_mut().filter(|a| !a.finished) else {
            continue;
        };
        if a.transcript.is_none() {
            let found = found.get_or_insert_with(|| transcripts(&dir.join("subagents")));
            let by_name = || found.get(&a.name).filter(|p| last.get(&a.name) == Some(&i) && !held.contains(*p));
            a.transcript = found.get(&a.tool_id).or_else(by_name).cloned();
            held.extend(a.transcript.clone());
        }
        let Some(path) = &a.transcript else {
            continue;
        };
        let (records, to) = lines_from(path, a.read_to);
        a.read_to = to;
        for v in &records {
            a.at = a.at.max(at_of(v, a.at));
            match v["type"].as_str() {
                Some("assistant") => a.waiting = v["message"]["stop_reason"] == "end_turn",
                Some("user") => a.waiting = false,
                _ => {}
            }
            if let Some(t) = turn_of(v, &cwd, at_of(v, 0)) {
                push(&mut a.turns, t);
            }
        }
        a.finished = meta(path)["finished"] == true;
    }
}

/// Each agent's transcript in `dir`, by the tool call that started it, and
/// a named one's by its name too; of two by one name, the newer.
fn transcripts(dir: &Path) -> HashMap<String, PathBuf> {
    let mut out = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut named: HashMap<String, (std::time::SystemTime, PathBuf)> = HashMap::new();
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "jsonl") {
            continue;
        }
        let m = meta(&path);
        if let Some(tool) = m["toolUseId"].as_str() {
            out.insert(tool.to_string(), path.clone());
        }
        if let Some(name) = m["name"].as_str() {
            let at = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            if named.get(name).is_none_or(|(was, _)| at > *was) {
                named.insert(name.to_string(), (at, path));
            }
        }
    }
    out.extend(named.into_iter().map(|(name, (_, path))| (name, path)));
    out
}

/// What Claude Code says of an agent, from the meta file beside its
/// transcript: the tool call that started it or its name, and whether it
/// finished.
fn meta(transcript: &Path) -> Value {
    std::fs::read_to_string(transcript.with_extension("meta.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".claude")
}

/// Claude Code names a checkout's folder by its path with every character
/// that isn't a letter or a digit as a dash.
fn slug(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The sessions running now, by id, each with its process, from the file
/// each keeps under its pid while it runs and removes when it exits.
fn live() -> HashMap<String, (State, Option<u32>)> {
    let mut out = HashMap::new();
    let Ok(entries) = std::fs::read_dir(home().join("sessions")) else {
        return out;
    };
    for e in entries.flatten() {
        let Ok(text) = std::fs::read_to_string(e.path()) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(id) = v["sessionId"].as_str() else {
            continue;
        };
        let state = match v["status"].as_str() {
            Some("idle") => State::Idle,
            _ => State::Busy,
        };
        let pid = e.path().file_stem().and_then(|p| p.to_str()?.parse().ok());
        out.insert(id.to_string(), (state, pid));
    }
    out
}

/// The transcript's whole records from `from` to its end, and how far that
/// is. A line still being written is left for next time.
fn lines_from(path: &Path, from: u64) -> (Vec<Value>, u64) {
    let mut bytes = vec![];
    let read = std::fs::File::open(path).and_then(|mut f| {
        f.seek(SeekFrom::Start(from))
            .and_then(|_| f.read_to_end(&mut bytes))
    });
    if read.is_err() {
        return (vec![], from);
    }
    let Some(whole) = bytes.iter().rposition(|b| *b == b'\n').map(|i| i + 1) else {
        return (vec![], from);
    };
    let records = String::from_utf8_lossy(&bytes[..whole])
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    (records, from + whole as u64)
}

/// One record of the transcript into what the session is.
fn fold(s: &mut Session, v: &Value) {
    if let Some(b) = v["gitBranch"].as_str() {
        s.branch = b.to_string();
    }
    let at = at_of(v, s.at);
    s.at = s.at.max(at);
    if s.started == 0 {
        s.started = at;
    }
    match v["type"].as_str() {
        Some("ai-title") => {
            if let Some(t) = v["aiTitle"].as_str() {
                s.title = t.to_string();
            }
        }
        Some("cost-state") => s.cost = v["totalCostUSD"].as_f64().unwrap_or(s.cost),
        _ => {}
    }
    // An agent's records in the session's transcript are told by the
    // agent's own.
    if v["isSidechain"] == true {
        return;
    }
    let calls = v["message"]["content"].as_array().into_iter().flatten();
    let stops = calls.filter(|p| p["name"] == "TaskStop").filter_map(|p| p["input"]["task_id"].as_str());
    for name in stops.filter(|n| !n.is_empty()) {
        if let Some(a) = s.turns.iter_mut().rev().filter_map(|t| t.agent.as_mut()).find(|a| a.name == name) {
            a.stopped = Some(at);
        }
    }
    if let Some(t) = turn_of(v, &s.cwd, at) {
        if s.title.is_empty() && t.who == Who::You {
            s.title = first_line(&t.text);
        }
        push(&mut s.turns, t);
    }
}

fn at_of(v: &Value, or: i64) -> i64 {
    v["timestamp"].as_str().and_then(seconds).unwrap_or(or)
}

/// A turn onto the list, folded into the last when it is the same tool on
/// the same thing again.
fn push(turns: &mut Vec<Turn>, t: Turn) {
    match turns.last_mut() {
        Some(last)
            if t.who == Who::Tool
                && last.who == Who::Tool
                && last.text == t.text
                && t.agent.is_none() =>
        {
            last.times += 1;
            last.at = t.at;
        }
        _ => turns.push(t),
    }
}

/// The turn a record is, if it is one: what you typed, or what Claude said
/// or did.
fn turn_of(v: &Value, cwd: &Path, at: i64) -> Option<Turn> {
    let turn = |who: Who, text: String, agent: Option<Agent>| Turn {
        who,
        text,
        at,
        times: 1,
        agent,
    };
    match v["type"].as_str()? {
        "user" if v["isMeta"] != true => {
            let text = prompt(&v["message"]["content"])?;
            Some(turn(Who::You, text, None))
        }
        "assistant" => {
            let parts = v["message"]["content"].as_array()?;
            let mut last = None;
            for p in parts {
                match p["type"].as_str() {
                    Some("text") => {
                        let t = p["text"].as_str().unwrap_or("").trim();
                        if !t.is_empty() {
                            last = Some(turn(Who::Claude, t.to_string(), None));
                        }
                    }
                    Some("tool_use") => {
                        let agent = (p["name"] == "Agent").then(|| Agent {
                            tool_id: p["id"].as_str().unwrap_or("").to_string(),
                            name: p["input"]["name"].as_str().unwrap_or("").to_string(),
                            ..Default::default()
                        });
                        last = Some(turn(Who::Tool, tool(p, cwd), agent));
                    }
                    _ => {}
                }
            }
            last
        }
        _ => None,
    }
}

/// What you typed, when the record is you typing and not a command's
/// output or something fed in. To an agent the swarm started, what its
/// lead says is what it is asked.
fn prompt(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(t) => t.clone(),
        Value::Array(parts) => parts.iter().find(|p| p["type"] == "text")?["text"]
            .as_str()?
            .to_string(),
        _ => return None,
    };
    let text = text.trim();
    if let Some(said) = from_lead(text) {
        return Some(said);
    }
    (!text.is_empty() && !text.starts_with('<')).then(|| text.to_string())
}

/// A message from the swarm's lead without the tags Claude Code wraps it
/// in. What agents say to the lead or each other is said again in their
/// own transcripts, so only the lead's counts.
fn from_lead(text: &str) -> Option<String> {
    let tagged = text.strip_prefix("<teammate-message teammate_id=\"team-lead\"")?;
    let body = match tagged.strip_prefix('>') {
        Some(body) => body,
        None => tagged.split_once("\">")?.1,
    };
    let body = body.trim_end();
    Some(body.strip_suffix("</teammate-message>").unwrap_or(body).trim().to_string())
}

/// A tool call in a few words: what it does to what. A file in the
/// checkout is named from its root; one anywhere else by its name alone.
fn tool(p: &Value, cwd: &Path) -> String {
    let input = &p["input"];
    let rel = |k: &str| {
        let path = Path::new(input[k].as_str().unwrap_or(""));
        path.strip_prefix(cwd)
            .ok()
            .or_else(|| path.file_name().map(Path::new))
            .map(|r| r.to_string_lossy().to_string())
            .unwrap_or_default()
    };
    match p["name"].as_str().unwrap_or("") {
        "Edit" | "Write" | "NotebookEdit" => format!("editing {}", rel("file_path")),
        "Read" => format!("reading {}", rel("file_path")),
        "Grep" => format!("searching for {}", input["pattern"].as_str().unwrap_or("")),
        "Glob" => format!("finding {}", input["pattern"].as_str().unwrap_or("")),
        "Bash" => match input["description"].as_str() {
            Some(d) if !d.is_empty() => d.to_string(),
            _ => format!("$ {}", first_line(input["command"].as_str().unwrap_or(""))),
        },
        "Agent" => format!("an agent: {}", input["description"].as_str().unwrap_or("")),
        name => name.to_string(),
    }
}

fn first_line(t: &str) -> String {
    t.trim().lines().next().unwrap_or("").trim().to_string()
}

fn seconds(iso: &str) -> Option<i64> {
    iso.parse::<jiff::Timestamp>().ok().map(|t| t.as_second())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session(records: &[Value]) -> Session {
        let mut s = Session {
            cwd: "/home/sam/app".into(),
            ..Default::default()
        };
        for r in records {
            fold(&mut s, r);
        }
        s
    }

    #[test]
    fn a_session_is_what_its_records_say() {
        let s = session(&[
            json!({ "type": "user", "timestamp": "2027-01-15T10:00:00Z", "gitBranch": "sam/thing",
                    "message": { "content": "make the board fresh\nand fast" } }),
            json!({ "type": "assistant", "timestamp": "2027-01-15T10:00:05Z", "gitBranch": "sam/thing",
                    "message": { "content": [{ "type": "text", "text": "Looking at the fetcher." },
                                             { "type": "tool_use", "name": "Read", "input": { "file_path": "/home/sam/app/src/main.rs" } }] } }),
            json!({ "type": "ai-title", "aiTitle": "A fresh board" }),
            json!({ "type": "cost-state", "totalCostUSD": 0.25 }),
        ]);
        assert_eq!(s.title, "A fresh board");
        assert_eq!(s.branch, "sam/thing");
        assert_eq!(s.doing(), "reading src/main.rs");
        assert_eq!(
            s.turns
                .iter()
                .map(|t| (t.who, t.text.as_str()))
                .collect::<Vec<_>>(),
            [
                (Who::You, "make the board fresh\nand fast"),
                (Who::Tool, "reading src/main.rs"),
            ],
            "a reply that goes straight on to a tool is that tool"
        );
        assert_eq!((s.started, s.at), (1_800_007_200, 1_800_007_205));
        assert_eq!(s.cost, 0.25);
    }

    #[test]
    fn without_a_name_the_first_line_you_typed_is_the_title() {
        let s = session(&[
            json!({ "type": "user", "message": { "content": "<command-name>/model</command-name>" } }),
            json!({ "type": "user", "isMeta": true, "message": { "content": "a skill's text" } }),
            json!({ "type": "user", "message": { "content": [{ "type": "text", "text": "  fix the test  \nplease" }] } }),
        ]);
        assert_eq!(s.title, "fix the test");
        assert_eq!(s.doing(), "you: fix the test");
    }

    #[test]
    fn a_tool_call_says_what_it_does() {
        let call = |name: &str, input: Value| {
            session(&[json!({ "type": "assistant", "message": { "content": [
                { "type": "tool_use", "name": name, "input": input }] } })])
            .doing()
        };
        assert_eq!(
            call("Edit", json!({ "file_path": "/home/sam/app/a.rs" })),
            "editing a.rs"
        );
        assert_eq!(
            call("Edit", json!({ "file_path": "/tmp/scratch/notes.md" })),
            "editing notes.md"
        );
        assert_eq!(
            call(
                "Bash",
                json!({ "command": "cargo test", "description": "Run the tests" })
            ),
            "Run the tests"
        );
        assert_eq!(call("Bash", json!({ "command": "ls\nwc" })), "$ ls");
        assert_eq!(
            call("Grep", json!({ "pattern": "fn main" })),
            "searching for fn main"
        );
    }

    #[test]
    fn the_same_tool_on_the_same_thing_again_is_one_turn() {
        let edit = json!({ "type": "assistant", "timestamp": "2027-01-15T10:00:05Z", "message": { "content": [
            { "type": "tool_use", "name": "Edit", "input": { "file_path": "/home/sam/app/a.rs" } }] } });
        let s = session(&[edit.clone(), edit.clone(), edit]);
        assert_eq!(s.turns.len(), 1);
        assert_eq!((s.turns[0].times, s.turns[0].at), (3, 1_800_007_205));
    }

    #[test]
    fn an_agent_is_a_turn_with_its_own_turns_under_it() {
        let mut s = session(&[
            json!({ "type": "assistant", "message": { "content": [
                { "type": "tool_use", "id": "toolu_1", "name": "Agent", "input": { "description": "Review the diff" } }] } }),
            json!({ "type": "assistant", "isSidechain": true, "message": { "content": [
                { "type": "text", "text": "the agent's own records are not the session's" }] } }),
        ]);
        assert_eq!(s.turns.len(), 1);
        let a = s.turns[0].agent.as_mut().unwrap();
        assert_eq!(a.tool_id, "toolu_1");
        assert_eq!(s.turns[0].text, "an agent: Review the diff");
        let a = s.turns[0].agent.as_mut().unwrap();
        for v in [
            json!({ "type": "user", "message": { "content": "Review this" } }),
            json!({ "type": "assistant", "message": { "content": [
                { "type": "tool_use", "name": "Read", "input": { "file_path": "/home/sam/app/a.rs" } }] } }),
        ] {
            push(
                &mut a.turns,
                turn_of(&v, Path::new("/home/sam/app"), 0).unwrap(),
            );
        }
        assert_eq!(
            s.doing(),
            "reading a.rs",
            "a session on an agent is on what the agent is on"
        );
        s.turns[0].agent.as_mut().unwrap().finished = true;
        assert_eq!(s.doing(), "an agent: Review the diff");
    }

    #[test]
    fn the_slug_is_the_path_with_dashes() {
        assert_eq!(
            slug(Path::new("/Users/sam/acme/.claude/worktrees/x_1")),
            "-Users-sam-acme--claude-worktrees-x-1"
        );
    }

    #[test]
    fn a_transcript_is_read_on_from_where_it_was() {
        let dir = std::env::temp_dir().join(format!("ot-claude-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"ai-title\",\"aiTitle\":\"one\"}\n{\"type\":\"ai-ti",
        )
        .unwrap();
        let (records, to) = lines_from(&path, 0);
        assert_eq!((records.len(), to), (1, 36), "the half line waits");
        std::fs::write(&path, "{\"type\":\"ai-title\",\"aiTitle\":\"one\"}\n{\"type\":\"ai-title\",\"aiTitle\":\"two\"}\n").unwrap();
        let (records, to) = lines_from(&path, to);
        assert_eq!((records[0]["aiTitle"].as_str(), to), (Some("two"), 72));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_session_is_at_work_on_its_worktrees_branch_only_in_one_made_for_it() {
        let at = |branch: &str| Worktree { path: "/slots/ap-8".into(), branch: branch.into() };
        assert_eq!(works_on(&at("sam/ap-8-x"), false, "sam/ap-8-x").as_deref(), Some("sam/ap-8-x"));
        assert_eq!(works_on(&at("sam/ap-8-x"), true, "sam/ap-8-x"), None, "the main checkout is where sessions on anything sit");
        assert_eq!(works_on(&at(""), false, "HEAD"), None, "a detached worktree is on no branch");
        assert_eq!(
            works_on(&at("sam/ap-12-y"), false, "sam/ap-8-x"),
            None,
            "one left open since its worktree moved to another branch stays with its own"
        );
    }

    #[test]
    fn a_named_agent_is_read_from_its_own_transcript_and_waits_once_its_turn_ends() {
        let dir = std::env::temp_dir().join(format!("ot-agent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let subagents = dir.join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        // A named agent's meta file names it, not the call that started it.
        std::fs::write(subagents.join("agent-aap-2-x.meta.json"), r#"{"name":"ap-2","agentType":"ap-2"}"#).unwrap();
        let records = [
            json!({ "type": "user", "message": { "content":
                "<teammate-message teammate_id=\"team-lead\" summary=\"Build AP-2\">\nBuild AP-2.\n</teammate-message>" } }),
            json!({ "type": "assistant", "message": { "stop_reason": "tool_use", "content": [
                { "type": "tool_use", "name": "Read", "input": { "file_path": "/home/sam/app/a.rs" } }] } }),
            json!({ "type": "user", "message": { "content": [{ "type": "tool_result", "content": "fn a() {}" }] } }),
            json!({ "type": "assistant", "message": { "stop_reason": "end_turn", "content": [
                { "type": "text", "text": "Pushed it." }] } }),
        ];
        let text: String = records.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(subagents.join("agent-aap-2-x.jsonl"), text).unwrap();
        let mut s = session(&[json!({ "type": "assistant", "message": { "content": [
            { "type": "tool_use", "id": "toolu_9", "name": "Agent", "input": { "name": "ap-2", "description": "Build AP-2" } }] } })]);
        read_agents(&dir, &mut s);
        let a = s.turns[0].agent.as_ref().unwrap();
        assert_eq!(
            a.turns.iter().map(|t| (t.who, t.text.as_str())).collect::<Vec<_>>(),
            [(Who::You, "Build AP-2."), (Who::Tool, "reading a.rs"), (Who::Claude, "Pushed it.")],
            "what its lead said is what it was asked"
        );
        assert!(a.waiting);
        // Started again by the same name, it waits for a transcript of its
        // own rather than read the first one's.
        s.turns.push(s.turns[0].clone());
        s.turns[1].agent = Some(Agent { tool_id: "toolu_10".into(), name: "ap-2".into(), ..Default::default() });
        read_agents(&dir, &mut s);
        assert_eq!(s.turns[1].agent.as_ref().unwrap().transcript, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_agent_the_session_stopped_is_done_until_it_is_told_more() {
        let dir = std::env::temp_dir().join(format!("ot-stop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let subagents = dir.join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::write(subagents.join("agent-aap-4-x.meta.json"), r#"{"name":"ap-4"}"#).unwrap();
        let mid_tool = json!({ "type": "assistant", "timestamp": "2027-01-15T10:01:00Z",
            "message": { "stop_reason": "tool_use", "content": [
                { "type": "tool_use", "name": "Read", "input": { "file_path": "/home/sam/app/a.rs" } }] } });
        std::fs::write(subagents.join("agent-aap-4-x.jsonl"), format!("{mid_tool}\n")).unwrap();
        let mut s = session(&[
            json!({ "type": "assistant", "timestamp": "2027-01-15T10:00:00Z", "message": { "content": [
                { "type": "tool_use", "id": "toolu_4", "name": "Agent", "input": { "name": "ap-4", "description": "Build AP-4" } }] } }),
            json!({ "type": "assistant", "timestamp": "2027-01-15T10:02:00Z", "message": { "content": [
                { "type": "tool_use", "name": "TaskStop", "input": { "task_id": "ap-4" } }] } }),
        ]);
        read_agents(&dir, &mut s);
        let a = s.turns[0].agent.as_ref().unwrap();
        assert!(!a.running(), "stopped mid-tool, it neither ends its turn nor gets marked finished");
        assert_eq!(s.doing(), "TaskStop");
        let told = json!({ "type": "user", "timestamp": "2027-01-15T10:05:00Z", "message": { "content":
            "<teammate-message teammate_id=\"team-lead\">\nGo on.\n</teammate-message>" } });
        std::fs::write(subagents.join("agent-aap-4-x.jsonl"), format!("{mid_tool}\n{told}\n")).unwrap();
        read_agents(&dir, &mut s);
        assert!(s.turns[0].agent.as_ref().unwrap().running(), "a message from its lead starts it again");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
