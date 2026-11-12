//! What this checkout knows without the network.
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

/// One git command's text output.
fn git(args: &[&str], stdin: &str) -> Result<String, String> {
    git_bytes(args, stdin, &[]).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// The same, in another checkout, fed `stdin`, with `env` set for it (an
/// author for commit-tree).
pub fn git_env(dir: &std::path::Path, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> Result<String, String> {
    let dir = dir.to_string_lossy();
    let mut all = vec!["-C", dir.as_ref()];
    all.extend(args);
    git_bytes(&all, stdin, env).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// The same, run in another checkout of a repo.
pub fn git_in(dir: &std::path::Path, args: &[&str]) -> Result<String, String> {
    git_env(dir, args, "", &[])
}

/// A checkout of this repo, and the branch it is on now; empty when its
/// HEAD is detached.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Worktree {
    pub path: std::path::PathBuf,
    pub branch: String,
}

/// Every checkout of this repo, the main one first.
pub fn worktrees() -> Vec<Worktree> {
    checkouts(&git(&["worktree", "list", "--porcelain"], "").unwrap_or_default())
}

/// What `git worktree list --porcelain` lists.
fn checkouts(porcelain: &str) -> Vec<Worktree> {
    let mut out: Vec<Worktree> = vec![];
    for l in porcelain.lines() {
        if let Some(path) = l.strip_prefix("worktree ") {
            out.push(Worktree { path: path.into(), branch: String::new() });
        } else if let (Some(b), Some(w)) = (l.strip_prefix("branch refs/heads/"), out.last_mut()) {
            w.branch = b.to_string();
        }
    }
    out
}

/// The branch checked out here; empty when HEAD is detached.
pub fn branch() -> String {
    git(&["symbolic-ref", "--short", "-q", "HEAD"], "")
        .map(|b| b.trim().to_string())
        .unwrap_or_default()
}

/// The commit a ref names, if it names one.
pub fn rev(repo: &std::path::Path, r: &str) -> Option<String> {
    git_in(repo, &["rev-parse", "--verify", "-q", r]).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// The checkout that has `branch` checked out, if one does.
pub fn checkout_of(repo: &std::path::Path, branch: &str) -> Option<std::path::PathBuf> {
    let list = git_in(repo, &["worktree", "list", "--porcelain"]).ok()?;
    let want = format!("branch refs/heads/{branch}");
    list.split("\n\n").find_map(|block| {
        let mut lines = block.lines();
        let path = lines.next()?.strip_prefix("worktree ")?;
        lines.any(|l| l == want).then(|| std::path::PathBuf::from(path))
    })
}

/// The local branch moved from `from` to `to`, checked out or not; else
/// why not.
pub fn follow(repo: &std::path::Path, branch: &str, from: &str, to: &str) -> Option<String> {
    match checkout_of(repo, branch) {
        None => git_in(repo, &["update-ref", &format!("refs/heads/{branch}"), to, from])
            .err()
            .map(|_| format!("your local {branch} moved meanwhile, so it was left alone")),
        Some(wt) if rev(&wt, "HEAD").as_deref() != Some(from) => {
            Some(format!("{branch} in {} moved meanwhile, so it was left alone", wt.display()))
        }
        Some(wt) => git_in(&wt, &["reset", "--quiet", "--keep", to]).err().map(|_| {
            format!("uncommitted changes in {} are in files that changed, so {branch} there was left alone", wt.display())
        }),
    }
}

/// `commit` picked onto `onto` in memory: the tree, or the files that
/// conflict.
pub fn merge_tree(repo: &std::path::Path, onto: &str, commit: &str) -> Result<String, Vec<String>> {
    let base = format!("--merge-base={commit}^");
    let out = Command::new("git")
        .args(["merge-tree", "--write-tree", "--name-only", &base, onto, commit])
        .current_dir(repo)
        .stdin(Stdio::null())
        .output()
        .map_err(|_| vec![])?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let tree = lines.next().unwrap_or_default().to_string();
    if out.status.success() {
        return Ok(tree);
    }
    // A conflict: the tree, the files, a blank line, then the messages.
    Err(lines.take_while(|l| !l.is_empty()).map(str::to_string).collect())
}

/// For each (head, target branch): seconds from where head split off
/// origin's target to that target's tip, which is how CI's staleness check
/// measures a base. None when the head or the target isn't here.
pub fn split_ages(pairs: &[(&str, &str)]) -> Vec<Option<i64>> {
    // Which heads and targets are here, in one call: a sha each, or none.
    let ask: String = pairs
        .iter()
        .map(|(head, target)| format!("{head}\nrefs/remotes/origin/{target}\n"))
        .collect();
    let Ok(found) = git(&["cat-file", "--batch-check=%(objectname)"], &ask) else {
        return vec![None; pairs.len()];
    };
    let shas: Vec<Option<&str>> = found
        .lines()
        .map(|l| (!l.ends_with(" missing")).then_some(l))
        .collect();
    // One merge-base each, all running at once.
    let bases: Vec<Option<(std::process::Child, &str)>> = (0..pairs.len())
        .map(|i| {
            let (head, tip) = (
                shas.get(2 * i).copied().flatten()?,
                shas.get(2 * i + 1).copied().flatten()?,
            );
            let child = Command::new("git")
                .args(["merge-base", head, tip])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok()?;
            Some((child, tip))
        })
        .collect();
    let bases: Vec<Option<(String, &str)>> = bases
        .into_iter()
        .map(|b| {
            let (child, tip) = b?;
            let out = child.wait_with_output().ok()?;
            let base = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!base.is_empty()).then_some((base, tip))
        })
        .collect();
    let mut want: Vec<&str> = bases
        .iter()
        .flatten()
        .flat_map(|(b, t)| [b.as_str(), *t])
        .collect();
    want.sort();
    want.dedup();
    if want.is_empty() {
        return vec![None; pairs.len()];
    }
    let mut args = vec!["show", "-s", "--format=%H %ct"];
    args.extend(&want);
    let times: HashMap<String, i64> = git(&args, "")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            l.split_once(' ')
                .and_then(|(h, t)| Some((h.to_string(), t.parse().ok()?)))
        })
        .collect();
    bases
        .iter()
        .map(|b| {
            b.as_ref()
                .and_then(|(base, tip)| Some(times.get(*tip)? - times.get(base)?))
        })
        .collect()
}

