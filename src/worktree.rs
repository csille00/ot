//! ot's own checkout for reviews: one git worktree, detached at the head of
//! the MR under review, so nvim opens real files there and your LSP works
//! on them. Moving it rewrites only the files that differ between two MRs.
//! Imports resolve through the main checkout's node_modules, mirrored in.
//! On your own MR, what you change there is pushed from it. Claude gets
//! one of its own to settle a rebase's conflicts in.
use crate::git::{follow, git_in, merge_tree, rev};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The repo's main checkout, and whether `at` is already a worktree of it.
fn place(repo: &Path, at: &Path) -> Result<(PathBuf, bool), String> {
    let list = git_in(repo, &["worktree", "list", "--porcelain"])?;
    let main = list
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("worktree "))
        .map(PathBuf::from)
        .ok_or("no main checkout")?;
    // git names a path with its links resolved (/var is /private/var).
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let ours = list
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .any(|p| real(Path::new(p)) == real(at));
    Ok((main, ours))
}

/// A new worktree at `at`, detached at `head`.
fn add(repo: &Path, at: &Path, head: &str) -> Result<(), String> {
    let path = at.to_string_lossy();
    if at.exists() {
        return Err(format!("{path} is in the way of ot's checkout"));
    }
    std::fs::create_dir_all(at.parent().unwrap_or(at)).map_err(|e| e.to_string())?;
    git_in(repo, &["worktree", "add", "--quiet", "--detach", &path, head]).map(|_| ())
}

/// Puts the review worktree at `at` onto `head`, making it the first time.
/// `repo` is any checkout of the repo. A worktree holding edits is left as
/// it is, since moving it would throw them away; one already at `head`
/// keeps them, so you can come back to them. One move at a time is the
/// caller's to keep, along with deciding whether a move is still wanted.
pub fn review(repo: &Path, at: &Path, head: &str) -> Result<(), String> {
    let (main, ours) = place(repo, at)?;
    let path = at.to_string_lossy();
    if !ours {
        add(repo, at, head)?;
    } else {
        recover(at);
        // A mark left by a move that didn't finish says the checkout's changes
        // are that move's, not yours: they go. One that finished after ot
        // went away left the checkout clean at the head it names, and needs
        // nothing. With no mark, changes are yours, and it stays put.
        let moving = moving_file(at);
        let cut_short = std::fs::read_to_string(&moving)
            .ok()
            .filter(|was| !settled(at, was.trim()));
        if cut_short.is_some() {
            git_in(at, &["checkout", "--quiet", "--force", "--detach", head])?;
        } else if !is_at(at, head) {
            let edits = git_in(at, &["status", "--porcelain", "--untracked-files=no"])?;
            if !edits.trim().is_empty() {
                let what = change(at).map(|c| format!(" on {} \"{}\" ({})", short(&c.on), c.subject, c.files.join(", "))).unwrap_or_default();
                return Err(format!(
                    "your change{what} holds the review checkout, so it stays where it is; leave the review and ot asks what to do with it: {path}"
                ));
            }
            std::fs::write(&moving, head).map_err(|e| e.to_string())?;
            git_in(at, &["switch", "--quiet", "--detach", head])?;
        }
        let _ = std::fs::remove_file(&moving);
    }
    mirror(&main.join("node_modules"), &at.join("node_modules"))
        .map_err(|e| format!("node_modules: {e}"))
}

/// Claude's checkout at `at`, clean and detached at `head`, made the first
/// time. Only Claude writes there, so whatever an earlier run left goes.
pub fn settle(repo: &Path, at: &Path, head: &str) -> Result<(), String> {
    let (main, ours) = place(repo, at)?;
    if !ours {
        add(repo, at, head)?;
    } else {
        git_in(at, &["reset", "--quiet", "--hard"])?;
        git_in(at, &["clean", "-fdq"])?;
        git_in(at, &["switch", "--quiet", "--detach", head])?;
    }
    mirror(&main.join("node_modules"), &at.join("node_modules"))
        .map_err(|e| format!("node_modules: {e}"))
}

