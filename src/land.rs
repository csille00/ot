//! Landing a stack: your MRs from its bottom up to the one you picked,
//! merged one at a time, each only once the one below it is live in
//! production, so no two go out in one deploy, unless Claude read the two
//! and said they can (judge.rs). ot keeps only which MRs you asked it to
//! land, what Claude said of them, and which it has moved onto the default
//! branch since the one below merged. Everything else is read again from
//! each refresh by `next`, so a merge from GitLab's page, a rebase by hand
//! or a late approval just counts. The bottom one waits for you to merge
//! it, and so does each that waited for the one below to be live. One
//! Claude said can go out with the one below, ot merges itself.
//!
//! A stack is a tree. Once one of a landing's MRs merges, the next waits
//! to be rebased past it, as does every other MR of yours that sat on it:
//! the other branches of a fork, what is stacked on its top. The board
//! does those rebases on its own, each MR on its own, landing or not, and
//! pushes them when they are only git's replay (main.rs, due). Two
//! landings may share the MRs under a fork, so both branches can land.
use crate::model::{bang, move_of, under, Move, Mr, Rules, Snapshot};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A stack you asked ot to land.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Landing {
    /// Bottom first.
    pub iids: Vec<u64>,
    /// MRs rebased onto the default branch past the one of these they sat
    /// on, once it merged, leaving out its commits: its own, and others
    /// that were on them.
    pub rebased: Vec<u64>,
    /// What Claude said of those above the bottom, each as it read it.
    pub verdicts: Vec<Verdict>,
}

/// Claude's reading of one MR: whether it can go out in the same deploy as
/// the one below it, and why, for you. It holds only while both are the
/// code it read: the MR at `head`, the one below at `under`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub iid: u64,
    pub head: String,
    pub under: String,
    pub together: bool,
    pub why: String,
}

/// Where a landing is.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// It can merge now, once you say so.
    Ready(u64),
    /// It needs rebasing onto the default branch, with what is stacked on
    /// it, past the MR below it, which merged.
    Rebase(u64),
    /// ot merges it itself, now or once its pipeline passes: it is approved
    /// and Claude said it can go out with the one below.
    Merge(u64),
    /// Nothing yet. `on` is what it waits on, for its row, or None when its
    /// move already says (a review, its pipeline, you).
    Wait { iid: u64, on: Option<String> },
    /// None of it is open.
    Done,
    /// It can't go on, and why.
    Stop(String),
}

/// What the board says of the MR a landing has come to.
#[derive(Clone, Debug, PartialEq)]
pub struct Said {
    /// Its landing's MRs: "!1, !2, !3".
    pub names: String,
    /// In place of its move, when its move doesn't say it.
    pub word: Option<String>,
    /// It is ready for you to merge.
    pub ready: bool,
    /// What Claude said of it going out with the one below.
    pub why: Option<String>,
    /// Claude said it can, so ot merges it itself and no question shows
    /// you that why.
    pub itself: bool,
}

impl Landing {
    /// `picked` and every MR under it in its stack, bottom first, down
    /// through one merged but not live yet, which it then waits on; or why
    /// not, when one of them isn't yours.
    pub fn of(snap: &Snapshot, was_on: &HashMap<u64, u64>, picked: &Mr) -> Result<Landing, String> {
        let mut chain = vec![picked];
        while let Some(p) = under(snap, was_on, chain[chain.len() - 1]).filter(|p| !chain.contains(p)) {
            chain.push(p);
        }
        if let Some(m) = chain.iter().find(|m| m.author != snap.me) {
            return Err(format!("{} is {}'s, so it isn't yours to land", bang(m.iid), m.author));
        }
        Ok(Landing {
            iids: chain.iter().rev().map(|m| m.iid).collect(),
            ..Default::default()
        })
    }

