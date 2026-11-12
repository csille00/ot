//! A stack rebased here, the way git rebase would, Claude settling what
//! conflicts, and pushed only once you have seen what it will do.
//!
//! GitLab's own rebase runs on its side and gives up at the first commit
//! that conflicts, leaving a sentence on the MR and nothing else; and
//! rebasing one layer of a stack there leaves the layers above carrying the
//! old copies of its commits. So each layer's own commits are picked onto
//! the layer below as rebased (the bottom onto its target as it is now), in
//! memory (merge-tree, commit-tree) the way rebase picks them, with no
//! checkout touched. A commit already where it goes is kept; one that comes
//! out empty is dropped. A commit that conflicts is Claude's to settle, in
//! ot's own checkout, and one it cannot settle stops it all with nothing
//! changed. Then every layer is pushed at once, each leased on the commit
//! fetched, so GitLab takes all of it or none, and your local branches
//! follow where that loses nothing.
use crate::ask;
use crate::git::{checkout_of, follow, git_env, git_in, merge_tree, rev};
use crate::model::{bang, Mr};
use crate::worktree;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;

/// What `prepare` found it would do, for you to see before `push`.
#[derive(Debug, Default)]
pub struct Rebased {
    pub layers: Vec<Layer>,
    /// For you, a line each: what each layer gets, and what Claude settled.
    pub said: Vec<Said>,
    /// "rebase it?" or "rebase the stack?"; empty when nothing would move.
    pub question: String,
    /// The merged MR whose commits the bottom layer left out (`prepare`'s
    /// `merged_below`).
    pub left_out: Option<u64>,
    /// How many of its commits Claude settled conflicts in.
    pub settled: usize,
    /// MRs whose own change would come out different: a conflict settled
    /// wrong, or a merge commit's change the replay leaves out.
    pub changed: Vec<u64>,
}

#[derive(Debug)]
pub struct Layer {
    pub iid: u64,
    pub branch: String,
    /// GitLab's commit when fetched, which the push is leased on.
    pub remote: String,
    /// Your local branch's commit, if the branch is here.
    pub local: Option<String>,
    /// Where the replay started: yours or GitLab's (prepare says which).
    pub start: String,
    /// Where it ends up; `remote` when it is already in place.
    pub new: String,
}

/// A line of what a rebase will do: a layer's heading, or a detail under
/// one (its commits, what Claude settled, how the lint went).
#[derive(Debug)]
pub struct Said {
    pub detail: bool,
    pub text: String,
}

impl Said {
    fn head(text: String) -> Said {
        Said { detail: false, text }
    }
    fn detail(text: String) -> Said {
        Said { detail: true, text }
    }
}

/// The settling of one layer's conflicts: where, for which MR, onto what.
struct Settling<'a> {
    repo: &'a Path,
    /// ot's directory for it: Claude's checkout and each run's files.
    at: &'a Path,
    mr: &'a Mr,
    /// What the commits go onto, as Claude is told it.
    on: String,
    step: &'a mut dyn FnMut(&'static str, &str),
}

impl Rebased {
    /// Some layer would move, so there is something to push.
    pub fn moves(&self) -> bool {
        self.layers.iter().any(|l| l.new != l.remote)
    }

    /// Only git's replay of what GitLab has: no conflict Claude settled, no
    /// change that comes out different, no commit of yours not pushed yet.
    pub fn plain(&self) -> bool {
        self.settled == 0 && self.changed.is_empty() && self.layers.iter().all(|l| l.start == l.remote)
    }

    /// The bottom layer and the merged MR whose commits it left out.
    pub fn past(&self) -> Option<(u64, u64)> {
        Some((self.layers.first()?.iid, self.left_out?))
    }
}

/// One rebase prepared at a time: they share ot's checkout for settling,
/// and one's fetch can fail on refs another's is writing.
static PREPARING: Mutex<()> = Mutex::new(());