/// How far each merged commit has got: live once production's commit has
/// it, deploying while a running deploy's does, else merged. `main` is
/// fetched first when any of them isn't here, as a new deploy's isn't.
pub fn reach(repo: &std::path::Path, prod: &crate::model::Production, main: &str, commits: &[&str]) -> Vec<crate::model::Reach> {
    use crate::model::Reach;
    let ask: String = prod.live.iter().chain(&prod.deploying).map(String::as_str).chain(commits.iter().copied()).map(|s| format!("{s}\n")).collect();
    let here = git_env(repo, &["cat-file", "--batch-check=%(objecttype)"], &ask, &[]);
    if !here.is_ok_and(|out| out.lines().all(|l| l == "commit")) {
        let _ = git_in(repo, &["fetch", "--quiet", "--no-tags", "origin", main]);
    }
    let has = |tip: &str, c: &str| git_in(repo, &["merge-base", "--is-ancestor", c, tip]).is_ok();
    commits
        .iter()
        .map(|c| {
            if prod.live.as_deref().is_some_and(|l| has(l, c)) {
                Reach::Live
            } else if prod.deploying.iter().any(|d| has(d, c)) {
                Reach::Deploying
            } else {
                Reach::Merged
            }
        })
        .collect()
}

/// Your local `main` brought up to origin's, as lazygit does: an editor or a
/// `git switch -c` on the checkout that sits on main reads the local one,
/// and nothing else moves it. Only ever a fast-forward, so a main with
/// commits of its own stays put, as does one checked out with tracked changes.
pub fn forward(repo: &std::path::Path, main: &str) {
    let theirs = format!("refs/remotes/origin/{main}");
    let mirror = format!("+refs/heads/{main}:{theirs}");
    if git_in(repo, &["fetch", "--quiet", "--no-tags", "--no-write-fetch-head", "origin", &mirror]).is_err() {
        return;
    }
    // Both refuse anything but a fast-forward; fetch also refuses a main
    // being rebased or bisected anywhere.
    match checkout_of(repo, main) {
        None => {
            let _ = git_in(repo, &["fetch", "--quiet", "--no-write-fetch-head", ".", &format!("{theirs}:refs/heads/{main}")]);
        }
        Some(wt) => {
            let changed = git_in(&wt, &["--no-optional-locks", "status", "--porcelain", "--untracked-files=no"]);
            if changed.is_ok_and(|c| c.is_empty()) {
                let _ = git_in(&wt, &["merge", "--quiet", "--ff-only", &theirs]);
            }
        }
    }
}