/// One thing at a time in the review checkout: a move, a push, a look at
/// what it holds. Callers hold it around the whole of what they do there.
pub static CHECKOUT: Mutex<()> = Mutex::new(());

/// What the review checkout holds that GitLab doesn't: your edits, on the
/// commit they were made on.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub on: String,
    /// That commit's subject, to name what an amend makes again.
    pub subject: String,
    /// What they touch, as git names them.
    pub files: Vec<String>,
}

/// Your change in the review checkout at `at`, if it holds one.
pub fn change(at: &Path) -> Option<Change> {
    recover(at);
    let files = edits(at);
    if files.is_empty() {
        return None;
    }
    let on = rev(at, "HEAD")?;
    Some(Change { subject: subject(at, &on), on, files })
}

/// Every file the checkout has that its commit doesn't: what `add -A`
/// would put in a commit.
fn edits(at: &Path) -> Vec<String> {
    let status = git_in(at, &["status", "--porcelain", "--no-renames"]).unwrap_or_default();
    status.lines().filter_map(|l| l.get(3..)).map(String::from).collect()
}

/// What your change goes out as.
pub enum Commit {
    /// Folded into the commit it was made on, that commit's message kept.
    Amend,
    /// A new commit on it, with this message.
    New(String),
}

/// Your change in the review checkout at `at`, made on `was`, committed as
/// `how` says and pushed to GitLab's `branch` with your hooks, leased on
/// `was`. A hook that commits fixes of its own and stops the push, as
/// some lint hooks do, has them folded in and the push goes again. Your
/// local `branch` follows if it was at `was` too. A line on what went.
///
/// Anything short of that pushes nothing and leaves your change as edits.
/// If GitLab's branch had moved on, the change is carried onto it when it
/// goes there cleanly, for you to look at before it goes; else it stays
/// on `was`.
pub fn push(at: &Path, branch: &str, was: &str, how: Commit) -> Result<String, String> {
    if matches!(&how, Commit::New(m) if m.trim().is_empty()) {
        return Err("a new commit needs a message, so nothing was pushed".into());
    }
    recover(at);
    if rev(at, "HEAD").as_deref() != Some(was) {
        let now = rev(at, "HEAD").unwrap_or_default();
        return Err(format!("nothing was pushed: your change is on {} now, not on {} as this review has it; leave the review and ot asks about it", short(&now), short(was)));
    }
    git_in(at, &["add", "-A"])?;
    let files = git_in(at, &["diff", "--cached", "--name-only"])?;
    if files.trim().is_empty() {
        return Err("nothing to push: the checkout is as GitLab has it".into());
    }
    std::fs::write(pushing_file(at), was).map_err(|e| e.to_string())?;
    let sent = send(at, branch, was, &how);
    let result = match sent {
        Ok(now) => {
            let did = match how {
                Commit::Amend => "amended",
                Commit::New(_) => "committed",
            };
            let mut said = format!("{did} and pushed {}: {}", short(&now), files.lines().collect::<Vec<_>>().join(", "));
            match rev(at, &format!("refs/heads/{branch}")) {
                Some(local) if local == was => {
                    said.push_str(&format!("; {}", follow(at, branch, was, &now).unwrap_or(format!("your {branch} here follows"))));
                }
                Some(_) => said.push_str(&format!("; your {branch} here was not at GitLab's commit, so it was left alone")),
                None => {}
            }
            Ok(said)
        }
        Err(e) => Err(undo(at, branch, was, &e)),
    };
    let _ = std::fs::remove_file(pushing_file(at));
    result
}

