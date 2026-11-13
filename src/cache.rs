//! What ot keeps between runs, under ~/.cache/ot/: per project the last
//! answer GitLab gave and Linear's, so the next launch can draw before
//! asking again, the stacks you asked it to land, v's viewed marks, and the
//! review's own worktree; and for every project, the key log.
use crate::keys::Act;
use crate::land::Landing;
use crate::model::{Snapshot, Ticket};
use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/ot")
}

fn dir(project: &str) -> PathBuf {
    root().join(project)
}

/// The key log, ~/.cache/ot/keys.tsv, for seeing which keys earn their
/// place: a line per key pressed on `screen` with when (unix seconds), the
/// key and what it did there, empty for nothing. Keys typed into an input
/// or answering a question are never logged. A log that can't be written
/// costs nothing else.
pub fn keys_path() -> PathBuf {
    root().join("keys.tsv")
}

pub fn pressed(screen: &str, key: &str, act: Option<Act>) {
    let act = act.map(|a| format!("{a:?}")).unwrap_or_default();
    let line = format!("{}\t{screen}\t{key}\t{act}\n", jiff::Timestamp::now().as_second());
    let _ = std::fs::create_dir_all(root());
    if let Ok(mut log) = std::fs::OpenOptions::new().create(true).append(true).open(keys_path()) {
        let _ = log.write_all(line.as_bytes());
    }
}

/// ot's own checkout for reviews (worktree.rs).
pub fn review_tree(project: &str) -> PathBuf {
    dir(project).join("review")
}

/// Where a rebase's conflicts are settled: Claude's checkout and its runs
/// (rebase.rs).
pub fn settle_dir(project: &str) -> PathBuf {
    dir(project).join("settle")
}

/// The pictures and videos reviews showed, by GitLab's "<secret>/<name>"
/// (nvim::upload). A secret names one upload for good, so a copy never
/// goes stale.
pub fn uploads(project: &str) -> PathBuf {
    dir(project).join("uploads")
}

/// Written aside and renamed into place, so a reader never sees half.
pub fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension(format!("part{}", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// None when there is nothing yet, or what is there can't be read.
pub fn load(project: &str) -> Option<Snapshot> {
    let bytes = std::fs::read(dir(project).join("board.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn save(project: &str, snap: &Snapshot) -> Result<(), String> {
    write(
        &dir(project).join("board.json"),
        &serde_json::to_vec(snap).map_err(|e| format!("cache: {e}"))?,
    )
}

/// Where Haiku reads an agent that stopped (brief.rs), the last read kept.
pub fn brief_dir(project: &str) -> PathBuf {
    dir(project).join("brief")
}

/// Where Claude tells an MR's tour (tour.rs), a run per base and head.
pub fn tour_dir(project: &str) -> PathBuf {
    dir(project).join("tour")
}

/// Where Claude reads a stack being landed (judge.rs), a run per landing.
pub fn land_dir(project: &str) -> PathBuf {
    dir(project).join("land")
}

/// Who each MR was stacked on (model::remember); none when it can't be read.
pub fn stacked(project: &str) -> HashMap<u64, u64> {
    std::fs::read(dir(project).join("stacked.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_stacked(project: &str, was_on: &HashMap<u64, u64>) -> Result<(), String> {
    write(
        &dir(project).join("stacked.json"),
        &serde_json::to_vec(was_on).map_err(|e| format!("cache: {e}"))?,
    )
}

/// The MRs you hid from the board; none when they can't be read.
pub fn hidden(project: &str) -> HashSet<u64> {
    std::fs::read(dir(project).join("hidden.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_hidden(project: &str, hidden: &HashSet<u64>) -> Result<(), String> {
    write(
        &dir(project).join("hidden.json"),
        &serde_json::to_vec(hidden).map_err(|e| format!("cache: {e}"))?,
    )
}

/// Your tickets as Linear last gave them; none when they can't be read.
pub fn tickets(project: &str) -> Vec<Ticket> {
    std::fs::read(dir(project).join("tickets.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_tickets(project: &str, tickets: &[Ticket]) -> Result<(), String> {
    write(
        &dir(project).join("tickets.json"),
        &serde_json::to_vec(tickets).map_err(|e| format!("cache: {e}"))?,
    )
}

/// The stacks being landed; none when there are none, or they can't be read.
pub fn landings(project: &str) -> Vec<Landing> {
    std::fs::read(dir(project).join("landing.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_landings(project: &str, landings: &[Landing]) -> Result<(), String> {
    write(
        &dir(project).join("landing.json"),
        &serde_json::to_vec(landings).map_err(|e| format!("cache: {e}"))?,
    )
}

/// v's marks: a row's key and the hash of its change, a line each, written
/// by the review in nvim. Kept for the whole project, so a symbol read in
/// one MR of a stack is read in all.
pub fn viewed_path(project: &str) -> PathBuf {
    dir(project).join("viewed")
}

pub fn viewed(project: &str) -> HashSet<String> {
    std::fs::read_to_string(viewed_path(project))
        .map(|s| s.lines().map(String::from).collect())
        .unwrap_or_default()
}
