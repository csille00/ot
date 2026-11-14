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
pub fn home() -> Option<PathBuf> {
    std::env::var_os("T3CODE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".t3")))
}

/// Whether `address` is a T3 thread's id rather than an hc address like "t9" or "mini/t1q".
pub fn owns(address: &str) -> bool {
    // Threads started through T3's CLI or MCP have their uuid after a "mcp:".
    let address = address.split_once(':').filter(|(src, _)| src.bytes().all(|b| b.is_ascii_lowercase())).map_or(address, |(_, id)| id);
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

/// What each thread waits on you for, by id: its questions and approvals.
fn waiting() -> &'static Mutex<HashMap<String, Vec<String>>> {
    static WAITING: OnceLock<Mutex<HashMap<String, Vec<String>>>> = OnceLock::new();
    WAITING.get_or_init(Default::default)
}

/// What the thread waits on you for, as T3's database said when last read: a
/// turn to end its conversation with, as T3 shows it, until you answer in T3.
pub fn waits_turn(address: &str) -> Option<crate::claude::Turn> {
    let asks = waiting().lock().ok()?.get(address)?.clone();
    let lines: Vec<String> = asks.iter().map(|a| format!("- {a}")).collect();
    let text = format!("**Waiting on you.** Answer in T3 (w opens it):\n\n{}", lines.join("\n"));
    Some(crate::claude::Turn { who: crate::claude::Who::Claude, text, at: 0, times: 1, agent: None })
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
        let waits = crate::finder::pending();
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
                // Every thread's link is kept, for w on one read from the thread list.
                if let Some(l) = t["link"].as_str() {
                    found.insert(id.clone(), l.to_string());
                }
                let settled = t["settled"].as_bool().unwrap_or(false);
                if status != "running" && (settled || now - at > RECENT) {
                    return None;
                }
                let open = t["pullRequests"].as_array().into_iter().flatten().filter(|p| p["state"] == "open");
                let open: Vec<(String, u64)> = open.filter_map(|p| Some((p["repository"].as_str()?.to_string(), p["number"].as_u64()?))).collect();
                opened.insert(id.clone(), open);
                let shown = self.show(&id, &updated, status == "running");
                let asks = waits.get(&id);
                let status = match status {
                    _ if asks.is_some() => Status::Blocked,
                    "running" => Status::Working,
                    _ if shown.pending > 0 => Status::Blocked,
                    "failed" | "error" => Status::Blocked,
                    _ => Status::Stopped,
                };
                Some(Agent {
                    name: t["title"].as_str().unwrap_or(&id).to_string(),
                    said: match asks.and_then(|a| a.first()) {
                        Some(a) => format!("asks you: {a}"),
                        None if shown.pending > 0 => "waits on your answer".into(),
                        None => String::new(),
                    },
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
        if let Ok(mut w) = waiting().lock() {
            *w = waits;
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
    match crate::finder::conversation(address, 60) {
        Ok(turns) if !turns.is_empty() => Ok(turns),
        _ => conversation_by_cli(address),
    }
}

/// The same through T3's CLI, which takes a second; for when its database can't be read.
fn conversation_by_cli(address: &str) -> Result<Vec<crate::claude::Turn>, String> {
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

/// Stops the thread's running turn, as T3's Stop button does.
pub fn interrupt(address: &str) -> Result<bool, String> {
    dispatch(serde_json::json!({ "type": "thread.stop", "threadId": address })).map(|_| true)
}

/// Gives the thread a title of yours, as renaming it in T3 does.
pub fn rename(address: &str, title: &str) -> Result<(), String> {
    dispatch(serde_json::json!({ "type": "thread.metadata.update", "threadId": address, "title": title })).map(|_| ())
}

/// Starts a thread in the T3 project this checkout is in, `text` its first message; its id.
pub fn start(text: &str) -> Result<String, String> {
    let bin = bin().ok_or("no t3 here")?;
    let mut args = vec!["thread".to_string(), "start".into()];
    if let Some((provider, model)) = crate::finder::last_model() {
        args.extend(["--provider".into(), provider, "--model".into(), model]);
    }
    args.extend([".".into(), "-".into()]);
    let mut child = Command::new(bin)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("t3: {e}"))?;
    if let Some(mut i) = child.stdin.take() {
        i.write_all(text.as_bytes()).map_err(|e| format!("t3: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("t3: {e}"))?;
    if !out.status.success() {
        return Err(last_line(&out.stderr));
    }
    let said = String::from_utf8_lossy(&out.stdout);
    said.split_whitespace().find(|w| owns(w)).map(String::from).ok_or_else(|| format!("t3 started no thread: {}", said.trim()))
}

/// Settles the thread, which takes it off the board.
pub fn settle(address: &str) -> Result<(), String> {
    dispatch(serde_json::json!({ "type": "thread.settle", "threadId": address })).map(|_| ())
}

/// One orchestration command through T3's WebSocket; T3's CLI has no settle or stop yet.
/// The command gets a fresh id.
fn dispatch(mut command: Value) -> Result<Value, String> {
    command["commandId"] = Value::String(uuid4());
    rpc("orchestration.dispatchCommand", command)
}

/// Sends `text` to the thread with these images attached, as T3's apps do: the images
/// stored first, then the message naming them.
pub fn send_with_images(address: &str, text: &str, images: &[PathBuf], busy: bool) -> Result<(), String> {
    let message_id = uuid4();
    let mut uploads = vec![];
    for p in images {
        let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
        let mime = crate::compose::mime(p).ok_or_else(|| format!("{} isn't an image", p.display()))?;
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "image".into());
        uploads.push(serde_json::json!({
            "type": "image",
            "name": name,
            "mimeType": mime,
            "sizeBytes": bytes.len(),
            "dataUrl": format!("data:{mime};base64,{}", crate::term::base64(&bytes)),
        }));
    }
    let stored = rpc("assets.persistChatAttachments", serde_json::json!({ "threadId": address, "messageId": message_id, "attachments": uploads }))?;
    let attachments = stored["attachments"].clone();
    dispatch(serde_json::json!({
        "type": "message.dispatch",
        "threadId": address,
        "messageId": message_id,
        "text": text,
        "attachments": attachments,
        "createdBy": "user",
        "creationSource": "web",
        // A busy thread takes it after its turn, as `t3 thread send` does.
        "dispatchMode": { "type": if busy { "queue_after_active" } else { "start_immediately" } },
    }))
    .map(|_| ())
}

/// One of T3's WebSocket RPCs, as its own apps call them; the server's answer back.
fn rpc(tag: &str, payload: Value) -> Result<Value, String> {
    let origin = server_origin()?;
    let ticket = match ws_ticket(&origin, &token(false)?) {
        Err(e) if e == "401" => ws_ticket(&origin, &token(true)?)?,
        t => t?,
    };
    let url = format!("{}/ws?wsTicket={ticket}&orchestrationProtocol=2", origin.replacen("http", "ws", 1));
    let (mut ws, _) = tungstenite::connect(url.as_str()).map_err(|e| format!("T3's socket: {e}"))?;
    let request = serde_json::json!({ "_tag": "Request", "id": "1", "tag": tag, "payload": payload, "headers": [] });
    ws.send(tungstenite::Message::text(request.to_string())).map_err(|e| format!("T3's socket: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let msg = ws.read().map_err(|e| format!("T3's socket: {e}"))?;
        let Ok(text) = msg.to_text() else { continue };
        for frame in text.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(v) = serde_json::from_str::<Value>(frame) else { continue };
            match v["_tag"].as_str() {
                Some("Ping") => {
                    let _ = ws.send(tungstenite::Message::text(r#"{"_tag":"Pong"}"#));
                }
                Some("Exit") if v["requestId"] == "1" => {
                    let _ = ws.close(None);
                    return match v["exit"]["_tag"].as_str() {
                        Some("Success") => Ok(v["exit"]["value"].clone()),
                        _ => Err(failure(&v["exit"]["cause"])),
                    };
                }
                Some("Defect" | "ClientProtocolError") => return Err(format!("T3 refused: {}", v)),
                _ => {}
            }
        }
    }
    Err("T3 didn't answer in 15s".into())
}

/// The words in an RPC failure's cause, however deep T3 put them.
fn failure(cause: &Value) -> String {
    fn find(v: &Value) -> Option<String> {
        if let Some(m) = v.get("message").and_then(Value::as_str) {
            return Some(m.to_string());
        }
        match v {
            Value::Object(o) => o.values().find_map(find),
            Value::Array(a) => a.iter().find_map(find),
            _ => None,
        }
    }
    find(cause).unwrap_or_else(|| format!("T3 refused: {cause}"))
}

/// Where the running T3 server answers, from its runtime state.
fn server_origin() -> Result<String, String> {
    let path = home().ok_or("no T3 home")?.join("userdata/server-runtime.json");
    let v: Value = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).ok_or("T3's server isn't running")?;
    v["origin"].as_str().map(String::from).ok_or_else(|| "T3's server didn't say where it is".into())
}

/// A short-lived ticket for T3's socket, or "401" when the token is no good.
fn ws_ticket(origin: &str, token: &str) -> Result<String, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).http_status_as_error(false).build().into();
    let mut resp = agent
        .post(&format!("{origin}/api/auth/websocket-ticket"))
        .header("Authorization", &format!("Bearer {token}"))
        .send_empty()
        .map_err(|e| format!("T3: {e}"))?;
    if resp.status() == 401 {
        return Err("401".into());
    }
    let v: Value = resp.body_mut().read_json().map_err(|e| format!("T3's ticket ({}): {e}", resp.status()))?;
    v["ticket"].as_str().map(String::from).ok_or_else(|| format!("T3 gave no ticket: {v}"))
}

/// ot's own T3 session token, kept in ~/.cache/ot/t3-token (yours alone) for its 30
/// days; a new one when `again` or none is kept.
fn token(again: bool) -> Result<String, String> {
    let path = PathBuf::from(std::env::var_os("HOME").ok_or("no HOME")?).join(".cache/ot/t3-token");
    if !again {
        if let Ok(t) = std::fs::read_to_string(&path) {
            if !t.trim().is_empty() {
                return Ok(t.trim().to_string());
            }
        }
    }
    let bin = bin().ok_or("no t3 here")?;
    let out = Command::new(bin)
        .args(["auth", "session", "issue", "--scope", "orchestration:read", "--scope", "orchestration:operate", "--ttl", "30d", "--label", "ot", "--json"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("t3: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let v: Value = text.find('{').and_then(|i| serde_json::from_str(&text[i..]).ok()).ok_or_else(|| last_line(&out.stderr))?;
    let t = v["token"].as_str().ok_or("t3 issued no token")?.to_string();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path).map_err(|e| e.to_string())?;
    f.write_all(t.as_bytes()).map_err(|e| e.to_string())?;
    Ok(t)
}

/// A random version 4 UUID, from the kernel's random source.
fn uuid4() -> String {
    let mut b = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = std::io::Read::read_exact(&mut f, &mut b);
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
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
        assert!(owns("mcp:40d81c83-6935-4c94-b279-7224c4c7be17"), "a thread started through T3's CLI");
        assert!(!owns("mini/t1q:40d81c83-6935-4c94-b279-7224c4c7be17"));
        assert!(!owns("bba1a549x60f8-4425-bb4e-569c328a3cc4"));
    }
}