/// For each of `heads`, the merged MR in `merged` (iid, head) whose own
/// commits it still has: it sat on that MR and hasn't been rebased past it.
/// Only a merge that left those commits off origin's `main`, as a squash
/// does, counts; after a merge commit main has them too and there is
/// nothing to leave out. None where it has none, or a commit isn't here.
pub fn carries(repo: &std::path::Path, main: &str, merged: &[(u64, &str)], heads: &[&str]) -> Vec<Option<u64>> {
    // Fails too for a commit that isn't here.
    let within = |a: &str, b: &str| !a.is_empty() && git_in(repo, &["merge-base", "--is-ancestor", a, b]).is_ok();
    let theirs = format!("refs/remotes/origin/{main}");
    let off: Vec<&(u64, &str)> = merged.iter().filter(|(_, h)| !within(h, &theirs)).collect();
    heads
        .iter()
        .map(|&h| off.iter().find(|(_, m)| within(m, h)).map(|(i, _)| *i))
        .collect()
}

/// Whether every one of these commits is here.
pub fn have(shas: &[&str]) -> bool {
    let ask: String = shas.iter().map(|s| format!("{s}\n")).collect();
    git(&["cat-file", "--batch-check=%(objecttype)"], &ask)
        .is_ok_and(|out| out.lines().all(|l| l == "commit"))
}

/// An MR's head from origin, for a branch never fetched here. It writes no
/// ref, only the objects.
pub fn fetch_mr(iid: u64) -> Result<(), String> {
    git(
        &[
            "fetch",
            "--quiet",
            "--no-write-fetch-head",
            "origin",
            &format!("refs/merge-requests/{iid}/head"),
        ],
        "",
    )
    .map(|_| ())
}

/// Files bigger than this (2 << 20 is 2 MiB) aren't read: the review says
/// it can't show them.
const MOST_BYTES: usize = 2 << 20;

