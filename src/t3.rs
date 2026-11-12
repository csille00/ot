//! T3 Code's threads as the board's agents, through T3's own CLI on this machine
//! (`t3 thread list|show|send --json`); each thread's claude names it in T3CODE_THREAD_ID.
use crate::hc::{Agent, Status};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// The least time between two reads; each `t3` run costs most of a second.
const APART: Duration = Duration::from_secs(3);

/// How often threads are read with T3's database quiet.
const QUIET_FOR: Duration = Duration::from_secs(60);

/// How long a thread's branch stays good; threads rarely switch branch.
const BRANCH_FOR: Duration = Duration::from_secs(300);

/// Threads untouched this long, and settled, are left off the board.
const RECENT: i64 = 3 * 24 * 3600;

/// The t3 the running server is, so the CLI never migrates the server's database
/// past it: T3_BIN, then the service's active version, then `t3` on PATH.
pub fn bin() -> Option<PathBuf> {
    static BIN: OnceLock<Option<PathBuf>> = OnceLock::new();
    BIN.get_or_init(|| {
        if let Some(b) = std::env::var_os("T3_BIN") {
            return Some(PathBuf::from(b));
        }
        let home = home()?;
        let state: Value = std::fs::read(home.join("runtime/service-state.json")).ok().and_then(|b| serde_json::from_slice(&b).ok())?;
        let active = home.join("runtime/versions").join(state["activeVersion"].as_str()?).join("t3");
        Some(active).filter(|p| p.exists())
    })
    .clone()
    .or_else(|| {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path).map(|d| d.join("t3")).find(|p| p.is_file())
    })
}

/// T3's home: T3CODE_HOME, else ~/.t3.
fn home() -> Option<PathBuf> {
    std::env::var_os("T3CODE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".t3")))
}

/// Whether `address` is a T3 thread's id rather than an hc address like "t9" or "mini/t1q".
pub fn owns(address: &str) -> bool {
    address.len() == 36 && address.bytes().enumerate().all(|(i, b)| if [8, 13, 18, 23].contains(&i) { b == b'-' } else { b.is_ascii_hexdigit() })
}

/// Each thread's page in T3, by id, as the last read found them.
fn links() -> &'static Mutex<HashMap<String, String>> {
    static LINKS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    LINKS.get_or_init(Default::default)
}

/// The thread's page in T3's web and mobile apps.
pub fn link(address: &str) -> Option<String> {
    links().lock().ok()?.get(address).cloned()
}

/// Each thread's open MRs, by id, as (repository, number).
fn mrs() -> &'static Mutex<HashMap<String, Vec<(String, u64)>>> {
    static MRS: OnceLock<Mutex<HashMap<String, Vec<(String, u64)>>>> = OnceLock::new();
    MRS.get_or_init(Default::default)
}

/// The newest open MR the thread made in `project`. Threads share the project's
/// checkout, so its branch says nothing of theirs; their MRs do.
pub fn mr(address: &str, project: &str) -> Option<u64> {
    let mrs = mrs().lock().ok()?;
    mrs.get(address)?.iter().filter(|(repo, _)| repo == project).map(|(_, n)| *n).max()
}

