//! One turn of Claude Code that ot runs for an answer in a shape it names:
//! a rebase's conflicts settled (rebase.rs), which of a stack can go out
//! in one deploy (judge.rs), both on Opus; or where an agent's work stands
//! (brief.rs), on Haiku. With only the tools it is given, no MCP or slash
//! commands, and never stopping to ask permission.
use serde_json::Value;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

/// Claude's `model` in `dir`, also reading `run`, fed `prompt`: its
/// answer, as `schema` has it, or why it gave none. Everything it said is
/// kept in `run`.
pub fn claude(model: &str, dir: &Path, run: &Path, tools: &str, schema: &str, prompt: &str) -> Result<Value, String> {
    let err = std::fs::File::create(run.join("claude.err")).map_err(|e| e.to_string())?;
    let run_dir = run.to_string_lossy().into_owned();
    let mut child = Command::new("claude")
        .args(["-p", "--model", model, "--tools", tools, "--add-dir", &run_dir])
        .args(["--strict-mcp-config", "--disable-slash-commands", "--permission-prompts", "none"])
        .args(["--permission-mode", "acceptEdits", "--output-format", "json", "--json-schema", schema])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(err)
        .spawn()
        .map_err(|e| format!("Claude isn't here: {e}"))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let text = prompt.to_string();
    let feed = std::thread::spawn(move || stdin.write_all(text.as_bytes()));
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let _ = feed.join();
    let _ = std::fs::write(run.join("claude.json"), &out.stdout);
    let all: Value = serde_json::from_slice(&out.stdout).map_err(|_| "Claude stopped without an answer".to_string())?;
    if all["is_error"] == true {
        return Err(all["result"].as_str().unwrap_or("Claude stopped without an answer").to_string());
    }
    Ok(all["structured_output"].clone())
}

/// The same turn, with `heard` handed each event as it happens (Claude
/// Code's stream-json lines: its thinking as it grows, its answer as it is
/// written), for a caller that shows the wait.
pub fn hearing(model: &str, dir: &Path, run: &Path, tools: &str, schema: &str, prompt: &str, heard: &mut dyn FnMut(&Value)) -> Result<Value, String> {
    use std::io::BufRead as _;
    let err = std::fs::File::create(run.join("claude.err")).map_err(|e| e.to_string())?;
    let run_dir = run.to_string_lossy().into_owned();
    let mut child = Command::new("claude")
        .args(["-p", "--model", model, "--tools", tools, "--add-dir", &run_dir])
        .args(["--strict-mcp-config", "--disable-slash-commands", "--permission-prompts", "none"])
        .args(["--permission-mode", "acceptEdits", "--json-schema", schema])
        .args(["--output-format", "stream-json", "--verbose", "--include-partial-messages"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(err)
        .spawn()
        .map_err(|e| format!("Claude isn't here: {e}"))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let text = prompt.to_string();
    let feed = std::thread::spawn(move || stdin.write_all(text.as_bytes()));
    let mut all = Value::Null;
    for line in std::io::BufReader::new(child.stdout.take().expect("piped stdout")).lines().map_while(Result::ok) {
        match serde_json::from_str::<Value>(&line) {
            // The last line is what `claude` gets whole.
            Ok(v) if v["type"] == "result" => all = v,
            Ok(v) => heard(&v),
            Err(_) => {}
        }
    }
    let _ = child.wait();
    let _ = feed.join();
    let _ = std::fs::write(run.join("claude.json"), all.to_string());
    if all.is_null() {
        return Err("Claude stopped without an answer".into());
    }
    if all["is_error"] == true {
        return Err(all["result"].as_str().unwrap_or("Claude stopped without an answer").to_string());
    }
    Ok(all["structured_output"].clone())
}