/// Everything but the push, for the stack `layers` (bottom first, as the
/// board stacks them) of the repo checked out at `repo`; `at` is ot's
/// directory for Claude's settling, its checkout and each run's files.
/// `merged_below` is the MR the bottom layer was stacked on, once that has
/// merged: a squash brought its commits in as new ones, so only the bottom
/// layer's commits past its head are its own. `step` hears what it is at: a
/// word for the row, and the whole of it for the panel. Nothing here or on
/// GitLab is changed, and an error says so.
pub fn prepare(repo: &Path, layers: &[Mr], merged_below: Option<u64>, at: &Path, step: &mut dyn FnMut(&'static str, &str)) -> Result<Rebased, String> {
    let _one = match PREPARING.try_lock() {
        Ok(one) => one,
        Err(_) => {
            step("waiting", "waits for the rebase being prepared ahead of it");
            PREPARING.lock().unwrap_or_else(|e| e.into_inner())
        }
    };
    step("fetching", "");
    // GitLab keeps a merged MR's head after its branch is gone.
    let merged = merged_below.map(|b| format!("refs/remotes/origin/merge-requests/{b}"));
    let mut refs: Vec<String> = merged
        .iter()
        .zip(merged_below)
        .map(|(r, b)| format!("+refs/merge-requests/{b}/head:{r}"))
        .collect();
    for m in layers {
        for b in [&m.target, &m.source] {
            let spec = format!("+refs/heads/{b}:refs/remotes/origin/{b}");
            if !refs.contains(&spec) {
                refs.push(spec);
            }
        }
    }
    let mut fetch = vec!["fetch", "--quiet", "origin"];
    fetch.extend(refs.iter().map(String::as_str));
    git_in(repo, &fetch).map_err(|e| format!("could not fetch the stack's branches: {e}"))?;

    let mut done: Vec<Layer> = vec![];
    let mut said = vec![];
    let mut settled = 0;
    let mut changed = vec![];
    for m in layers {
        step("rebasing", &format!("replaying {}'s own commits", bang(m.iid)));
        let b = m.source.as_str();
        let remote = rev(repo, &format!("refs/remotes/origin/{b}")).ok_or_else(|| format!("GitLab has no branch {b}"))?;
        let local = rev(repo, &format!("refs/heads/{b}"));
        // Yours where it is GitLab's and more, or GitLab's rewritten here (a
        // rebase, a conflict fixed by hand): your branch was at GitLab's
        // commit once, by its reflog. GitLab's where yours has nothing of
        // its own.
        let (start, mine) = match &local {
            Some(l) if *l != remote && !behind(repo, b, l, &remote) => {
                if ancestor(repo, &remote, l) {
                    (l.clone(), Some(format!("with {} of yours not on GitLab yet", count(repo, &remote, l))))
                } else if was_at(repo, &format!("refs/heads/{b}"), &remote) {
                    (l.clone(), Some("as you rebased it here".to_string()))
                } else {
                    return Err(format!("your {b} and GitLab's each have commits the other does not; git pull --rebase in its checkout puts them together, then b. nothing was changed"));
                }
            }
            _ => (remote.clone(), None),
        };
        // On a layer, its own commits are those past where it was cut from
        // that layer, by the tips GitLab's branch for it has had (the fork
        // point): a layer rebased since has old commits that are not these.
        let below = done.iter().find(|d| d.branch == m.target);
        let (old, onto, on, fork) = match below {
            Some(d) => (
                d.start.clone(),
                d.new.clone(),
                bang(d.iid),
                fork_point(repo, &format!("refs/remotes/origin/{}", m.target), &start),
            ),
            None => (
                format!("refs/remotes/origin/{}", m.target),
                rev(repo, &format!("refs/remotes/origin/{}", m.target)).ok_or_else(|| format!("GitLab has no branch {}", m.target))?,
                m.target.clone(),
                merged.as_deref().and_then(|r| rev(repo, r)),
            ),
        };
        let on_now = match below {
            Some(_) => format!("{on} as just rebased"),
            None => format!("{on} as it is now"),
        };
        let mut settling = Settling { repo, at, mr: m, on: on_now.clone(), step: &mut *step };
        let played = replay(&mut settling, &onto, &old, &start, fork.as_deref()).map_err(|c| {
            let how = match below {
                Some(_) => format!("rebase it onto {on} by hand in its checkout, then b"),
                None => format!("git pull --rebase in a checkout of {b}, fix it there, then b again does the rest"),
            };
            format!(
                "nothing was changed: {}'s commit {} \"{}\" conflicts with {}, in {}. {}. {how}",
                bang(m.iid),
                short(repo, &c.commit),
                subject(repo, &c.commit),
                on_now.trim_end_matches(" as it is now"),
                names(&c.files),
                c.why.trim_end_matches('.')
            )
        })?;
        let new = played.new;
        // What it changes against the layer below, before and after: the
        // same, unless something in the replay went wrong.
        let differs = if new == remote {
            vec![]
        } else {
            let below_was = match below {
                Some(d) => d.start.clone(),
                None => fork.clone().unwrap_or_else(|| onto.clone()),
            };
            let was_base = git_in(repo, &["merge-base", &start, &below_was]).map(|s| s.trim().to_string())?;
            drift(&own_change(repo, &was_base, &start)?, &own_change(repo, &onto, &new)?)
        };
        // A checkout of it has to be able to move with it.
        if let (Some(l), Some(wt)) = (&local, checkout_of(repo, b)) {
            if *l != new {
                if rev(&wt, "HEAD").as_ref() != Some(l) {
                    return Err(format!("{b} in {} is partway through something (its HEAD is not the branch); finish it there, then b. nothing was changed", wt.display()));
                }
                let moved = git_in(repo, &["diff", "--name-only", l, &new])?;
                let dirty = git_in(&wt, &["diff", "--name-only", "HEAD"])?;
                let hit: Vec<&str> = dirty.lines().filter(|f| moved.lines().any(|m| m == *f)).collect();
                if !hit.is_empty() {
                    return Err(format!("uncommitted changes in {} are in files the rebase changes ({}); commit them first, then b. nothing was changed", wt.display(), names(&hit)));
                }
            }
        }
        let n = count(repo, &onto, &new);
        said.push(Said::head(format!("{}  {}", bang(m.iid), m.title)));
        let mut line = format!("{n} commit{} onto {on}", if n == 1 { "" } else { "s" });
        if new == remote {
            line.push_str(", already there");
        } else {
            if below.is_none() {
                line.push_str(&format!(", {} new there", count(repo, &start, &onto)));
            }
            if let (None, Some(b)) = (below, merged_below) {
                line.push_str(&format!(", leaving out {}'s, which merged", bang(b)));
            }
            if let Some(mine) = &mine {
                line.push_str(&format!(", {mine}"));
            }
            if played.dropped > 0 {
                line.push_str(&format!(
                    "; {} more dropped, as {on} has {} already",
                    played.dropped,
                    if played.dropped == 1 { "its change" } else { "theirs" }
                ));
            }
        }
        said.push(Said::detail(line));
        settled += played.claude;
        said.extend(played.settled.into_iter().map(Said::detail));
        if !differs.is_empty() {
            changed.push(m.iid);
            said.extend(differs.into_iter().map(Said::detail));
        }
        done.push(Layer { iid: m.iid, branch: b.to_string(), remote, local, start, new });
    }
    let mut r = Rebased { layers: done, said, question: String::new(), left_out: merged_below, settled, changed };
    let one = r.layers.len() == 1;
    if r.moves() {
        r.said.push(Said::head(if one {
            "it is pushed with a new pipeline; your branch here follows".into()
        } else {
            "every layer is pushed at once, each with a new pipeline; your branches here follow".into()
        }));
        r.question = if r.changed.is_empty() {
            if one { "rebase it?" } else { "rebase the stack?" }.into()
        } else {
            let iids: Vec<String> = r.changed.iter().map(|i| bang(*i)).collect();
            format!("rebase {}, though it changes what {} does?", if one { "it" } else { "the stack" }, iids.join(" and "))
        };
    } else {
        let iids: Vec<String> = r.layers.iter().map(|l| bang(l.iid)).collect();
        r.said = vec![Said::head(format!(
            "{} already in place on {}",
            iids.join(", "),
            layers.first().map(|m| m.target.as_str()).unwrap_or_default()
        ))];
    }
    Ok(r)
}

/// Pushes every layer that moved, at once, each leased on the commit
/// fetched, then moves your local branches to follow. The lines about any
/// branch that had to be left alone.
pub fn push(repo: &Path, r: &Rebased, step: &mut dyn FnMut(&'static str, &str)) -> Result<Vec<String>, String> {
    let moved: Vec<&Layer> = r.layers.iter().filter(|l| l.new != l.remote).collect();
    if !moved.is_empty() {
        step("pushing", "");
        let leases: Vec<String> = moved.iter().map(|l| format!("--force-with-lease=refs/heads/{}:{}", l.branch, l.remote)).collect();
        let pushes: Vec<String> = moved.iter().map(|l| format!("{}:refs/heads/{}", l.new, l.branch)).collect();
        let mut args = vec!["push", "--quiet", "--atomic", "--no-verify"];
        args.extend(leases.iter().map(String::as_str));
        args.push("origin");
        args.extend(pushes.iter().map(String::as_str));
        git_in(repo, &args).map_err(|e| {
            let tail: Vec<&str> = e.lines().filter(|l| !l.trim().is_empty()).collect();
            let tail = &tail[tail.len().saturating_sub(3)..];
            format!("GitLab took none of it, so nothing changed: {}", tail.join(" "))
        })?;
    }
    let mut left = vec![];
    for l in &r.layers {
        if let Some(local) = &l.local {
            if *local != l.new {
                left.extend(follow(repo, &l.branch, local, &l.new));
            }
        }
    }
    Ok(left)
}

struct Played {
    new: String,
    dropped: usize,
    /// A line on each commit Claude settled, with how its lint went under it.
    settled: Vec<String>,
    /// How many it settled.
    claude: usize,
}

/// A commit that could not be put onto its layer.
struct Conflict {
    commit: String,
    files: Vec<String>,
    why: String,
}

/// The tip's own commits picked onto `onto`: those past the old base and
/// the fork, less any that are the same change as one there (rev-list
/// --cherry-pick, as rebase does), and the new tip.
fn replay(s: &mut Settling, onto: &str, old: &str, tip: &str, fork: Option<&str>) -> Result<Played, Conflict> {
    let repo = s.repo;
    let range = format!("{old}...{tip}");
    let mut args = vec!["rev-list", "--reverse", "--no-merges", "--right-only", "--cherry-pick", &range];
    let past;
    if let Some(f) = fork {
        past = format!("^{f}");
        args.push(&past);
    }
    let commits = git_in(repo, &args).map_err(|e| Conflict { commit: tip.into(), files: vec![], why: e })?;
    let mut played = Played { new: onto.to_string(), dropped: 0, settled: vec![], claude: 0 };
    for c in commits.lines() {
        if rev(repo, &format!("{c}^")).as_deref() == Some(&played.new) {
            played.new = c.to_string();
            continue;
        }
        let (new, settled) = match merge_tree(repo, &played.new, c) {
            Ok(tree) => (commit_as(repo, c, &tree, &played.new).map_err(|e| Conflict { commit: c.into(), files: vec![], why: e })?, vec![]),
            Err(files) => {
                let onto = played.new.clone();
                played.claude += 1;
                settle(s, &onto, c, &files).map_err(|why| Conflict { commit: c.into(), files: files.clone(), why })?
            }
        };
        if new == played.new {
            played.dropped += 1;
        } else {
            played.new = new;
        }
        played.settled.extend(settled);
    }
    Ok(played)
}

/// Lines added and lines removed, by file.
type Change = BTreeMap<String, (Vec<String>, Vec<String>)>;

/// The lines `tip` adds and removes against `base`.
fn own_change(repo: &Path, base: &str, tip: &str) -> Result<Change, String> {
    let diff = git_in(repo, &["diff", "-U0", "--no-color", "--no-ext-diff", "--no-renames", base, tip])?;
    let mut files = Change::new();
    let mut file = String::new();
    for l in diff.lines() {
        if l.starts_with("+++ ") || l.starts_with("--- ") {
            continue;
        }
        if let Some(rest) = l.strip_prefix("diff --git ") {
            file = rest.rsplit(" b/").next().unwrap_or(rest).to_string();
        } else if let Some(added) = l.strip_prefix('+') {
            files.entry(file.clone()).or_default().0.push(added.trim_end().to_string());
        } else if let Some(removed) = l.strip_prefix('-') {
            files.entry(file.clone()).or_default().1.push(removed.trim_end().to_string());
        }
    }
    Ok(files)
}

/// A line for each file whose change differs between `was` and `now`, up
/// to three, with a line it no longer makes and one it makes new.
fn drift(was: &Change, now: &Change) -> Vec<String> {
    let empty = (vec![], vec![]);
    let files: BTreeSet<&String> = was.keys().chain(now.keys()).collect();
    let mut lines = vec![];
    for f in &files {
        let (w, n) = (was.get(*f).unwrap_or(&empty), now.get(*f).unwrap_or(&empty));
        let lost: Vec<String> = [minus(&w.0, &n.0), minus(&w.1, &n.1).into_iter().map(|l| format!("-{l}")).collect()].concat();
        let gained: Vec<String> = [minus(&n.0, &w.0), minus(&n.1, &w.1).into_iter().map(|l| format!("-{l}")).collect()].concat();
        if lost.is_empty() && gained.is_empty() {
            continue;
        }
        let mut line = format!("its own change in {} differs after the rebase:", names(&[f.as_str()]));
        if let Some(l) = lost.first() {
            line.push_str(&format!(" {} line{} it no longer makes (`{}`)", lost.len(), if lost.len() == 1 { "" } else { "s" }, l.trim()));
        }
        if let Some(l) = gained.first() {
            line.push_str(&format!("{} {} it makes new (`{}`)", if lost.is_empty() { "" } else { ";" }, gained.len(), l.trim()));
        }
        lines.push(line);
    }
    if lines.len() > 3 {
        let more = lines.len() - 3;
        lines.truncate(3);
        lines.push(format!("and {more} more files differ"));
    }
    lines
}

/// The non-blank lines of `a` that `b` lacks, counting repeats.
fn minus(a: &[String], b: &[String]) -> Vec<String> {
    let mut left: HashMap<&str, usize> = HashMap::new();
    for l in b {
        *left.entry(l.trim()).or_default() += 1;
    }
    a.iter()
        .filter(|l| !l.trim().is_empty())
        .filter(|l| match left.get_mut(l.trim()) {
            Some(n) if *n > 0 => {
                *n -= 1;
                false
            }
            _ => true,
        })
        .cloned()
        .collect()
}

/// A commit of `tree` on `parent` with `commit`'s message and author, as
/// rebase makes one; the tree already there is `parent` itself, dropped.
fn commit_as(repo: &Path, commit: &str, tree: &str, parent: &str) -> Result<String, String> {
    if rev(repo, &format!("{parent}^{{tree}}")).as_deref() == Some(tree) {
        return Ok(parent.to_string());
    }
    let who = git_in(repo, &["log", "-1", "--format=%an%n%ae%n%ad", "--date=raw", commit])?;
    let mut who = who.lines();
    let (name, email, date) = (who.next().unwrap_or_default(), who.next().unwrap_or_default(), who.next().unwrap_or_default());
    let message = git_in(repo, &["log", "-1", "--format=%B", commit])?;
    let env = [("GIT_AUTHOR_NAME", name), ("GIT_AUTHOR_EMAIL", email), ("GIT_AUTHOR_DATE", date)];
    Ok(git_env(repo, &["commit-tree", tree, "-p", parent], &message, &env)?.trim().to_string())
}

/// Claude settles the conflicts `commit` has on `onto`, in ot's checkout
/// at `at`/tree: the pick is made there, conflict markers and all (zdiff3,
/// so it sees what both sides started from), with the file tools alone and
/// no shell. Of what it leaves, only the conflicted files are taken, and
/// only with no marker left and Claude saying they are settled; with
/// ot.lintcommand set, they are linted there first. The commit made of
/// that, as the commit was, and the lines to say about it (what Claude did,
/// how the lint went); else why not. The run's files stay in
/// `at`/runs/<iid>-<sha>.
fn settle(s: &mut Settling, onto: &str, commit: &str, files: &[String]) -> Result<(String, Vec<String>), String> {
    let (repo, at, mr, on) = (s.repo, s.at, s.mr, s.on.as_str());
    let short = short(repo, commit);
    (s.step)("settling", &format!("Claude is settling the conflicts in {} ({}'s {short})", names(files), bang(mr.iid)));
    let wt = at.join("tree");
    worktree::settle(repo, &wt, onto)?;
    let _ = git_in(&wt, &["-c", "merge.conflictStyle=zdiff3", "cherry-pick", "--no-commit", commit]);
    let run = at.join("runs").join(format!("{}-{short}", mr.iid));
    let _ = std::fs::remove_dir_all(&run);
    std::fs::create_dir_all(&run).map_err(|e| e.to_string())?;
    let keep = |name: &str, text: &str| std::fs::write(run.join(name), text).map_err(|e| e.to_string());
    keep("commit.patch", &git_in(repo, &["show", commit])?)?;
    let parent = format!("{commit}^");
    let mut base = vec!["diff", parent.as_str(), onto, "--"];
    base.extend(files.iter().map(String::as_str));
    keep("base.patch", &git_in(repo, &base)?)?;
    let listed: String = files.iter().map(|f| format!("- {f}\n")).collect();
    // Its first line names the run on ot's board.
    let prompt = format!(
        "Settle !{}'s commit {short} for ot's rebase.\n\n\
You are finishing a rebase. Commit {short}, \"{}\", of merge request !{}, is being put onto {on}, which has moved on since the commit was made, and some of it conflicts. The working directory is the repository at {on} with the commit applied, except where it conflicted. These files have conflict markers in them:\n{listed}\n\
In each conflict, between <<<<<<< and ||||||| is what {on} has now; between ||||||| and ======= is what both sides started from; between ======= and >>>>>>> is what the commit made of that.\n\
- The commit, its message and its whole diff: {run}/commit.patch\n\
- What changed in those files on the other side, from where the commit started to {on}: {run}/base.patch\n\n\
Settle every conflict: keep everything {on} has, and make the commit's change on top of it, as its author would have written it against this code. Edit only the files listed, and leave no conflict markers in them. You have no shell, so you cannot build or test: read the code around each conflict, and what it calls, to get it right.\n\n\
If a conflict needs a decision only the author can make, or you are not sure the result is right, leave it and answer resolved false, saying why.\n\n\
Answer resolved (whether every conflict is settled) and summary: one sentence for the author on what you did with them, or why not.\n",
        mr.iid,
        subject(repo, commit),
        mr.iid,
        run = run.display()
    );
    keep("prompt", &prompt)?;
    const SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"required":["resolved","summary"],"properties":{"resolved":{"type":"boolean"},"summary":{"type":"string"}}}"#;
    let answer = ask::claude("opus", &wt, &run, "Read,Grep,Glob,Edit,Write", SCHEMA, &prompt);
    let (resolved, summary) = match &answer {
        Ok(v) => (v["resolved"] == true, v["summary"].as_str().unwrap_or("it stopped without an answer").to_string()),
        Err(e) => (false, e.clone()),
    };
    let left = files.iter().any(|f| std::fs::read_to_string(wt.join(f)).is_ok_and(|t| t.lines().any(marker)));
    let made = if resolved && !left {
        let mut add = vec!["add", "--"];
        add.extend(files.iter().map(String::as_str));
        git_in(&wt, &add)
            .and_then(|_| lint_settled(&wt, onto, &run, s.step))
            .and_then(|lint| {
                let tree = git_in(&wt, &["write-tree"])?;
                Ok((commit_as(repo, commit, tree.trim(), onto)?, lint))
            })
    } else {
        Err(format!("Claude could not settle it: {summary}"))
    };
    let _ = git_in(&wt, &["reset", "--quiet", "--hard"]);
    let (new, lint) = made?;
    let mut lines = vec![format!("Claude settled {short} in {}: {summary}", names(files))];
    lines.extend(lint);
    Ok((new, lines))
}

/// A line that is a conflict marker.
fn marker(l: &str) -> bool {
    l == "======="
        || ["<<<<<<<", ">>>>>>>", "|||||||"]
            .iter()
            .any(|m| l == *m || l.starts_with(&format!("{m} ")))
}

/// ot.lintcommand run over what Claude settled, staged in `wt`, with
/// GIT_BASE_REF as `onto` and OT_CHANGED_FILES the files the commit
/// changes. Claude's edits outside the conflicted files go first, as they
/// would have anyway, so what lint fixes is all that is staged after it. A
/// line on how it went, its output kept in the run; nothing with no command.
fn lint_settled(wt: &Path, onto: &str, run: &Path, step: &mut dyn FnMut(&'static str, &str)) -> Result<Option<String>, String> {
    let Some(cmd) = git_in(wt, &["config", "--get", "ot.lintcommand"]).ok().map(|c| c.trim().to_string()).filter(|c| !c.is_empty()) else {
        return Ok(None);
    };
    step("linting", "linting what Claude settled");
    git_in(wt, &["checkout", "--quiet", "--", "."])?;
    let changed = run.join("changed-files");
    std::fs::write(&changed, git_in(wt, &["diff", "--cached", "--name-only", onto])?).map_err(|e| e.to_string())?;
    let log = run.join("lint.log");
    let out = Command::new("sh")
        .args(["-c", &cmd])
        .current_dir(wt)
        .env("GIT_BASE_REF", onto)
        .env("OT_CHANGED_FILES", &changed)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("lint: {e}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let _ = std::fs::write(&log, &text);
    git_in(wt, &["add", "-u"])?;
    Ok(Some(if out.status.success() {
        "lint passed".to_string()
    } else {
        let why = text
            .lines()
            .find(|l| l.to_lowercase().contains("error"))
            .or(text.lines().last())
            .unwrap_or_default();
        format!("lint failed: {why} (all of it in {})", log.display())
    }))
}

/// Your branch has nothing of its own, so it follows GitLab's with nothing
/// lost: its commit is in GitLab's, or is one GitLab's branch was at before
/// it was rewritten there (a push from another checkout leaves yours on the
/// old copy). Not when yours has commits past GitLab's, or has been at
/// GitLab's commit and moved off it: then the difference is yours.
fn behind(repo: &Path, branch: &str, yours: &str, theirs: &str) -> bool {
    if ancestor(repo, yours, theirs) {
        return true;
    }
    if ancestor(repo, theirs, yours) {
        return false;
    }
    was_at(repo, &format!("refs/remotes/origin/{branch}"), yours) && !was_at(repo, &format!("refs/heads/{branch}"), theirs)
}

/// That ref has been at that commit, by its reflog here.
fn was_at(repo: &Path, r: &str, commit: &str) -> bool {
    git_in(repo, &["log", "-g", "--format=%H", r]).is_ok_and(|l| l.lines().any(|c| c == commit))
}

fn ancestor(repo: &Path, a: &str, b: &str) -> bool {
    git_in(repo, &["merge-base", "--is-ancestor", a, b]).is_ok()
}

fn fork_point(repo: &Path, upstream: &str, start: &str) -> Option<String> {
    git_in(repo, &["merge-base", "--fork-point", upstream, start])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn short(repo: &Path, c: &str) -> String {
    git_in(repo, &["rev-parse", "--short", c]).map(|s| s.trim().to_string()).unwrap_or_else(|_| c.to_string())
}

fn subject(repo: &Path, c: &str) -> String {
    git_in(repo, &["log", "-1", "--format=%s", c]).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn count(repo: &Path, from: &str, to: &str) -> usize {
    git_in(repo, &["rev-list", "--count", &format!("{from}..{to}")]).ok().and_then(|n| n.trim().parse().ok()).unwrap_or(0)
}

/// Paths by their last part, up to three and "and N more".
fn names<S: AsRef<str>>(files: &[S]) -> String {
    let last: Vec<&str> = files.iter().map(|f| f.as_ref().rsplit('/').next().unwrap_or_default()).collect();
    let mut s = last[..last.len().min(3)].join(", ");
    if last.len() > 3 {
        s.push_str(&format!(" and {} more", last.len() - 3));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use crate::model::fixture::mr;

    fn sh(dir: &Path, cmd: &str) -> String {
        let out = Command::new("sh").arg("-c").arg(cmd).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "{cmd}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A bare origin with main and feat (two commits in b), a clone of it
    /// with both branches, and main moved on at origin by `main_more`.
    fn repos(name: &str, main_more: &str) -> (PathBuf, PathBuf) {
        let d = std::env::temp_dir().join(format!("ot-test-rebase-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let g = "git -c user.name=t -c user.email=t@t -c commit.gpgsign=false";
        sh(&d, "git init -q --bare origin");
        sh(&d, &format!("git clone -q origin repo 2>/dev/null; cd repo && git switch -q -c main && echo a > a && git add a && {g} commit -q -m one && git push -q origin main"));
        let repo = d.join("repo");
        sh(&repo, &format!("git switch -q -c feat && echo b1 > b && git add b && {g} commit -q -m 'b one' && echo b2 >> b && git add b && {g} commit -q -m 'b two' && git push -q origin feat"));
        sh(&repo, &format!("git switch -q main && {main_more} && git push -q origin main && git switch -q feat"));
        (d, repo)
    }

    #[test]
    fn a_branch_is_replayed_onto_main_and_pushed_with_its_checkout_following() {
        let g = "git -c user.name=t -c user.email=t@t -c commit.gpgsign=false";
        let (d, repo) = repos("plain", &format!("echo c > c && git add c && {g} commit -q -m two"));
        let layers = [mr(7, "me", "feat", "main")];
        let mut steps = vec![];
        let r = prepare(&repo, &layers, None, &d.join("settle"), &mut |w, _| steps.push(w)).unwrap();
        // "waiting" first, when another test's rebase holds ot's one at a time.
        steps.retain(|w| *w != "waiting");
        assert_eq!(steps, ["fetching", "rebasing"]);
        assert_eq!(r.said[1].text, "2 commits onto main, 1 new there");
        assert_eq!(r.question, "rebase it?");
        let main = sh(&repo, "git rev-parse origin/main");
        assert_eq!(sh(&repo, &format!("git rev-parse {}~2", r.layers[0].new)), main);
        // Nothing moved yet.
        assert_eq!(sh(&repo, "git rev-parse feat"), r.layers[0].local.clone().unwrap());
        let left = push(&repo, &r, &mut |_, _| {}).unwrap();
        assert!(left.is_empty(), "{left:?}");
        assert_eq!(sh(&d.join("origin"), "git rev-parse feat"), r.layers[0].new);
        assert_eq!(sh(&repo, "git rev-parse HEAD"), r.layers[0].new);
        assert_eq!(sh(&repo, "git log --format=%s -3"), "b two\nb one\ntwo");
        // Again: already there, nothing to ask.
        let again = prepare(&repo, &layers, None, &d.join("settle"), &mut |_, _| {}).unwrap();
        assert!(!again.moves());
        assert_eq!(again.said[0].text, "!7 already in place on main");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_commit_main_already_has_is_dropped() {
        let g = "git -c user.name=t -c user.email=t@t -c commit.gpgsign=false";
        // main's commit is not the same patch (it brings c too), so rev-list
        // keeps b one; put onto main it changes nothing, and is dropped.
        let (d, repo) = repos("dropped", &format!("echo b1 > b && echo c > c && git add b c && {g} commit -q -m 'b one and c'"));
        let r = prepare(&repo, &[mr(8, "me", "feat", "main")], None, &d.join("settle"), &mut |_, _| {}).unwrap();
        assert_eq!(r.said[1].text, "1 commit onto main, 1 new there; 1 more dropped, as main has its change already");
        assert_eq!(sh(&repo, &format!("git log --format=%s -2 {}", r.layers[0].new)), "b two\nb one and c");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn uncommitted_changes_in_a_file_the_rebase_touches_stop_it() {
        let g = "git -c user.name=t -c user.email=t@t -c commit.gpgsign=false";
        // main changed a, so the rebase brings a new a to feat's checkout,
        // where a has edits of yours.
        let (d, repo) = repos("dirty", &format!("echo a2 > a && git add a && {g} commit -q -m two"));
        std::fs::write(repo.join("a"), "mine\n").unwrap();
        let e = prepare(&repo, &[mr(9, "me", "feat", "main")], None, &d.join("settle"), &mut |_, _| {}).unwrap_err();
        assert!(e.starts_with("uncommitted changes in") && e.contains("(a)"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_stack_moves_as_a_whole_each_layer_onto_the_one_below_as_rebased() {
        let g = "git -c user.name=t -c user.email=t@t -c commit.gpgsign=false";
        let (d, repo) = repos("stack", &format!("echo c > c && git add c && {g} commit -q -m two"));
        sh(&repo, &format!("git switch -q -c feat2 feat && echo d > d && git add d && {g} commit -q -m 'd one' && git push -q origin feat2 && git switch -q feat"));
        let layers = [mr(1, "me", "feat", "main"), mr(2, "me", "feat2", "feat")];
        let r = prepare(&repo, &layers, None, &d.join("settle"), &mut |_, _| {}).unwrap();
        let texts: Vec<&str> = r.said.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts[1], "2 commits onto main, 1 new there");
        assert_eq!(texts[3], "1 commit onto !1");
        assert_eq!(r.question, "rebase the stack?");
        // feat2's one commit sits on feat's new tip, which sits on main.
        assert_eq!(sh(&repo, &format!("git rev-parse {}^", r.layers[1].new)), r.layers[0].new);
        assert_eq!(sh(&repo, &format!("git rev-parse {}~2", r.layers[0].new)), sh(&repo, "git rev-parse origin/main"));
        push(&repo, &r, &mut |_, _| {}).unwrap();
        assert_eq!(sh(&d.join("origin"), "git rev-parse feat2"), r.layers[1].new);
        assert_eq!(sh(&repo, "git rev-parse feat2"), r.layers[1].new);
        assert_eq!(sh(&repo, "git log --format=%s -4 feat2"), "d one\nb two\nb one\ntwo");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_layer_whose_mr_below_merged_by_squash_takes_only_its_own_commits() {
        let g = "git -c user.name=t -c user.email=t@t -c commit.gpgsign=false";
        let (d, repo) = repos("landed", "true");
        sh(&repo, &format!("git switch -q -c feat2 feat && echo d > d && git add d && {g} commit -q -m 'd one' && git push -q origin feat2"));
        // GitLab squashes feat's two commits into one on main, keeps its
        // head as the MR's, and deletes the branch.
        let o = d.join("origin");
        sh(&o, "git update-ref refs/merge-requests/1/head feat");
        sh(&repo, &format!("git switch -q main && git merge -q --squash feat && {g} commit -q -m 'b, squashed' && git push -q origin main && git push -q origin :feat && git switch -q feat2"));
        let r = prepare(&repo, &[mr(2, "me", "feat2", "main")], Some(1), &d.join("settle"), &mut |_, _| {}).unwrap();
        assert_eq!(r.said[1].text, "1 commit onto main, 1 new there, leaving out !1's, which merged");
        assert_eq!((r.left_out, r.settled), (Some(1), 0), "so a landing pushes it without asking");
        assert_eq!(sh(&repo, &format!("git rev-parse {}^", r.layers[0].new)), sh(&repo, "git rev-parse origin/main"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_change_held_in_a_merge_commit_that_the_replay_leaves_out_is_said_and_asked_about() {
        let g = "git -c user.name=t -c user.email=t@t -c commit.gpgsign=false";
        let (d, repo) = repos("merged", &format!("echo c > c && git add c && {g} commit -q -m two"));
        // feat2 merges feat's newer commit, and the merge itself brings e,
        // as a conflict settled in a merge would; replay skips merges.
        sh(&repo, &format!("git switch -q -c feat2 feat && echo d > d && git add d && {g} commit -q -m 'd one' && git switch -q feat && echo b3 >> b && git add b && {g} commit -q -m 'b three' && git push -q origin feat"));
        sh(&repo, &format!("git switch -q feat2 && git merge -q --no-commit --no-ff feat && echo settled > e && git add e && {g} commit -q -m 'merge feat' && git push -q origin feat2 && git switch -q feat"));
        let layers = [mr(1, "me", "feat", "main"), mr(2, "me", "feat2", "feat")];
        let r = prepare(&repo, &layers, None, &d.join("settle"), &mut |_, _| {}).unwrap();
        assert_eq!(r.changed, [2]);
        let texts: Vec<&str> = r.said.iter().map(|s| s.text.as_str()).collect();
        assert!(texts.contains(&"its own change in e differs after the rebase: 1 line it no longer makes (`settled`)"), "{texts:?}");
        assert_eq!(r.question, "rebase the stack, though it changes what !2 does?");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn markers_are_known_by_their_lines() {
        assert!(marker("<<<<<<< HEAD"));
        assert!(marker("======="));
        assert!(marker(">>>>>>> abc (b one)"));
        assert!(!marker("======= not a marker"));
        assert!(!marker("<<<<<<<< eight"));
        assert_eq!(names(&["src/a.ts", "src/b.ts", "c.ts", "d.ts", "e.ts"]), "a.ts, b.ts, c.ts and 2 more");
    }
}