    pub fn next(&self, snap: &Snapshot, rules: &Rules) -> Step {
        let Some(m) = self.iids.iter().find_map(|&i| snap.mr(i).filter(|m| m.reach.is_none())) else {
            return Step::Done;
        };
        let iid = m.iid;
        let wait = |on: String| Step::Wait { iid, on: Some(on) };
        if let Some(b) = self.under(iid) {
            // Not open, so merged: it stays on the board until it is live,
            // or it closed, which leaves the board too.
            let merged = snap.mr(b).is_some();
            if !self.rebased.contains(&iid) {
                return if !merged {
                    Step::Stop(format!("{} left the board before ot saw it merge, so it can't tell which of {}'s commits are its own", bang(b), bang(iid)))
                } else if m.target != snap.default_branch {
                    // GitLab does that once the branch is deleted.
                    wait(format!("waits for GitLab to move it off {}'s branch", bang(b)))
                } else {
                    Step::Rebase(iid)
                };
            }
            let together = self.verdict(snap, m).is_some_and(|v| v.together);
            // Set to merge when green, GitLab won't wait for it.
            if merged && !m.auto_merge && !together {
                return wait(format!("lands once {} is live", bang(b)));
            }
            // Until GitLab looks again after ot's push, which the next board
            // asks it to, its conflict may be the one ot's rebase took away.
            if m.rechecking {
                return wait("waits for GitLab to check the rebase".into());
            }
            // The rebase started a pipeline, and you'd only be waiting on it
            // to say yes to what Claude already read.
            if together && m.approved && matches!(move_of(m, &snap.me, rules), Move::Merge | Move::Running) {
                return Step::Merge(iid);
            }
        }
        match move_of(m, &snap.me, rules) {
            Move::Merge => Step::Ready(iid),
            _ => Step::Wait { iid, on: None },
        }
    }

    /// ot pushed a rebase of `iid` and what is stacked on it. What Claude
    /// said was of the code pushed over, which GitLab can go on showing for
    /// a moment, so nothing merges on it; it reads the new heads.
    pub fn pushed(&mut self, iid: u64) {
        if self.iids.contains(&iid) {
            self.verdicts.clear();
        }
    }

    /// `iid` went onto the default branch leaving out the commits of
    /// `left_out`, the merged MR it sat on, which counts when that is one
    /// of this landing's.
    pub fn rebased(&mut self, iid: u64, left_out: u64) {
        if self.iids.contains(&left_out) && !self.rebased.contains(&iid) {
            self.rebased.push(iid);
        }
    }

    /// Its MRs on the board, bottom first, for Claude to read, when one
    /// above the bottom is open with no verdict on its head as it is now: a
    /// push since, ot's own rebase included, has Claude read it again.
    pub fn to_read(&self, snap: &Snapshot) -> Option<Vec<Mr>> {
        let layers: Vec<&Mr> = self.iids.iter().filter_map(|&i| snap.mr(i)).collect();
        let unread = layers.iter().skip(1).any(|m| m.reach.is_none() && self.verdict(snap, m).is_none());
        unread.then(|| layers.into_iter().cloned().collect())
    }

    /// What Claude said, each in place of what it said of that MR before;
    /// but not what it said of code pushed over since it read it.
    pub fn judged(&mut self, snap: &Snapshot, verdicts: Vec<Verdict>) {
        let now: Vec<Verdict> = verdicts.into_iter().filter(|v| self.holds(snap, v)).collect();
        self.verdicts.retain(|o| !now.iter().any(|v| v.iid == o.iid));
        self.verdicts.extend(now);
    }

    fn verdict(&self, snap: &Snapshot, m: &Mr) -> Option<&Verdict> {
        self.verdicts.iter().find(|v| v.iid == m.iid && self.holds(snap, v))
    }

    /// `v` is of the code on the board: its MR at the head Claude read, and
    /// the one below too, while that is on the board (once it is gone it is
    /// live, and nothing waits on it).
    fn holds(&self, snap: &Snapshot, v: &Verdict) -> bool {
        let head = |i: u64| snap.mr(i).map(|m| m.head.as_str());
        head(v.iid) == Some(v.head.as_str()) && self.under(v.iid).and_then(head).is_none_or(|h| h == v.under)
    }

    /// The MR under `iid` in this landing.
    fn under(&self, iid: u64) -> Option<u64> {
        let at = self.iids.iter().position(|&i| i == iid)?;
        at.checked_sub(1).map(|b| self.iids[b])
    }

