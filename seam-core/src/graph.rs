//! The table as the graph a renderer draws (share/graph.jq), and the order a
//! reviewer reads it in (share/order.jq).
use crate::kinds::kind_of;
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

pub struct Ends<'a> {
    pub rev: &'a str,
    pub base: &'a str,
    pub head: &'a str,
    /// What the providers said: from, to, facet, note and who said it, tab
    /// separated, a line each.
    pub refs: &'a str,
    pub kinds: Option<&'a HashMap<String, String>>,
}

fn num(s: &str) -> Value {
    s.parse::<i64>().map(Value::from).unwrap_or(Value::Null)
}
fn flag(s: &str) -> Value {
    if s.is_empty() { Value::Null } else { Value::Bool(s == "1") }
}
fn text(s: &str) -> Value {
    if s.is_empty() { Value::Null } else { Value::from(s) }
}
fn is_name(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}
/// Whether a head names n: not after a dot or inside a longer name.
fn names(head: &str, n: &str) -> bool {
    !n.is_empty()
        && head.match_indices(n).any(|(i, _)| {
            let before = head[..i].chars().next_back();
            let after = head[i + n.len()..].chars().next();
            !before.is_some_and(|c| is_name(c) || c == '.') && !after.is_some_and(is_name)
        })
}

/// A definition a row, an edge a mention of one definition's name inside
/// another's text, with where the mention sits: in the user's own signature
/// (iface) or in its body.
pub fn graph(table: &str, e: &Ends) -> Value {
    let rows: Vec<Vec<&str>> = table.split('\n').filter(|l| !l.is_empty()).map(|l| l.split('\t').collect()).collect();
    let defid = |r: &Vec<&str>| if cell(r, 3).is_empty() { format!("{}#{}", r[0], cell(r, 2)) } else { format!("{}#{}.{}", r[0], cell(r, 3), cell(r, 2)) };
    let boxed = |p: &str| {
        let k = e.kinds.and_then(|k| k.get(p).map(String::as_str)).unwrap_or_else(|| kind_of(p));
        json!({"file": p, "kind": if k.is_empty() { "source" } else { k }})
    };
    let defs: Vec<&Vec<&str>> = rows.iter().filter(|r| cell(r, 1) != "file").collect();
    let isdef: HashSet<String> = defs.iter().map(|r| defid(r)).collect();
    let refs: Vec<Vec<&str>> = e.refs.split('\n').filter(|l| !l.is_empty()).map(|l| l.split('\t').collect::<Vec<_>>()).filter(|r| r.len() >= 2).collect();
    let reference = |r: &Vec<&str>| json!({"from": r[0], "to": r[1], "facet": if cell(r, 2) == "iface" { "iface" } else { "body" }, "note": text(cell(r, 3)), "via": r.get(4).map(|v| Value::from(*v)).unwrap_or(Value::Null)});
    let mut edges: Vec<Value> = vec![];
    for r in &defs {
        let new: Vec<&str> = cell(r, 19).split('|').collect();
        for to in cell(r, 17).split('|').filter(|t| !t.is_empty()) {
            let n = to.split_once('#').map(|x| x.1).unwrap_or(to);
            let n = n.strip_prefix("type ").unwrap_or(n);
            edges.push(json!({"from": defid(r), "to": to, "facet": if names(cell(r, 12), n) { "iface" } else { "body" }, "new": new.contains(&to)}));
        }
    }
    // A reference with a definition at both ends is an edge seam should have
    // had, so it joins the ones it found, with what said it.
    for r in refs.iter().filter(|r| isdef.contains(r[0]) && isdef.contains(r[1]) && r[0] != r[1]) {
        let mut v = reference(r);
        v["new"] = Value::Bool(false);
        edges.push(v);
    }
    json!({
        "rev": e.rev,
        "base": text(e.base),
        "head": text(e.head),
        "definitions": defs.iter().map(|r| json!({
            "id": defid(r), "path": r[0], "kind": cell(r, 1), "name": cell(r, 2), "class": text(cell(r, 3)),
            "exported": flag(cell(r, 4)), "change": cell(r, 5), "side": cell(r, 6),
            "start": num(cell(r, 7)), "end": num(cell(r, 8)), "open": num(cell(r, 9)),
            "added": num(cell(r, 10)), "removed": num(cell(r, 11)), "head": cell(r, 12),
            "base": if cell(r, 13).is_empty() { Value::Null } else { json!({"start": num(cell(r, 13)), "end": num(cell(r, 14))}) },
            "staged": flag(cell(r, 15)), "unstaged": flag(cell(r, 16)), "root": cell(r, 18) == "1", "box": boxed(r[0]),
        })).collect::<Vec<_>>(),
        "references": refs.iter().map(reference).collect::<Vec<_>>(),
        "edges": edges,
        "files": rows.iter().filter(|r| cell(r, 1) == "file").map(|r| json!({"path": r[0], "change": cell(r, 5), "added": num(cell(r, 10)), "removed": num(cell(r, 11)), "box": boxed(r[0])})).collect::<Vec<_>>(),
    })
}

