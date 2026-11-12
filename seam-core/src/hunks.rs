//! The diff reader (share/hunks.awk): per file, which new lines came, which
//! old lines went, and where a line on one side sits on the other.
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct FileHunks {
    pub add: HashSet<u32>,
    pub del: HashSet<u32>,
    /// Where a deletion sits in new numbering, and an insertion in old.
    pub delat: HashSet<u32>,
    pub addat: HashSet<u32>,
    /// For each old line that went, the new line where the change now sits.
    pub at: HashMap<u32, u32>,
    pub blank_old: HashSet<u32>,
    pub blank_new: HashSet<u32>,
}

impl FileHunks {
    /// How many of the lines in [s, e] on this side changed.
    pub fn lines(&self, new: bool, s: u32, e: u32) -> u32 {
        let set = if new { &self.add } else { &self.del };
        (s..=e).filter(|l| set.contains(l)).count() as u32
    }
    /// Whether anything in [s, e] changed at all: a line of it, or a change
    /// sitting between two of its lines.
    pub fn touched(&self, new: bool, s: u32, e: u32) -> bool {
        (s..=e).any(|l| {
            if new {
                self.add.contains(&l) || (l > s && self.delat.contains(&l))
            } else {
                self.del.contains(&l) || (l < e && self.addat.contains(&l))
            }
        })
    }
}

/// A unified diff, of any context, by the path each file has on the new side
/// (the old one for a file that went).
///
/// Where a change sits on the side it left nothing on is said the way a -U0
/// hunk header says it, since that is what touched() was written against:
/// lines that went with none coming sit after the new line before them, and
/// lines that came with none going sit after the old line before them. With
/// both, the lines that went sit at the first that came, and the other way
/// about.
pub fn read(diff: &str) -> HashMap<String, FileHunks> {
    let mut files: HashMap<String, FileHunks> = HashMap::new();
    let (mut fo, mut f) = (String::new(), String::new());
    // The next line on each side, and how many the hunk still has.
    let (mut o, mut nn, mut left_o, mut left_n) = (0u32, 0u32, 0u32, 0u32);
    // The run of lines that went, waiting to hear whether any came.
    let mut went: Vec<u32> = vec![];
    let mut came = false;
    fn settle(h: &mut FileHunks, went: &mut Vec<u32>, came: &mut bool, nn: u32) {
        for o in went.drain(..) {
            h.delat.insert(nn.saturating_sub(1));
            h.at.insert(o, nn.saturating_sub(1));
        }
        *came = false;
    }
    for line in diff.split('\n') {
        // Inside a hunk a line is the hunk's whatever it opens with: a line
        // that went and began "-- " is not the next file's header.
        if left_o > 0 || left_n > 0 {
            let h = files.entry(f.clone()).or_default();
            let blank = line[1.min(line.len())..].chars().all(|c| c == ' ' || c == '\t');
            match line.as_bytes().first() {
                Some(b'-') => {
                    if came {
                        settle(h, &mut went, &mut came, nn);
                    }
                    h.del.insert(o);
                    went.push(o);
                    if blank {
                        h.blank_old.insert(o);
                    }
                    o += 1;
                    left_o = left_o.saturating_sub(1);
                }
                Some(b'+') => {
                    if !came {
                        h.addat.insert(if went.is_empty() { o.saturating_sub(1) } else { o });
                        for w in went.drain(..) {
                            h.delat.insert(nn);
                            h.at.insert(w, nn);
                        }
                        came = true;
                    }
                    h.add.insert(nn);
                    if blank {
                        h.blank_new.insert(nn);
                    }
                    nn += 1;
                    left_n = left_n.saturating_sub(1);
                }
                Some(b'\\') => {}
                _ => {
                    settle(h, &mut went, &mut came, nn);
                    o += 1;
                    nn += 1;
                    left_o = left_o.saturating_sub(1);
                    left_n = left_n.saturating_sub(1);
                }
            }
            if left_o == 0 && left_n == 0 {
                settle(h, &mut went, &mut came, nn);
            }
            continue;
        }
        if let Some(p) = line.strip_prefix("--- ") {
            fo = p.strip_prefix("a/").unwrap_or(p).to_string();
        } else if let Some(p) = line.strip_prefix("+++ ") {
            f = p.strip_prefix("b/").unwrap_or(p).to_string();
            if f == "/dev/null" {
                f = fo.clone();
            }
        } else if line.starts_with("@@") {
            let mut w = line.split(' ');
            // A side with no lines in the hunk is given by the line before.
            let side = |t: Option<&str>| {
                let t = t.map(|t| &t[1.min(t.len())..]).unwrap_or("");
                let mut p = t.split(',');
                let a: u32 = p.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                let n: u32 = p.next().and_then(|x| x.parse().ok()).unwrap_or(1);
                (if n == 0 { a + 1 } else { a }, n)
            };
            w.next();
            (o, left_o) = side(w.next());
            (nn, left_n) = side(w.next());
        }
    }
    files
}