/// What changed between two commits and both sides of every changed file,
/// in three git calls however many files there are.
pub fn sides(base: &str, head: &str) -> Result<crate::review::Sides, String> {
    let range = format!("{base}..{head}");
    let diff = std::thread::scope(|s| {
        let diff = s.spawn(|| {
            git(
                &[
                    "diff",
                    "-U0",
                    "--no-renames",
                    "--no-color",
                    "--no-ext-diff",
                    "--src-prefix=a/",
                    "--dst-prefix=b/",
                    &range,
                ],
                "",
            )
        });
        let status = git(&["diff", "--name-status", "--no-renames", &range], "");
        (
            status,
            diff.join().unwrap_or_else(|_| Err("git diff".into())),
        )
    });
    let (status, diff) = (diff.0?, diff.1?);
    // Every side of every file, asked of one `git cat-file --batch`, which
    // answers in the order asked: a "<sha> <type> <size>" line, the bytes,
    // a newline.
    let mut ask = String::new();
    let mut asked: Vec<(bool, &str)> = vec![];
    for (letter, path) in status.lines().filter_map(|l| l.split_once('\t')) {
        if letter != "A" {
            ask.push_str(&format!("{base}:{path}\n"));
            asked.push((false, path));
        }
        if letter != "D" {
            ask.push_str(&format!("{head}:{path}\n"));
            asked.push((true, path));
        }
    }
    let out = git_bytes(&["cat-file", "--batch"], &ask, &[])?;
    let (mut old, mut new) = (HashMap::new(), HashMap::new());
    let mut at = 0;
    for (is_new, path) in asked {
        let Some(nl) = out[at..].iter().position(|&b| b == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&out[at..at + nl]).into_owned();
        at += nl + 1;
        // "<object> missing" when a side isn't there, with no bytes after.
        let [_sha, kind, size] = header.split(' ').collect::<Vec<_>>()[..] else {
            continue;
        };
        let size: usize = size.parse().unwrap_or(0);
        let body = &out[at..(at + size).min(out.len())];
        at += size + 1;
        // Text only: a NUL in the first 8000 bytes means binary, as git
        // itself decides.
        let binary = body[..body.len().min(8000)].contains(&0);
        if kind == "blob" && size <= MOST_BYTES && !binary {
            let text = String::from_utf8_lossy(body).into_owned();
            if is_new { &mut new } else { &mut old }.insert(path.to_string(), text);
        }
    }
    Ok(crate::review::Sides {
        status,
        diff,
        old,
        new,
    })
}

