//! The Claude Code agents hc runs, here and on the machines it reaches
//! (`hc ls`), each with the branch its checkout is on, so the board can say
//! when one is at work on an MR. An agent elsewhere has its branch read
//! through hc (`hc exec --on`), only while hc says that machine is
//! connected, so reading never wakes a stopped VM. And what you ask of
//! one: its terminal, something typed to it, its stop key.
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub struct Agent {
    /// hc's address for it: "t9" here, "mini/t1q" on the machine named mini.
    pub address: String,
    pub name: String,
    pub status: Status,
    /// What it says it is on, in its own words; empty when it says nothing.
    pub said: String,
    /// Unix seconds of when it came to that status.
    pub at: i64,
    /// Its process, on its own machine.
    pub pid: Option<u32>,
    /// The branch its checkout is on; empty while that is unknown.
    pub branch: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// On a turn.
    Working,
    /// Stopped mid-turn on a person: a permission, a question, an error.
    Blocked,
    /// Done with its turn, or not given one yet.
    Stopped,
}

/// How long a branch read stays good. Agents rarely switch branch, and a
/// read on another machine costs a round trip.
const BRANCHES_FOR: Duration = Duration::from_secs(30);

/// How often the agents are read with hc saying nothing, for what it
/// doesn't say: a checkout changing branch, a machine connecting.
const QUIET_FOR: Duration = Duration::from_secs(60);

/// The least time between two reads, while agents say what they are on
/// one after another.
const APART: Duration = Duration::from_secs(1);

/// How long until `hc events` is started again after it ends, as when
/// hc's server restarts.
const AGAIN_IN: Duration = Duration::from_secs(5);

/// Hands `tell` hc's agents for as long as ot runs: now, and whenever
/// they change, as hc says one came, went or says something new of
/// itself (`hc events`).
pub fn watch(mut tell: impl FnMut(Vec<Agent>) + Send + 'static) {
    let (wake, woken) = mpsc::channel();
    std::thread::spawn(move || loop {
        follow(&wake);
        std::thread::sleep(AGAIN_IN);
    });
    std::thread::spawn(move || {
        let mut reader = Reader::default();
        let mut last = vec![];
        loop {
            let found = reader.agents();
            if found != last {
                last = found.clone();
                tell(found);
            }
            let _ = woken.recv_timeout(QUIET_FOR);
            // A burst of changes is read once.
            std::thread::sleep(APART);
            while woken.try_recv().is_ok() {}
        }
    });
}

/// Follows `hc events` until it ends, waking the reader at each change.
fn follow(wake: &Sender<()>) {
    let child = Command::new("hc")
        .arg("events")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return };
    let lines = BufReader::new(child.stdout.take().unwrap()).lines();
    for line in lines.map_while(Result::ok) {
        if serde_json::from_str(&line).is_ok_and(|ev| changes(&ev)) {
            let _ = wake.send(());
        }
    }
    let _ = child.wait();
}

/// Whether a line of `hc events` changes what ot shows of an agent: one
/// came or went, or its status did, the words it says with it included.
fn changes(ev: &Value) -> bool {
    match (ev["method"].as_str(), ev["params"]["type"].as_str()) {
        (Some("heartbeat"), _) => false,
        (Some("event"), Some("created" | "lost" | "restarted" | "finished" | "removed" | "status")) => true,
        (Some("event"), _) => false,
        // The terminals it opens with, and again after it fell behind.
        _ => true,
    }
}

/// This terminal on the agent's, until ctrl+\ there.
pub fn attach(address: &str) -> Command {
    let mut c = Command::new("hc");
    c.args(["attach", "--", address]);
    c
}

/// Types `text` to the agent and presses enter, once nobody has typed
/// there for a few seconds; or hc's reason it didn't.
pub fn send(address: &str, text: &str) -> Result<(), String> {
    call(&["send", "--", address, text]).map(|_| ())
}

/// Presses the agent's stop key and watches it ten seconds: whether it
/// stopped.
pub fn interrupt(address: &str) -> Result<bool, String> {
    let said = call(&["interrupt", "--", address])?;
    Ok(matches!(said["outcome"].as_str(), Some("stopped" | "exited")))
}

/// What hc answers `args`, or its reason it couldn't.
fn call(args: &[&str]) -> Result<Value, String> {
    let out = Command::new("hc").args(args).output().map_err(|e| format!("hc: {e}"))?;
    if out.status.success() {
        return serde_json::from_slice(&out.stdout).map_err(|e| format!("hc: {e}"));
    }
    let why = serde_json::from_slice::<Value>(&out.stderr).ok().and_then(|v| v["error"]["message"].as_str().map(String::from));
    Err(why.unwrap_or_else(|| String::from_utf8_lossy(&out.stderr).trim().to_string()))
}