/// Your change out of the review checkout, into git's stash, so the
/// checkout can move again and the change is still there to take back.
pub fn put_aside(at: &Path) -> Result<String, String> {
    let Some(c) = change(at) else {
        return Err("the review checkout holds nothing of yours now".into());
    };
    let name = format!("ot: your change on {} {}", short(&c.on), c.subject);
    git_in(at, &["stash", "push", "--quiet", "--include-untracked", "-m", &name])?;
    Ok(format!("put aside in git's stash as \"{name}\"; git stash list shows it"))
}

/// Commits what is staged as `how` says and pushes it, again once if a
/// hook committed on top and stopped it, with what that hook committed
/// folded in. The commit pushed.
fn send(at: &Path, branch: &str, was: &str, how: &Commit) -> Result<String, String> {
    let lease = format!("--force-with-lease=refs/heads/{branch}:{was}");
    let to = format!("HEAD:refs/heads/{branch}");
    let mut again = true;
    loop {
        match how {
            Commit::Amend => git_in(at, &["commit", "--quiet", "--amend", "--no-edit"])?,
            Commit::New(message) => git_in(at, &["commit", "--quiet", "-m", message.trim()])?,
        };
        let made = rev(at, "HEAD").ok_or("the commit went missing")?;
        match git_in(at, &["push", "--quiet", &lease, "origin", &to]) {
            Ok(_) => return Ok(made),
            Err(_) if again && rev(at, "HEAD").as_deref() != Some(made.as_str()) => {
                git_in(at, &["reset", "--quiet", "--soft", was])?;
                again = false;
            }
            Err(e) => return Err(e),
        }
    }
}

/// A push that failed, undone: your change is edits on `was` again, or on
/// GitLab's `branch` if it had moved on and the change goes there cleanly.
/// Why nothing was pushed, in words.
fn undo(at: &Path, branch: &str, was: &str, e: &str) -> String {
    // The files as they are, which hold your change and any hook's fixes
    // however far the try got, kept as a commit on `was` to carry.
    let tried = git_in(at, &["add", "-A"])
        .and_then(|_| git_in(at, &["write-tree"]))
        .and_then(|t| git_in(at, &["commit-tree", t.trim(), "-p", was, "-m", "your change"]))
        .map(|c| c.trim().to_string());
    let _ = git_in(at, &["reset", "--quiet", was]);
    let here = "nothing was pushed, and your change is still here";
    let fetched = git_in(at, &["fetch", "--quiet", "origin", &format!("+refs/heads/{branch}:refs/remotes/origin/{branch}")]);
    if fetched.is_err() && git_in(at, &["ls-remote", "--exit-code", "origin", &format!("refs/heads/{branch}")]).is_err() {
        return format!("GitLab has no {branch} any more (merged, or deleted), so {here}");
    }
    let theirs = fetched.ok().and_then(|_| rev(at, &format!("refs/remotes/origin/{branch}"))).filter(|t| t != was);
    let (Some(theirs), Ok(tried)) = (theirs, tried) else {
        let tail: Vec<&str> = e.lines().filter(|l| !l.trim().is_empty()).collect();
        let tail = &tail[tail.len().saturating_sub(3)..];
        return format!("{here}: {}", tail.join(" "));
    };
    let moved = format!("GitLab's {branch} had moved on to {} \"{}\"", short(&theirs), subject(at, &theirs));
    match merge_tree(at, &theirs, &tried) {
        Ok(tree) => {
            // Its files become the carried tree, then HEAD moves under them.
            let carried = git_in(at, &["read-tree", "-u", "--reset", &tree])
                .and_then(|_| git_in(at, &["reset", "--quiet", "--soft", &theirs]))
                .and_then(|_| git_in(at, &["reset", "--quiet"]));
            match carried {
                Ok(_) if edits(at).is_empty() => format!("{moved}, which has your change already, so there was nothing to push"),
                Ok(_) => format!("{moved}, so nothing was pushed; your change is on it now, for you to look at before it goes"),
                Err(e) => format!("{moved}, so nothing was pushed, and carrying your change onto it failed: {e}"),
            }
        }
        Err(files) => format!(
            "{moved}, so nothing was pushed; your change conflicts with it in {}, so it stays on {}",
            files.join(", "),
            short(was)
        ),
    }
}