/// One git command's output; stdin is fed from another thread so a big
/// answer can't wedge the pipe.
fn git_bytes(args: &[&str], stdin: &str, env: &[(&str, &str)]) -> Result<Vec<u8>, String> {
    // Its own process group, so closing the terminal or ctrl-c can't kill a
    // git that is halfway through rewriting the review checkout.
    let mut child = Command::new("git")
        .args(args)
        .envs(env.iter().copied())
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git: {e}"))?;
    let mut pipe = child.stdin.take().expect("piped stdin");
    let input = stdin.to_string();
    let feed = std::thread::spawn(move || pipe.write_all(input.as_bytes()));
    let out = child.wait_with_output().map_err(|e| format!("git: {e}"))?;
    let _ = feed.join();
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Production, Reach};
    use std::path::Path;

    fn sh(dir: &Path, cmd: &str) -> String {
        let out = Command::new("sh").arg("-c").arg(cmd).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "{cmd}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn each_checkout_is_on_its_branch_or_none_when_detached() {
        let listed = "worktree /r\nHEAD a1\nbranch refs/heads/main\n\nworktree /slots/ap-9\nHEAD b2\ndetached\n\nworktree /slots/ap-8\nHEAD c3\nbranch refs/heads/sam/ap-8-x\n";
        let got: Vec<(String, String)> =
            checkouts(listed).into_iter().map(|w| (w.path.to_string_lossy().into(), w.branch)).collect();
        let want = [("/r", "main"), ("/slots/ap-9", ""), ("/slots/ap-8", "sam/ap-8-x")];
        assert_eq!(got, want.map(|(p, b)| (p.to_string(), b.to_string())));
    }

    #[test]
    fn a_merged_commit_is_live_once_production_runs_one_that_has_it() {
        let d = std::env::temp_dir().join(format!("ot-test-reach-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let g = "git -c user.name=t -c user.email=t@t -c commit.gpgsign=false";
        sh(&d, "git init -q --bare origin");
        sh(&d, &format!("git clone -q origin repo 2>/dev/null; cd repo && git switch -q -c main && {g} commit -q --allow-empty -m one && git push -q origin main"));
        // Main moves on at origin, past what this checkout has fetched.
        sh(&d, &format!("git clone -q -b main origin other && cd other && {g} commit -q --allow-empty -m two && {g} commit -q --allow-empty -m three && {g} commit -q --allow-empty -m four && git push -q origin main"));
        let at = |n: u8| sh(&d.join("other"), &format!("git rev-parse HEAD~{n}"));
        let (two, three, four) = (at(2), at(1), at(0));
        let prod = Production {
            live: Some(two.clone()),
            deploying: vec![three.clone()],
        };
        let got = reach(&d.join("repo"), &prod, "main", &[&two, &three, &four]);
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(got, [Reach::Live, Reach::Deploying, Reach::Merged]);
    }

    #[test]
    fn local_main_follows_origin_only_by_fast_forward() {
        let d = std::env::temp_dir().join(format!("ot-test-forward-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let g = "git -c user.name=t -c user.email=t@t -c commit.gpgsign=false";
        sh(&d, "git init -q --bare origin");
        sh(&d, &format!("git clone -q origin repo 2>/dev/null; cd repo && git switch -q -c main && echo a > f && git add f && {g} commit -q -m one && git push -q origin main && git switch -q -c work"));
        sh(&d, "git clone -q -b main origin other");
        let land = |msg: &str| sh(&d.join("other"), &format!("{g} commit -q --allow-empty -m {msg} && git push -q origin main"));
        let (repo, wt) = (d.join("repo"), d.join("wt"));
        let main = || sh(&repo, "git log -1 --format=%s main");

        land("two");
        forward(&repo, "main");
        assert_eq!(main(), "two", "not checked out");

        sh(&repo, "git worktree add -q ../wt main && echo b > ../wt/f");
        land("three");
        forward(&repo, "main");
        assert_eq!(main(), "two", "checked out with a change");
        sh(&wt, "git checkout -q -- f");
        forward(&repo, "main");
        assert_eq!(main(), "three", "checked out and clean");

        sh(&wt, &format!("{g} commit -q --allow-empty -m mine"));
        land("four");
        forward(&repo, "main");
        let got = main();
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(got, "mine", "commits of its own");
    }

    #[test]
    fn a_branch_carries_a_squashed_mr_until_rebased_past_it() {
        let d = std::env::temp_dir().join(format!("ot-test-carries-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let g = "git -c user.name=t -c user.email=t@t -c commit.gpgsign=false";
        sh(&d, "git init -q --bare origin");
        sh(&d, &format!("git clone -q origin repo 2>/dev/null; cd repo && git switch -q -c main && echo a > a && git add a && {g} commit -q -m one && git push -q origin main"));
        let repo = d.join("repo");
        let commit = |name: &str| sh(&repo, &format!("echo {name} > {name} && git add {name} && {g} commit -q -m {name} && git rev-parse HEAD"));
        // !1 with two commits, and !2 and !3 on it.
        sh(&repo, "git switch -q -c one");
        commit("b");
        let one = commit("c");
        sh(&repo, "git switch -q -c two");
        let two = commit("d");
        sh(&repo, "git switch -q -c three one");
        let three = commit("e");
        // !1 squashed onto main.
        sh(&repo, &format!("git switch -q main && git merge -q --squash one && {g} commit -q -m 'one, squashed' && git push -q origin main"));
        let merged = [(1, one.as_str())];
        assert_eq!(carries(&repo, "main", &merged, &[&two, &three, "f00d"]), [Some(1), Some(1), None]);
        // !2 rebased past it.
        sh(&repo, &format!("git rebase -q --onto main one two 2>/dev/null"));
        let two = sh(&repo, "git rev-parse two");
        assert_eq!(carries(&repo, "main", &merged, &[&two, &three]), [None, Some(1)]);
        // A merge commit leaves main with !4's own commits: nothing to leave out.
        sh(&repo, "git switch -q -c four main");
        let four = commit("f");
        sh(&repo, "git switch -q -c five");
        let five = commit("g");
        sh(&repo, &format!("git switch -q main && {g} merge -q --no-ff four -m 'merge four' && git push -q origin main && git fetch -q origin"));
        let got = carries(&repo, "main", &[(4, four.as_str())], &[&five]);
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(got, [None]);
    }
}