fn cell<'a>(r: &[&'a str], i: usize) -> &'a str {
    r.get(i).copied().unwrap_or("")
}
fn id(d: &Value) -> &str {
    d["id"].as_str().unwrap_or("")
}

/// A member's id as its class's.
fn top(id: &str) -> &str {
    let Some(hash) = id.rfind('#') else { return id };
    match id.rfind('.') {
        Some(dot) if dot > hash => &id[..dot],
        _ => id,
    }
}

struct Net<'a> {
    pos: HashMap<&'a str, usize>,
    path: HashMap<&'a str, &'a str>,
    uses: HashMap<&'a str, Vec<&'a str>>,
    users: HashMap<&'a str, Vec<&'a str>>,
    sig: HashSet<(&'a str, &'a str)>,
}

#[derive(Default)]
struct Read<'a> {
    read: HashSet<&'a str>,
    at: HashMap<&'a str, i64>,
    met: HashMap<&'a str, i64>,
    left: HashMap<&'a str, i64>,
    order: Vec<&'a str>,
    unmet: Vec<&'a str>,
    loose: i64,
    most: i64,
    last: Option<&'a str>,
}

impl<'a> Net<'a> {
    fn of(&self, m: &HashMap<&'a str, Vec<&'a str>>, x: &str) -> Vec<&'a str> {
        m.get(x).cloned().unwrap_or_default()
    }
    fn unmet(&self, s: &Read, x: &str) -> bool {
        self.users.get(x).is_some_and(|u| !u.is_empty()) && s.met.get(x).copied().unwrap_or(0) == 0
    }
    /// The unread definitions reached from x along m.
    fn reach(&self, s: &Read, m: &HashMap<&'a str, Vec<&'a str>>, x: &'a str) -> BTreeSet<&'a str> {
        let (mut seen, mut queue) = (BTreeSet::new(), VecDeque::from([x]));
        while let Some(y) = queue.pop_front() {
            for z in self.of(m, y) {
                if !s.read.contains(z) && seen.insert(z) {
                    queue.push_back(z);
                }
            }
        }
        seen
    }
    /// With every one left unmet, the way in: a circle nothing else leads
    /// to, found by climbing from x to whatever mentions it and is not
    /// mentioned back, until nothing is.
    fn way_in(&self, s: &Read, x: &'a str) -> Vec<&'a str> {
        let (up, down) = (self.reach(s, &self.users, x), self.reach(s, &self.uses, x));
        match up.iter().find(|z| !down.contains(*z)) {
            Some(z) => self.way_in(s, z),
            None => {
                let mut all = up;
                all.insert(x);
                all.into_iter().collect()
            }
        }
    }
    fn thread(&self, ids: &[&'a str]) -> Read<'a> {
        let mut s = Read { left: ids.iter().map(|&i| (i, self.of(&self.uses, i).len() as i64)).collect(), ..Default::default() };
        while s.order.len() < ids.len() {
            let unread: Vec<&str> = ids.iter().copied().filter(|i| !s.read.contains(i)).collect();
            let mut ready: Vec<&str> = unread.iter().copied().filter(|x| !self.unmet(&s, x)).collect();
            if ready.is_empty() {
                ready = self.way_in(&s, unread[0]);
            }
            let x = ready
                .into_iter()
                .min_by_key(|&x| {
                    let readers: Vec<&str> = self.of(&self.users, x).into_iter().filter(|u| s.read.contains(u)).collect();
                    (
                        self.unmet(&s, x),
                        s.left[x] - readers.len() as i64,
                        -readers.iter().map(|r| s.at[r]).max().unwrap_or(-1),
                        !readers.iter().any(|r| self.sig.contains(&(*r, x))),
                        s.last.is_some_and(|l| self.path[x] != l),
                        self.pos[x],
                    )
                })
                .unwrap();
            if self.unmet(&s, x) {
                s.unmet.push(x);
            }
            s.loose += s.left[x] - s.met.get(x).copied().unwrap_or(0);
            s.read.insert(x);
            s.at.insert(x, s.order.len() as i64);
            s.order.push(x);
            s.last = Some(self.path[x]);
            for u in self.of(&self.users, x) {
                *s.left.entry(u).or_default() -= 1;
            }
            for t in self.of(&self.uses, x) {
                *s.met.entry(t).or_default() += 1;
            }
            s.most = s.most.max(s.loose);
        }
        s
    }
}

/// The graph with .reading: the definitions in the order a reviewer takes
/// them, as threads, and what that order leaves them carrying. A thread reads
/// top down, a definition before what it mentions; the order is the one that
/// leaves the fewest mentions of something not read yet at any point, as near
/// as a step at a time gets it. share/order.jq says how each step is chosen.
pub fn order(g: &mut Value) {
    let empty = vec![];
    let defs = g["definitions"].as_array().unwrap_or(&empty);
    let tops: Vec<&Value> = defs.iter().filter(|d| d["class"].is_null()).collect();
    let by: HashMap<&str, &Value> = tops.iter().map(|d| (id(d), *d)).collect();
    let kind = |i: &str| by[i]["box"]["kind"].as_str().unwrap_or("");
    let mut net = Net { pos: tops.iter().enumerate().map(|(i, d)| (id(d), i)).collect(), path: tops.iter().map(|d| (id(d), d["path"].as_str().unwrap_or(""))).collect(), uses: HashMap::new(), users: HashMap::new(), sig: HashSet::new() };
    let mut pairs: BTreeSet<(&str, &str)> = BTreeSet::new();
    for e in g["edges"].as_array().unwrap_or(&empty) {
        let (from, to) = (top(e["from"].as_str().unwrap_or("")), top(e["to"].as_str().unwrap_or("")));
        if from == to || !by.contains_key(from) || !by.contains_key(to) || kind(from) != kind(to) {
            continue;
        }
        // The ids the definitions hold, which outlive the edge.
        let (from, to) = (id(by[from]), id(by[to]));
        pairs.insert((from, to));
        if e["facet"] == "iface" {
            net.sig.insert((from, to));
        }
    }
    for &(from, to) in &pairs {
        net.uses.entry(from).or_default().push(to);
        net.users.entry(to).or_default().push(from);
    }
    // Definitions no mention joins are read apart, one group through before
    // the next, and each kind of file keeps to its own. Source first; a
    // definition joined to nothing after the rest of its kind.
    let (mut done, mut groups): (HashSet<&str>, Vec<Vec<&str>>) = Default::default();
    for d in &tops {
        if done.contains(id(d)) {
            continue;
        }
        let (mut seen, mut queue, mut list) = (HashSet::from([id(d)]), VecDeque::from([id(d)]), vec![]);
        while let Some(x) = queue.pop_front() {
            list.push(x);
            for y in net.of(&net.uses, x).into_iter().chain(net.of(&net.users, x)) {
                if seen.insert(y) {
                    queue.push_back(y);
                }
            }
        }
        list.sort_by_key(|x| net.pos[x]);
        done.extend(&list);
        groups.push(list);
    }
    let rank = |k: &str| ["source", "tests", "gen", "cfg"].iter().position(|x| *x == k).unwrap_or(4);
    groups.sort_by_key(|c| (rank(kind(c[0])), c.len() == 1));
    let threads: Vec<Read> = groups.iter().map(|c| net.thread(c)).collect();
    let all: Vec<&str> = threads.iter().flat_map(|t| t.order.iter().copied()).collect();
    let mut reading = Map::new();
    reading.insert("threads".into(), json!(threads.iter().map(|t| &t.order).collect::<Vec<_>>()));
    reading.insert("unmet".into(), json!(threads.iter().flat_map(|t| t.unmet.iter()).collect::<Vec<_>>()));
    reading.insert("loose".into(), json!(threads.iter().map(|t| t.most).max().unwrap_or(0)));
    reading.insert("hops".into(), json!(all.windows(2).filter(|w| net.path[w[0]] != net.path[w[1]]).count()));
    g["reading"] = Value::Object(reading);
}
