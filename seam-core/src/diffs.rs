//! Each definition's own patch, for a renderer that shows one (diffs() in
//! bin/seam): the lines it has on the new side and the lines it had on the
//! old, in the order a diff of the whole file has them. Taking the two sides
//! separately is what keeps a removed definition's lines out of the
//! definition that followed it.
use crate::hunks;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};

/// The most lines of one definition seam's own page is handed (bin/seam's
/// 400); a caller with room for all of it passes usize::MAX.
pub const MOST: usize = 400;

fn lines(text: Option<&String>) -> Vec<&str> {
    let mut l: Vec<&str> = text.map(|t| t.split('\n').collect()).unwrap_or_default();
    if l.last() == Some(&"") {
        l.pop();
    }
    l
}

pub fn diffs(table: &str, diff: &str, old: &HashMap<String, String>, new: &HashMap<String, String>, most: usize) -> Value {
    // A file's definitions: an id, and its lines on each side (0 for none).
    let mut ranges: HashMap<&str, Vec<(String, [u32; 4])>> = HashMap::new();
    for r in table.split('\n').map(|l| l.split('\t').collect::<Vec<_>>()).filter(|r| r.len() >= 15 && r[1] != "file" && !r[7].is_empty()) {
        let id = if r[3].is_empty() { format!("{}#{}", r[0], r[2]) } else { format!("{}#{}.{}", r[0], r[3], r[2]) };
        let n = |i: usize| r[i].parse().unwrap_or(0);
        let at = if r[6] == "new" { [n(7), n(8), n(13), n(14)] } else { [0, 0, n(7), n(8)] };
        ranges.entry(r[0]).or_default().push((id, at));
    }
    let hunks = hunks::read(diff);
    let mut by_id: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for (path, defs) in &ranges {
        let Some(h) = hunks.get(*path) else { continue };
        let (was, now) = (lines(old.get(*path)), lines(new.get(*path)));
        let (mut o, mut n) = (1u32, 1u32);
        while (o as usize) <= was.len() || (n as usize) <= now.len() {
            let (mark, text) = if h.del.contains(&o) {
                ("-", was.get(o as usize - 1))
            } else if h.add.contains(&n) {
                ("+", now.get(n as usize - 1))
            } else {
                (" ", now.get(n as usize - 1))
            };
            for (id, [na, nz, oa, oz]) in defs {
                if (mark != "-" && *na > 0 && n >= *na && n <= *nz) || (mark != "+" && *oa > 0 && o >= *oa && o <= *oz) {
                    let l = by_id.entry(id).or_default();
                    if l.len() < most {
                        l.push(json!({"mark": mark, "old": if mark == "+" { String::new() } else { o.to_string() }, "new": if mark == "-" { String::new() } else { n.to_string() }, "text": text.copied().unwrap_or("")}));
                    }
                }
            }
            if mark != "+" {
                o += 1;
            }
            if mark != "-" {
                n += 1;
            }
        }
    }
    Value::Object(by_id.into_iter().map(|(k, v)| (k.to_string(), Value::Array(v))).collect::<Map<_, _>>())
}