/// Hands `tell` this project's threads now and whenever T3's database changes.
pub fn watch(mut tell: impl FnMut(Vec<Agent>) + Send + 'static) {
    std::thread::spawn(move || {
        let mut reader = Reader::default();
        let (mut last, mut stamp, mut read) = (None, None, None::<Instant>);
        loop {
            let now = db_stamp();
            let due = read.is_none_or(|t| t.elapsed() >= QUIET_FOR || (now != stamp && t.elapsed() >= APART));
            if due {
                stamp = now;
                read = Some(Instant::now());
                let found = reader.agents();
                if last.as_ref() != Some(&found) {
                    last = Some(found.clone());
                    tell(found);
                }
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

/// When T3's database last changed: its write-ahead log moves on every event.
fn db_stamp() -> Option<SystemTime> {
    let dir = home()?.join("userdata");
    ["statev2.sqlite-wal", "statev2.sqlite"]
        .iter()
        .filter_map(|f| std::fs::metadata(dir.join(f)).and_then(|m| m.modified()).ok())
        .max()
}

#[derive(Default)]
struct Reader {
    /// By thread: its updatedAt when last shown, and what `show` said then.
    shown: HashMap<String, (String, Shown)>,
}

#[derive(Clone, Default)]
struct Shown {
    branch: String,
    pending: u64,
    at: Option<Instant>,
}

impl Reader {
    fn agents(&mut self) -> Vec<Agent> {
        let Ok(list) = call(&["thread", "list", "--json"]) else {
            return vec![];
        };
        let now = jiff::Timestamp::now().as_second();
        let pids = claude_pids();
        let mut found = HashMap::new();
        let mut opened = HashMap::new();
        let agents = list
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| {
                let id = t["threadId"].as_str()?.to_string();
                let status = t["status"].as_str().unwrap_or_default();
                let updated = t["updatedAt"].as_str().unwrap_or_default().to_string();
                let at = updated.parse::<jiff::Timestamp>().map_or(now, |t| t.as_second());
                let settled = t["settled"].as_bool().unwrap_or(false);
                if status != "running" && (settled || now - at > RECENT) {
                    return None;
                }
                if let Some(l) = t["link"].as_str() {
                    found.insert(id.clone(), l.to_string());
                }
                let open = t["pullRequests"].as_array().into_iter().flatten().filter(|p| p["state"] == "open");
                let open: Vec<(String, u64)> = open.filter_map(|p| Some((p["repository"].as_str()?.to_string(), p["number"].as_u64()?))).collect();
                opened.insert(id.clone(), open);
                let shown = self.show(&id, &updated, status == "running");
                let status = match status {
                    "running" => Status::Working,
                    _ if shown.pending > 0 => Status::Blocked,
                    "failed" | "error" => Status::Blocked,
                    _ => Status::Stopped,
                };
                Some(Agent {
                    name: t["title"].as_str().unwrap_or(&id).to_string(),
                    said: if shown.pending > 0 { "waits on your answer".into() } else { String::new() },
                    status,
                    at,
                    pid: pids.get(&id).copied(),
                    branch: shown.branch,
                    address: id,
                })
            })
            .collect();
        if let Ok(mut l) = links().lock() {
            *l = found;
        }
        if let Ok(mut m) = mrs().lock() {
            *m = opened;
        }
        agents
    }

    /// The thread's branch and pending answers, from `t3 thread show`, read again once
    /// it has changed (a running one only for its branch, every BRANCH_FOR).
    fn show(&mut self, id: &str, updated: &str, running: bool) -> Shown {
        if let Some((was, s)) = self.shown.get(id) {
            let fresh = s.at.is_some_and(|t| t.elapsed() < BRANCH_FOR);
            if fresh && (was == updated || running) {
                return s.clone();
            }
        }
        let Ok(v) = call(&["thread", "show", id, "--json", "--messages", "0"]) else {
            return self.shown.get(id).map(|(_, s)| s.clone()).unwrap_or_default();
        };
        let t = &v["thread"];
        // Only a worktree of its own is on a branch of the thread's.
        let own = t["worktreePath"].as_str().is_some_and(|w| !w.is_empty());
        let s = Shown {
            branch: if own { t["branch"].as_str().unwrap_or_default().to_string() } else { String::new() },
            pending: t["pendingRequestCount"].as_u64().unwrap_or_default(),
            at: Some(Instant::now()),
        };
        self.shown.insert(id.to_string(), (updated.to_string(), s.clone()));
        s
    }
}

/// The claude processes T3 runs, by the thread each names in its environment.
fn claude_pids() -> HashMap<String, u32> {
    let Ok(procs) = std::fs::read_dir("/proc") else { return HashMap::new() };
    procs
        .flatten()
        .filter_map(|p| {
            let pid: u32 = p.file_name().to_str()?.parse().ok()?;
            let comm = std::fs::read_to_string(p.path().join("comm")).ok()?;
            if comm.trim() != "claude" {
                return None;
            }
            let env = std::fs::read(p.path().join("environ")).ok()?;
            let thread = env.split(|b| *b == 0).find_map(|kv| kv.strip_prefix(b"T3CODE_THREAD_ID="))?;
            Some((String::from_utf8_lossy(thread).into_owned(), pid))
        })
        .collect()
}

/// Sends `text` to the thread; a busy one gets it as its next turn.
pub fn send(address: &str, text: &str) -> Result<(), String> {
    let bin = bin().ok_or("no t3 here")?;
    let mut child = Command::new(bin)
        .args(["thread", "send", address, "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("t3: {e}"))?;
    if let Some(mut i) = child.stdin.take() {
        i.write_all(text.as_bytes()).map_err(|e| format!("t3: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("t3: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(last_line(&out.stderr))
    }
}

/// The thread's latest messages as turns to read, oldest first.
pub fn conversation(address: &str) -> Result<Vec<crate::claude::Turn>, String> {
    use crate::claude::{Turn, Who};
    let v = call(&["thread", "show", address, "--json", "--messages", "60"])?;
    let turns = v["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let who = match m["role"].as_str()? {
                "user" => Who::You,
                "assistant" => Who::Claude,
                _ => return None,
            };
            let text = m["text"].as_str()?.trim();
            (!text.is_empty()).then(|| Turn { who, text: text.to_string(), at: 0, times: 1, agent: None })
        })
        .collect();
    Ok(turns)
}

/// T3's CLI has no stop yet.
pub fn interrupt(_address: &str) -> Result<bool, String> {
    Err("T3's CLI can't stop a thread yet: stop it in T3".into())
}

/// What `t3 <args>` prints as JSON, or its reason it couldn't.
fn call(args: &[&str]) -> Result<Value, String> {
    let bin = bin().ok_or("no t3 here")?;
    let out = Command::new(bin).args(args).stdin(Stdio::null()).output().map_err(|e| format!("t3: {e}"))?;
    if !out.status.success() {
        return Err(last_line(&out.stderr));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("t3: {e}"))
}

fn last_line(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("t3 failed").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_ids_are_t3s_and_hc_addresses_are_not() {
        assert!(owns("bba1a549-60f8-4425-bb4e-569c328a3cc4"));
        assert!(!owns("t9"));
        assert!(!owns("mini/t1q"));
        assert!(!owns("bba1a549x60f8-4425-bb4e-569c328a3cc4"));
    }
}