/// Reads hc's agents, keeping each checkout's branch for BRANCHES_FOR and
/// when each one stopped.
#[derive(Default)]
struct Reader {
    /// By address, a stopped agent's word from hc and the turns it has
    /// had, then since when and its process: read once, as neither changes
    /// until one of the first two does.
    stopped: HashMap<String, ((String, u64), (i64, Option<u32>))>,
    /// By machine ("" here) and checkout.
    branches: HashMap<(String, String), String>,
    read: Option<Instant>,
}

impl Reader {
    /// hc's Claude Code agents now; none without hc.
    fn agents(&mut self) -> Vec<Agent> {
        let Ok(ls) = call(&["ls"]) else {
            return vec![];
        };
        if self.read.is_none_or(|t| t.elapsed() >= BRANCHES_FOR) {
            self.branches.clear();
            self.read = Some(Instant::now());
        }
        let list = |k: &str| ls[k].as_array().cloned().unwrap_or_default();
        let connected: Vec<String> = list("machines")
            .iter()
            .filter(|m| m["state"] == "connected")
            .filter_map(|m| m["name"].as_str().map(String::from))
            .collect();
        let text = |t: &Value, k: &str| t[k].as_str().unwrap_or_default().to_string();
        let mut stopped = std::mem::take(&mut self.stopped);
        let agents = list("terminals")
            .iter()
            .filter(|t| t["agent"] == "claude-code")
            .map(|t| {
                let address = text(t, "address");
                let machine = address.split_once('/').map_or("", |(m, _)| m).to_string();
                let cwd = text(t, "cwd");
                let reachable = machine.is_empty() || connected.contains(&machine);
                let branch = self
                    .branches
                    .entry((machine.clone(), cwd.clone()))
                    .or_insert_with(|| if reachable { branch(&machine, &cwd) } else { String::new() })
                    .clone();
                let word = text(t, "status");
                let status = match word.as_str() {
                    "working" => Status::Working,
                    "blocked" | "error" => Status::Blocked,
                    _ => Status::Stopped,
                };
                // `hc ls` has neither its words nor since when.
                let turn = (word, t["turn"].as_u64().unwrap_or_default());
                let (said, at, pid) = match stopped.remove(&address).filter(|(was, _)| status == Status::Stopped && *was == turn) {
                    Some((_, (at, pid))) => (String::new(), at, pid),
                    None if reachable => more(&address),
                    None => (String::new(), jiff::Timestamp::now().as_second(), None),
                };
                if status == Status::Stopped {
                    self.stopped.insert(address.clone(), (turn, (at, pid)));
                }
                Agent {
                    name: Some(text(t, "name")).filter(|n| !n.is_empty()).unwrap_or_else(|| address.clone()),
                    address,
                    status,
                    said,
                    at,
                    pid,
                    branch,
                }
            })
            .collect();
        agents
    }
}

/// What the agent says it is on, or which of you it waits for, since when
/// it has had its status, and its process (`hc get`); since now if hc
/// can't say.
fn more(address: &str) -> (String, i64, Option<u32>) {
    let t = call(&["get", "--", address]).unwrap_or_default();
    let said = [&t["status"]["msg"], &t["status"]["kind"]].into_iter().find_map(|v| v.as_str()).unwrap_or_default();
    let ms = t["status_ms"].as_i64().or_else(|| t["created_ms"].as_i64());
    let pid = [&t["foreground"]["pid"], &t["pid"]].into_iter().find_map(|v| v.as_u64()).map(|p| p as u32);
    (said.to_string(), ms.map_or_else(|| jiff::Timestamp::now().as_second(), |ms| ms / 1000), pid)
}

/// The branch checked out at `cwd` on `machine` ("" for here); empty when
/// git can't say.
fn branch(machine: &str, cwd: &str) -> String {
    let git = ["git", "-C", cwd, "branch", "--show-current"];
    let mut c = if machine.is_empty() {
        Command::new(git[0])
    } else {
        let mut c = Command::new("hc");
        c.args(["exec", "--on", machine, "--timeout", "10", git[0]]);
        c
    };
    c.args(&git[1..])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_change_is_an_agent_coming_going_or_saying_something_of_itself() {
        let event = |kind: &str| json!({"jsonrpc": "2.0", "method": "event", "params": {"type": kind, "terminal": "mini/t3s", "seq": 2, "data": {}}});
        assert!(changes(&json!({"channel": null, "seq": 1, "terminals": []})), "what it opens with");
        assert!(changes(&event("status")));
        assert!(changes(&event("finished")));
        assert!(!changes(&event("title")));
        assert!(!changes(&json!({"jsonrpc": "2.0", "method": "heartbeat", "params": {"seq": 1}})));
    }
}