/// A push cut short, by ot going away while it ran, made edits on the
/// commit it was made on again. Whether GitLab took it can't be known
/// here; if it did, the next push finds GitLab has the change already.
fn recover(at: &Path) {
    let mark = pushing_file(at);
    if let Ok(was) = std::fs::read_to_string(&mark) {
        if git_in(at, &["reset", "--quiet", was.trim()]).is_ok() {
            let _ = std::fs::remove_file(&mark);
        }
    }
}

/// Beside the checkout, while a push runs: the commit its change was made on.
fn pushing_file(at: &Path) -> PathBuf {
    at.with_extension("pushing")
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(7)]
}

fn subject(at: &Path, commit: &str) -> String {
    git_in(at, &["log", "-1", "--format=%s", commit]).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// Beside the checkout, while it moves: the head it is moving to.
fn moving_file(at: &Path) -> PathBuf {
    at.with_extension("moving")
}

/// The checkout is at `head`, which a move there would not need to change:
/// cheap enough to ask before deciding whether there will be a wait.
pub fn is_at(at: &Path, head: &str) -> bool {
    git_in(at, &["rev-parse", "HEAD"]).is_ok_and(|h| h.trim() == head)
}

/// The checkout is clean at `head`.
fn settled(at: &Path, head: &str) -> bool {
    is_at(at, head)
        && git_in(at, &["status", "--porcelain", "--untracked-files=no"])
            .is_ok_and(|e| e.trim().is_empty())
}

/// The file in a mirror that says it is one, and when it was made.
const MIRROR_MARK: &str = ".ot-mirror";

/// A main checkout's node_modules, mirrored: its links copied as they are,
/// so a package of the repo (a relative link) resolves to this checkout's
/// own source and a third-party one, through .content, to the one install;
/// its scope directories and .bin walked one level the same way; anything
/// else linked whole. Made again when main's is newer. A node_modules of
/// the worktree's own, with no mark, is never touched.
fn mirror(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::fs;
    let Ok(made) = fs::metadata(src).and_then(|m| m.modified()) else {
        return Ok(());
    };
    match fs::metadata(dst.join(MIRROR_MARK)).and_then(|m| m.modified()) {
        Ok(mirrored) if mirrored >= made => return Ok(()),
        Ok(_) => fs::remove_dir_all(dst)?,
        Err(_) if dst.exists() => return Ok(()),
        Err(_) => {}
    }
    // Built aside and moved into place whole, so a mirror is never half made.
    let new = dst.with_file_name(".node_modules.ot");
    let _ = fs::remove_dir_all(&new);
    fs::create_dir_all(&new)?;
    // A link is copied as it is; anything else is linked to.
    let one = |from: &Path, to: &Path| match fs::read_link(from) {
        Ok(target) => std::os::unix::fs::symlink(target, to),
        Err(_) => std::os::unix::fs::symlink(from, to),
    };
    for e in fs::read_dir(src)?.flatten() {
        let name = e.file_name();
        let (from, to) = (e.path(), new.join(&name));
        let walk = name.to_string_lossy().starts_with('@') || name == ".bin";
        if walk && e.file_type().is_ok_and(|t| t.is_dir()) {
            fs::create_dir_all(&to)?;
            for inner in fs::read_dir(&from)?.flatten() {
                one(&inner.path(), &to.join(inner.file_name()))?;
            }
        } else {
            one(&from, &to)?;
        }
    }
    fs::write(new.join(MIRROR_MARK), "")?;
    fs::rename(&new, dst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn sh(dir: &Path, cmd: &str) -> String {
        let ok = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            ok.status.success(),
            "{cmd}: {}",
            String::from_utf8_lossy(&ok.stderr)
        );
        String::from_utf8_lossy(&ok.stdout).trim().to_string()
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ot-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_review_checkout_follows_each_head_and_keeps_edits() {
        let d = scratch("worktree");
        let repo = d.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        sh(&repo, "git init -q -b main && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m one && echo a > f && git add f && git -c user.name=t -c user.email=t@t commit -q -m two");
        let heads: Vec<String> = ["HEAD~1", "HEAD"]
            .iter()
            .map(|r| git_in(&repo, &["rev-parse", r]).unwrap().trim().to_string())
            .collect();
        let at = d.join("cache/review");
        review(&repo, &at, &heads[0]).unwrap();
        assert!(!at.join("f").exists());
        review(&repo, &at, &heads[1]).unwrap();
        assert_eq!(std::fs::read_to_string(at.join("f")).unwrap(), "a\n");
        std::fs::write(at.join("f"), "mine\n").unwrap();
        assert!(review(&repo, &at, &heads[0])
            .unwrap_err()
            .contains("holds the review checkout"));
        assert_eq!(std::fs::read_to_string(at.join("f")).unwrap(), "mine\n");
        review(&repo, &at, &heads[1]).expect("its own head again needs no move");
        assert_eq!(std::fs::read_to_string(at.join("f")).unwrap(), "mine\n");
        // The same changes left by a move cut short are that move's: the
        // next one goes through.
        std::fs::write(moving_file(&at), &heads[1]).unwrap();
        review(&repo, &at, &heads[0]).unwrap();
        assert!(!at.join("f").exists() && !moving_file(&at).exists());
        // A mark from a move that finished after ot went away is just cleared.
        std::fs::write(moving_file(&at), &heads[0]).unwrap();
        review(&repo, &at, &heads[1]).unwrap();
        assert_eq!(std::fs::read_to_string(at.join("f")).unwrap(), "a\n");
        assert!(!moving_file(&at).exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// GitLab (a bare origin) with a branch feat holding f and g, your
    /// checkout of it, and the review checkout at its head.
    fn gitlab(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let d = scratch(name);
        sh(&d, "git init -q --bare origin && git clone -q origin repo 2>/dev/null");
        let repo = d.join("repo");
        sh(&repo, "git config user.name t && git config user.email t@t && git config commit.gpgsign false");
        sh(&repo, "git switch -q -c feat && echo a > f && echo a > g && git add f g && git commit -q -m one && git push -q origin feat");
        let at = d.join("review");
        review(&repo, &at, &sh(&repo, "git rev-parse HEAD")).unwrap();
        (d, repo, at)
    }

    /// Someone else pushes `cmd`'s change to feat, from a clone of their own.
    fn they_push(d: &Path, cmd: &str) -> String {
        sh(d, &format!("rm -rf other && git clone -q -b feat origin other 2>/dev/null && cd other && {cmd} && git -c user.name=o -c user.email=o@o commit -qam theirs && git push -q origin feat"));
        sh(&d.join("origin"), "git rev-parse feat")
    }

    fn head(at: &Path) -> String {
        rev(at, "HEAD").unwrap()
    }

    #[test]
    fn your_change_goes_out_as_a_new_commit_or_amended_and_your_branch_follows() {
        let (d, repo, at) = gitlab("push");
        let log = |what: &str| sh(&d.join("origin"), &format!("git log --format={what} feat"));
        assert!(change(&at).is_none());
        assert!(push(&at, "feat", &head(&at), Commit::Amend).unwrap_err().contains("nothing to push"));
        assert!(push(&at, "feat", &head(&at), Commit::New(" ".into())).unwrap_err().contains("needs a message"));

        std::fs::write(at.join("f"), "b\n").unwrap();
        let c = change(&at).unwrap();
        assert_eq!((c.subject.as_str(), c.files.as_slice()), ("one", &["f".to_string()][..]));
        let said = push(&at, "feat", &c.on, Commit::New("two".into())).unwrap();
        assert!(said.starts_with("committed and pushed") && said.ends_with(": f; your feat here follows"), "{said}");
        assert_eq!(log("%s"), "two\none");
        assert_eq!(std::fs::read_to_string(repo.join("f")).unwrap(), "b\n", "the checkout of feat follows");

        std::fs::write(at.join("f"), "c\n").unwrap();
        push(&at, "feat", &head(&at), Commit::Amend).unwrap();
        assert_eq!(log("%s"), "two\none", "amended in place");
        assert_eq!(sh(&d.join("origin"), "git show feat:f"), "c");
        assert!(change(&at).is_none() && !pushing_file(&at).exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_hook_that_commits_its_fixes_and_stops_the_push_has_them_folded_in() {
        let (d, repo, at) = gitlab("hook");
        // As some pre-push lint hooks do: fix, commit the fix, refuse.
        let hook = repo.join(".git/hooks/pre-push");
        std::fs::write(&hook, "#!/bin/sh\ngrep -q fixed f && exit 0\necho fixed >> f && git commit -qam 'Apply lint fixes'\necho 'Run git push again' >&2\nexit 1\n").unwrap();
        sh(&repo, "chmod +x .git/hooks/pre-push");
        std::fs::write(at.join("f"), "b\n").unwrap();
        push(&at, "feat", &head(&at), Commit::Amend).unwrap();
        assert_eq!(sh(&d.join("origin"), "git log --format=%s feat"), "one", "one commit, not two");
        assert_eq!(sh(&d.join("origin"), "git show feat:f"), "b\nfixed");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_branch_pushed_since_takes_your_change_on_top_when_it_goes_cleanly_and_nothing_is_pushed() {
        let (d, _repo, at) = gitlab("carry");
        let was = head(&at);
        let theirs = they_push(&d, "echo theirs > g");
        std::fs::write(at.join("f"), "mine\n").unwrap();
        let why = push(&at, "feat", &was, Commit::Amend).unwrap_err();
        assert!(why.contains("had moved on to") && why.contains("your change is on it now"), "{why}");
        assert_eq!(head(&at), theirs);
        assert_eq!(change(&at).unwrap().files, ["f"]);
        assert_eq!(std::fs::read_to_string(at.join("g")).unwrap(), "theirs\n");
        // A push from where the review last was is turned away; from here it goes.
        assert!(push(&at, "feat", &was, Commit::Amend).unwrap_err().contains("leave the review and ot asks"));
        push(&at, "feat", &theirs, Commit::New("mine".into())).unwrap();
        assert_eq!(sh(&d.join("origin"), "git log --format=%s feat"), "mine\ntheirs\none");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_branch_pushed_since_that_conflicts_leaves_your_change_where_it_was() {
        let (d, _repo, at) = gitlab("conflict");
        let was = head(&at);
        they_push(&d, "echo theirs > f");
        std::fs::write(at.join("f"), "mine\n").unwrap();
        let why = push(&at, "feat", &was, Commit::Amend).unwrap_err();
        assert!(why.contains("conflicts with it in f"), "{why}");
        assert_eq!(head(&at), was);
        assert_eq!(std::fs::read_to_string(at.join("f")).unwrap(), "mine\n");
        assert_eq!(change(&at).unwrap().files, ["f"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_commit_that_fails_while_the_branch_moved_keeps_your_files() {
        let (d, repo, at) = gitlab("refused");
        let was = head(&at);
        they_push(&d, "echo theirs > g");
        std::fs::write(repo.join(".git/hooks/pre-commit"), "#!/bin/sh\nexit 1\n").unwrap();
        sh(&repo, "chmod +x .git/hooks/pre-commit");
        std::fs::write(at.join("f"), "mine\n").unwrap();
        push(&at, "feat", &was, Commit::New("mine".into())).unwrap_err();
        assert_eq!(std::fs::read_to_string(at.join("f")).unwrap(), "mine\n");
        assert_eq!(change(&at).unwrap().files, ["f"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_branch_gone_from_gitlab_says_so_and_keeps_your_change() {
        let (d, _repo, at) = gitlab("gone");
        let was = head(&at);
        sh(&d.join("origin"), "git branch -q -m feat merged");
        std::fs::write(at.join("f"), "mine\n").unwrap();
        let why = push(&at, "feat", &was, Commit::Amend).unwrap_err();
        assert!(why.starts_with("GitLab has no feat any more"), "{why}");
        assert_eq!((head(&at), change(&at).unwrap().files), (was, vec!["f".to_string()]));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_change_put_aside_frees_the_checkout_and_waits_in_the_stash() {
        let (d, repo, at) = gitlab("aside");
        std::fs::write(at.join("f"), "mine\n").unwrap();
        std::fs::write(at.join("new"), "mine\n").unwrap();
        put_aside(&at).unwrap();
        assert!(change(&at).is_none() && !at.join("new").exists());
        let other = sh(&repo, "git commit -q --allow-empty -m other && git rev-parse HEAD");
        review(&repo, &at, &other).unwrap();
        assert!(sh(&repo, "git stash list").contains("ot: your change on"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_change_gitlab_has_already_leaves_nothing_to_push() {
        let (d, _repo, at) = gitlab("already");
        let was = head(&at);
        let theirs = they_push(&d, "echo mine > f");
        std::fs::write(at.join("f"), "mine\n").unwrap();
        let why = push(&at, "feat", &was, Commit::Amend).unwrap_err();
        assert!(why.contains("has your change already"), "{why}");
        assert!(change(&at).is_none() && head(&at) == theirs);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_push_cut_short_is_your_change_again() {
        let (d, repo, at) = gitlab("cut");
        let was = head(&at);
        std::fs::write(at.join("f"), "mine\n").unwrap();
        // ot went away after committing, before the push was through.
        std::fs::write(pushing_file(&at), &was).unwrap();
        sh(&at, "git commit -qam mine");
        let c = change(&at).unwrap();
        assert_eq!((c.on.as_str(), c.files.as_slice()), (was.as_str(), &["f".to_string()][..]));
        assert!(!pushing_file(&at).exists());
        // A move away refuses, naming it.
        let other = sh(&repo, "git commit -q --allow-empty -m other && git rev-parse HEAD");
        let why = review(&repo, &at, &other).unwrap_err();
        assert!(why.contains("your change on") && why.contains("\"one\" (f)"), "{why}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn node_modules_is_mirrored_link_for_link() {
        let d = scratch("mirror");
        let (src, dst) = (d.join("main/node_modules"), d.join("tree/node_modules"));
        sh(&d, "mkdir -p main/node_modules/@acme main/node_modules/.bin main/packages/x main/node_modules/.content/lodash tree");
        sh(&src, "ln -s ../packages/x @acme/x && ln -s ../.content/lodash/cli .bin/lodash && ln -s .content/lodash lodash");
        mirror(&src, &dst).unwrap();
        assert_eq!(
            std::fs::read_link(dst.join("@acme/x")).unwrap(),
            Path::new("../packages/x"),
            "a repo package resolves in the worktree"
        );
        assert_eq!(
            std::fs::read_link(dst.join("lodash")).unwrap(),
            Path::new(".content/lodash")
        );
        assert_eq!(
            std::fs::read_link(dst.join(".content")).unwrap(),
            src.join(".content"),
            "third-party code is main's one install"
        );
        assert!(dst.join(MIRROR_MARK).exists());
        // A node_modules of the worktree's own is left alone.
        let own = d.join("own/node_modules");
        std::fs::create_dir_all(&own).unwrap();
        mirror(&src, &own).unwrap();
        assert!(!own.join(MIRROR_MARK).exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