    /// "!1, !2, !3".
    pub fn names(&self) -> String {
        names(&self.iids)
    }
}

fn names(iids: &[u64]) -> String {
    let all: Vec<String> = iids.iter().map(|&i| bang(i)).collect();
    all.join(", ")
}

/// What the board says of each MR a landing has come to, by its iid. The
/// landings of a fork's branches come to the MRs under it alike, and its
/// row names all of theirs.
pub fn said(landings: &[Landing], snap: &Snapshot, rules: &Rules) -> HashMap<u64, Said> {
    let mut all = HashMap::new();
    let mut of: HashMap<u64, Vec<u64>> = HashMap::new();
    for l in landings {
        let (iid, word, ready) = match l.next(snap, rules) {
            Step::Ready(iid) => (iid, Some("ready to land".to_string()), true),
            Step::Rebase(iid) => (iid, None, false),
            // Its move would say it is yours to merge.
            Step::Merge(iid) => (iid, Some("merges itself".to_string()), false),
            Step::Wait { iid, on } => (iid, on, false),
            Step::Done | Step::Stop(_) => continue,
        };
        let verdict = snap.mr(iid).and_then(|m| l.verdict(snap, m));
        let (why, itself) = (verdict.map(|v| v.why.clone()), verdict.is_some_and(|v| v.together));
        all.entry(iid).or_insert(Said { names: String::new(), word, ready, why, itself });
        let named = of.entry(iid).or_default();
        for &i in &l.iids {
            if !named.contains(&i) {
                named.push(i);
            }
        }
    }
    for (iid, s) in &mut all {
        s.names = names(&of[iid]);
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixture::{mr, rules};
    use crate::model::{remember, Reach};

    /// sam's stack of three, approved and green, as GitLab shows it before
    /// any of it merges.
    fn stack() -> Snapshot {
        let mut mrs = vec![
            mr(1, "sam", "sam/stamp", "main"),
            mr(2, "sam", "sam/step", "sam/stamp"),
            mr(3, "sam", "sam/retry", "sam/step"),
        ];
        for m in &mut mrs {
            m.approved = true;
            m.mergeable = true;
        }
        Snapshot { me: "sam".into(), default_branch: "main".into(), mrs, ..Default::default() }
    }

    /// `iid` merged, as GitLab shows it after: listed with how far it has
    /// gone until it is live (None), its branch gone, so the MRs on it
    /// moved onto main; and `was`, the board's memory of where each was, as
    /// it reads that.
    fn merged(snap: &mut Snapshot, was: &mut HashMap<u64, u64>, iid: u64, reach: Option<Reach>) {
        let source = snap.mr(iid).unwrap().source.clone();
        match reach {
            Some(r) => snap.mrs.iter_mut().find(|m| m.iid == iid).unwrap().reach = Some(r),
            None => snap.mrs.retain(|m| m.iid != iid),
        }
        for m in snap.mrs.iter_mut().filter(|m| m.target == source) {
            m.target = "main".into();
        }
        *was = remember(was, snap);
    }

    #[test]
    fn a_stack_lands_bottom_up_each_once_the_one_below_is_live() {
        let mut snap = stack();
        let mut was = remember(&HashMap::new(), &snap);
        let top = snap.mr(3).unwrap().clone();
        let mut l = Landing::of(&snap, &was, &top).unwrap();
        assert_eq!(l.names(), "!1, !2, !3");
        assert_eq!(l.next(&snap, &rules()), Step::Ready(1));

        merged(&mut snap, &mut was, 1, Some(Reach::Merged));
        // Rebased while !1 deploys, so its pipeline runs meanwhile.
        assert_eq!(l.next(&snap, &rules()), Step::Rebase(2));
        l.rebased(2, 1);
        let wait = Step::Wait { iid: 2, on: Some("lands once !1 is live".into()) };
        assert_eq!(l.next(&snap, &rules()), wait);
        merged(&mut snap, &mut was, 1, Some(Reach::Deploying));
        assert_eq!(l.next(&snap, &rules()), wait);
        // Unless you set it to merge when green: its row says so.
        snap.mrs.iter_mut().find(|m| m.iid == 2).unwrap().auto_merge = true;
        assert_eq!(l.next(&snap, &rules()), Step::Wait { iid: 2, on: None });
        // Except the conflict GitLab still shows from before the rebase,
        // until it looks again; one it found since is yours to fix.
        let two = snap.mrs.iter_mut().find(|m| m.iid == 2).unwrap();
        (two.conflicts, two.rechecking) = (true, true);
        let checking = Step::Wait { iid: 2, on: Some("waits for GitLab to check the rebase".into()) };
        assert_eq!(l.next(&snap, &rules()), checking);
        snap.mrs.iter_mut().find(|m| m.iid == 2).unwrap().rechecking = false;
        assert_eq!(l.next(&snap, &rules()), Step::Wait { iid: 2, on: None });
        snap.mrs.iter_mut().find(|m| m.iid == 2).unwrap().conflicts = false;
        snap.mrs.iter_mut().find(|m| m.iid == 2).unwrap().auto_merge = false;

        merged(&mut snap, &mut was, 1, None);
        assert_eq!(l.next(&snap, &rules()), Step::Ready(2));
        merged(&mut snap, &mut was, 2, Some(Reach::Merged));
        assert_eq!(l.next(&snap, &rules()), Step::Rebase(3));
        l.rebased(3, 2);
        assert_eq!(l.next(&snap, &rules()), Step::Wait { iid: 3, on: Some("lands once !2 is live".into()) });
        merged(&mut snap, &mut was, 2, None);
        assert_eq!(l.next(&snap, &rules()), Step::Ready(3));
        merged(&mut snap, &mut was, 3, Some(Reach::Merged));
        assert_eq!(l.next(&snap, &rules()), Step::Done);
    }

    #[test]
    fn one_claude_said_can_go_out_with_the_one_below_merges_without_you() {
        let mut snap = stack();
        let mut was = remember(&HashMap::new(), &snap);
        let mut l = Landing::of(&snap, &was, snap.mr(2).unwrap()).unwrap();
        merged(&mut snap, &mut was, 1, Some(Reach::Merged));
        l.rebased(2, 1);
        snap.mrs.iter_mut().find(|m| m.iid == 2).unwrap().pipeline = "running".into();
        let waits = Step::Wait { iid: 2, on: Some("lands once !1 is live".into()) };
        let verdict = |together| Verdict { iid: 2, head: String::new(), under: String::new(), together, why: String::new() };
        l.judged(&snap, vec![verdict(false)]);
        assert_eq!(l.next(&snap, &rules()), waits);
        l.judged(&snap, vec![verdict(true)]);
        assert_eq!(l.next(&snap, &rules()), Step::Merge(2), "set to merge once its pipeline passes");
        let row = &said(&[l.clone()], &snap, &rules())[&2];
        assert_eq!((row.word.as_deref(), row.ready, row.itself), (Some("merges itself"), false, true), "not yours to merge");
        // GitLab can show the head ot pushed over for a moment: not on that.
        let read = l.clone();
        l.pushed(2);
        assert_eq!(l.next(&snap, &rules()), waits);
        l = read;
        snap.mrs.iter_mut().find(|m| m.iid == 2).unwrap().auto_merge = true;
        assert_eq!(l.next(&snap, &rules()), Step::Wait { iid: 2, on: None }, "and once set, GitLab does it");
        // Not one still in review: that waits as its row says.
        let two = snap.mrs.iter_mut().find(|m| m.iid == 2).unwrap();
        (two.auto_merge, two.approved) = (false, false);
        assert_eq!(l.next(&snap, &rules()), Step::Wait { iid: 2, on: None });
    }

    #[test]
    fn a_fork_lands_both_ways_each_branch_rebased_on_its_own() {
        // !1 with !2 and !3 on it, as on the board: next-turn-metric with
        // next-turn-stuck and next-turn-per-turn.
        let mut snap = stack();
        snap.mrs[2].target = "sam/stamp".into();
        let mut was = remember(&HashMap::new(), &snap);
        let mut three = Landing::of(&snap, &was, snap.mr(3).unwrap()).unwrap();
        let mut two = Landing::of(&snap, &was, snap.mr(2).unwrap()).unwrap();
        assert_eq!((three.names(), two.names()), ("!1, !3".into(), "!1, !2".into()));
        let both = [three.clone(), two.clone()];
        assert_eq!(said(&both, &snap, &rules())[&1].names, "!1, !3, !2", "its row names both");

        merged(&mut snap, &mut was, 1, Some(Reach::Merged));
        assert_eq!((three.next(&snap, &rules()), two.next(&snap, &rules())), (Step::Rebase(3), Step::Rebase(2)));
        three.rebased(3, 1);
        two.rebased(2, 1);
        let wait = |iid| Step::Wait { iid, on: Some("lands once !1 is live".into()) };
        assert_eq!((three.next(&snap, &rules()), two.next(&snap, &rules())), (wait(3), wait(2)));
    }

    #[test]
    fn one_picked_on_a_merged_mr_waits_for_that_to_be_live() {
        // !1 merged before you picked !2: past it, then once it is live.
        let mut snap = stack();
        let mut was = remember(&HashMap::new(), &snap);
        merged(&mut snap, &mut was, 1, Some(Reach::Merged));
        let mut l = Landing::of(&snap, &was, snap.mr(2).unwrap()).unwrap();
        assert_eq!(l.names(), "!1, !2");
        assert_eq!(l.next(&snap, &rules()), Step::Rebase(2));
        l.rebased(2, 1);
        assert_eq!(l.next(&snap, &rules()), Step::Wait { iid: 2, on: Some("lands once !1 is live".into()) });
    }

    #[test]
    fn one_gone_before_ot_saw_it_merge_stops_it() {
        // Live while ot was shut, or closed: either way it left the board,
        // and no rebase of the next one left its commits out.
        let mut snap = stack();
        let mut was = remember(&HashMap::new(), &snap);
        let l = Landing::of(&snap, &was, snap.mr(3).unwrap()).unwrap();
        merged(&mut snap, &mut was, 1, None);
        assert_eq!(
            l.next(&snap, &rules()),
            Step::Stop("!1 left the board before ot saw it merge, so it can't tell which of !2's commits are its own".into())
        );
    }

    #[test]
    fn a_rebase_counts_past_any_of_its_mrs_for_whatever_was_on_it() {
        let snap = stack();
        let mut l = Landing::of(&snap, &HashMap::new(), snap.mr(3).unwrap()).unwrap();
        l.rebased(9, 7);
        assert!(l.rebased.is_empty());
        l.rebased(9, 2);
        assert_eq!(l.rebased, [9]);
    }

    #[test]
    fn it_waits_on_what_the_row_already_says_and_on_gitlab() {
        let mut snap = stack();
        snap.mrs[0].approved = false;
        let l = Landing::of(&snap, &HashMap::new(), snap.mr(3).unwrap()).unwrap();
        assert_eq!(l.next(&snap, &rules()), Step::Wait { iid: 1, on: None });
        snap.mrs[0].approved = true;
        assert_eq!(
            said(std::slice::from_ref(&l), &snap, &rules())[&1],
            Said { names: "!1, !2, !3".into(), word: Some("ready to land".into()), ready: true, why: None, itself: false }
        );

        // Merged, but GitLab hasn't moved !2 off !1's branch yet.
        snap.mrs[0].reach = Some(Reach::Merged);
        assert_eq!(
            l.next(&snap, &rules()),
            Step::Wait { iid: 2, on: Some("waits for GitLab to move it off !1's branch".into()) }
        );
    }

    #[test]
    fn only_your_own_mrs_land() {
        let mut snap = stack();
        snap.mrs[0].author = "kim".into();
        assert_eq!(
            Landing::of(&snap, &HashMap::new(), snap.mr(2).unwrap()),
            Err("!1 is kim's, so it isn't yours to land".into())
        );
    }
}
