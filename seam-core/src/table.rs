//! The table: one row a listed definition, in reading order, members right
//! after their class, then one row a file it reads no definitions from. What
//! build() in bin/seam does, without git: the two sides of every changed file
//! and the diff between them are handed in.
use crate::decls::{Decl, Rules};
use crate::hunks::{self, FileHunks};
use crate::imports::Imports;
use crate::kinds::kind_of;
use std::collections::{HashMap, HashSet};

pub struct Change<'a> {
    /// `git diff --name-status --no-renames`: a status, a tab, a path.
    pub status: &'a str,
    /// `git diff --numstat -M`, for the files no definition is read from.
    pub numstat: &'a str,
    /// The diff of the two sides. -U0 is all it needs, more context is read.
    pub diff: &'a str,
    /// Each side's files by path; one too big to read by name is left out.
    pub old: &'a HashMap<String, String>,
    pub new: &'a HashMap<String, String>,
    /// What each path is (tests, gen, cfg, or nothing for source), when the
    /// repo knows better than kind_of.
    pub kinds: Option<&'a HashMap<String, String>>,
    pub max_bytes: usize,
}

struct D {
    new: bool,
    path: String,
    rule: String,
    /// Where it starts once the comment above it is its own, and where its
    /// keyword is.
    a: u32,
    d: u32,
    z: u32,
    name: String,
    bare: String,
    head: String,
    text: String,
    par: String,
}

#[derive(Default)]
struct Row {
    i: usize,
    change: &'static str,
    add: u32,
    del: u32,
    open: u32,
    top: bool,
    exported: String,
    uses: Vec<usize>,
    new_uses: String,
    root: bool,
    sortkey: String,
}

fn is_name(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// The names a definition's text names: how many times each, and the order it
/// first names them. Not after a dot, since runner.run() is a property of
/// runner and not the run declared here, but after a spread; and not before a
/// colon or a question mark, where the name is a key or a typed property.
/// Every other character is a boundary.
fn mentions(text: &str) -> (HashMap<String, i32>, HashMap<String, u32>) {
    let (mut cnt, mut first): (HashMap<String, i32>, HashMap<String, u32>) = Default::default();
    let mut s = String::with_capacity(text.len());
    let mut last = '\0';
    for c in text.replace("...", " ").chars() {
        let c = match c {
            ' ' | '\t' | '\r' | '\n' | '\x0b' | '\x0c' => ' ',
            c if is_name(c) || matches!(c, '.' | ':' | '?') => c,
            _ => '\t',
        };
        if (c == ' ' || c == '\t') && c == last {
            continue;
        }
        s.push(c);
        last = c;
    }
    let (mut prev, mut n) = (String::new(), 0);
    for chunk in s.split(' ').filter(|c| !c.is_empty()) {
        // A chunk opening with : or ? unmakes the name before it, which is
        // then a key or a typed property.
        if chunk.starts_with([':', '?']) && !prev.is_empty() {
            let c = cnt.entry(prev.clone()).or_default();
            *c -= 1;
            if *c <= 0 {
                cnt.remove(&prev);
                first.remove(&prev);
            }
        }
        prev.clear();
        let pieces: Vec<&str> = chunk.split('\t').collect();
        for (h, piece) in pieces.iter().enumerate() {
            if piece.is_empty() {
                continue;
            }
            // Split on runs of : and ?, the way awk's split does.
            let mut segs: Vec<&str> = vec![];
            let (mut from, mut in_sep) = (0, false);
            for (at, c) in piece.char_indices() {
                let sep = c == ':' || c == '?';
                if sep && !in_sep {
                    segs.push(&piece[from..at]);
                }
                if !sep && in_sep {
                    from = at;
                }
                in_sep = sep;
            }
            segs.push(if in_sep { "" } else { &piece[from..] });
            for (j, seg) in segs.iter().enumerate() {
                if !seg.starts_with(is_name) || j + 1 < segs.len() {
                    continue;
                }
                let tok = &seg[..seg.find(|c| !is_name(c)).unwrap_or(seg.len())];
                *cnt.entry(tok.to_string()).or_default() += 1;
                first.entry(tok.to_string()).or_insert_with(|| {
                    n += 1;
                    n
                });
                if h + 1 == pieces.len() {
                    prev = tok.to_string();
                }
            }
        }
    }
    (cnt, first)
}

/// The new path of a `git diff --numstat -M` record, which writes a rename as
/// "old => new" or, where the paths share ends, "dir/{old => new}.ts".
fn numstat_path(p: &str) -> String {
    if !p.contains(" => ") {
        return p.to_string();
    }
    if let (Some(a), Some(b)) = (p.find('{'), p.find('}')) {
        if a < b && p[a..b].contains(" => ") {
            let mid = &p[a + 1..b];
            let mid = &mid[mid.rfind(" => ").unwrap() + 4..];
            return format!("{}{}{}", &p[..a], mid, &p[b + 1..]).replace("//", "/");
        }
    }
    p[p.rfind(" => ").unwrap() + 4..].to_string()
}

pub fn table(rules: &Rules, c: &Change) -> String {
    let status: Vec<(&str, &str)> = c.status.lines().filter_map(|l| l.split_once('\t')).collect();
    let read = |side: &HashMap<String, String>, p: &str| rules.lang_of(p).is_some() && side.get(p).is_some_and(|t| t.len() <= c.max_bytes);
    let mut new_paths: Vec<&str> = status.iter().filter(|(s, p)| *s != "D" && read(c.new, p)).map(|x| x.1).collect();
    let mut old_paths: Vec<&str> = status.iter().filter(|(s, p)| *s != "A" && *s != "?" && read(c.old, p)).map(|x| x.1).collect();
    new_paths.sort();
    old_paths.sort();
    let paths: HashSet<&str> = new_paths.iter().chain(&old_paths).copied().collect();

    // ast-grep walks a directory the way ripgrep does, so a file under a
    // dotted directory was never read by name. Kept, so that the same change
    // reads the same in both; see README before changing it.
    let hidden = |p: &str| p.split('/').any(|part| part.starts_with('.'));
    let mut found: Vec<(bool, Decl)> = vec![];
    for (new, list, side) in [(false, &old_paths, c.old), (true, &new_paths, c.new)] {
        let mut ds = vec![];
        for p in list.iter().filter(|p| !hidden(p)) {
            rules.scan(p, &side[*p], &mut ds);
        }
        found.extend(ds.into_iter().map(|d| (new, d)));
    }

    // Each file keeps to its kind and its language.
    let group = |p: &str| {
        let kind = c.kinds.and_then(|k| k.get(p).map(String::as_str)).unwrap_or_else(|| kind_of(p));
        format!("{kind}\x1c{}", rules.lang_of(p).map(|l| l.name.as_str()).unwrap_or(""))
    };
    // A name in a file written in ES modules is another file's definition
    // only if the file imports it from there; in its own file, or in another
    // language, the name is enough.
    let imports: HashMap<(bool, &str), Imports> = [(false, &old_paths, c.old), (true, &new_paths, c.new)]
        .into_iter()
        .flat_map(|(new, list, side)| list.iter().filter(|p| rules.lang_of(p).is_some_and(|l| l.imports)).map(move |p| ((new, *p), Imports::read(&side[*p]))))
        .collect();
    let can_mean = |new: bool, from: &str, name: &str, to: &str| from == to || imports.get(&(new, from)).is_none_or(|i| i.brings(name, from, to));
    let mut hunks = hunks::read(c.diff);
    hunks.retain(|f, _| paths.contains(f.as_str()));
    let none = FileHunks::default();
    let hunk = |p: &str| hunks.get(p).unwrap_or(&none);

    let mut exported: HashSet<(bool, String, u32)> = HashSet::new();
    let mut cend: HashMap<(bool, String, u32), u32> = HashMap::new();
    let mut ds: Vec<D> = vec![];
    for (new, d) in found {
        match d.rule.as_str() {
            "export" => {
                exported.insert((new, d.path, d.start));
            }
            // Comments, by the line each ends on; two on one line keep the first.
            "comment" => {
                let e = cend.entry((new, d.path, d.end)).or_insert(d.start);
                *e = (*e).min(d.start);
            }
            _ => ds.push(D { new, path: d.path, rule: d.rule, a: d.start, d: d.start, z: d.end, bare: d.name.clone(), name: d.name, head: d.head, text: d.text, par: String::new() }),
        }
    }
    let n = ds.len();
    let member = |d: &D| d.rule == "method" || d.rule == "field";
    // A member belongs to the definition around it on its side. Only the
    // declarations of its own file on its own side can hold it.
    let mut in_file: HashMap<(bool, &str), Vec<usize>> = HashMap::new();
    for (i, d) in ds.iter().enumerate() {
        in_file.entry((d.new, d.path.as_str())).or_default().push(i);
    }
    let parents: Vec<Option<String>> = ds
        .iter()
        .map(|d| {
            if !member(d) {
                return Some(String::new());
            }
            in_file[&(d.new, d.path.as_str())].iter().map(|&j| &ds[j]).find(|p| !member(p) && p.a <= d.a && d.z <= p.z).map(|p| p.name.clone())
        })
        .collect();
    drop(in_file);
    for (d, p) in ds.iter_mut().zip(parents) {
        match p {
            Some(p) => d.par = p,
            None => d.rule.clear(),
        }
    }
    // The comment block right above a declaration belongs to it. It runs up
    // while each comment ends on the line before, so a blank line stops it,
    // and so does a comment that trails the declaration before.
    let ends: HashSet<(bool, &str, u32)> = ds.iter().filter(|d| !d.rule.is_empty()).map(|d| (d.new, d.path.as_str(), d.z)).collect();
    let tops: Vec<u32> = ds
        .iter()
        .map(|d| {
            let mut a = d.a;
            if d.rule.is_empty() {
                return a;
            }
            while let Some(&s) = cend.get(&(d.new, d.path.clone(), a.wrapping_sub(1))) {
                if ends.contains(&(d.new, d.path.as_str(), s)) {
                    break;
                }
                a = s;
            }
            a
        })
        .collect();
    drop(ends);
    for (d, a) in ds.iter_mut().zip(tops) {
        d.a = a;
    }
    // TypeScript keeps types and values apart, so a type may share its name
    // with a const. The type is then named "type X" on both sides, so the two
    // are two symbols; bare keeps the name its users mention.
    let isval: HashSet<(String, String)> = ds.iter().filter(|d| !d.rule.is_empty() && d.rule != "type" && d.rule != "interface").map(|d| (d.path.clone(), d.name.clone())).collect();
    for d in ds.iter_mut() {
        if (d.rule == "type" || d.rule == "interface") && isval.contains(&(d.path.clone(), d.name.clone())) {
            d.name = format!("type {}", d.name);
        }
    }
    let key = |d: &D| format!("{}\x1c{}\x1c{}", d.path, d.par, d.name);
    let (mut old, mut new): (HashMap<String, usize>, HashMap<String, usize>) = Default::default();
    for (i, d) in ds.iter().enumerate().filter(|(_, d)| !d.rule.is_empty()) {
        if d.new { &mut new } else { &mut old }.insert(key(d), i);
    }
    let count = |i: usize| hunk(&ds[i].path).lines(ds[i].new, ds[i].a, ds[i].z);
    let touched = |i: usize| hunk(&ds[i].path).touched(ds[i].new, ds[i].a, ds[i].z);

    // One row a symbol touched on either side, keyed by file, class and name.
    let mut rows: Vec<Row> = vec![];
    for i in 0..n {
        let d = &ds[i];
        let k = key(d);
        if d.rule.is_empty() || !d.new || new[&k] != i {
            continue;
        }
        let row = match old.get(&k) {
            Some(&o) => {
                if !touched(i) && !touched(o) {
                    continue;
                }
                Row { i, change: if d.head != ds[o].head { "signature" } else { "body" }, add: count(i), del: count(o), ..Default::default() }
            }
            None => {
                if !touched(i) {
                    continue;
                }
                Row { i, change: "added", add: count(i), ..Default::default() }
            }
        };
        rows.push(Row { open: d.d, ..row });
    }
    for i in 0..n {
        let d = &ds[i];
        let k = key(d);
        if d.rule.is_empty() || d.new || old[&k] != i || new.contains_key(&k) || !touched(i) {
            continue;
        }
        rows.push(Row { i, change: "removed", del: count(i), open: hunk(&d.path).at.get(&d.d).copied().unwrap_or(1), ..Default::default() });
    }
    let m = rows.len();
    let mut at_row: HashMap<String, usize> = HashMap::new();
    for (r, row) in rows.iter_mut().enumerate() {
        let d = &ds[row.i];
        row.top = d.par.is_empty();
        if row.top {
            at_row.insert(key(d), r);
            row.exported = if exported.contains(&(d.new, d.path.clone(), d.d)) { "1" } else { "0" }.into();
        }
    }
    for r in 0..m {
        if !rows[r].top {
            let d = &ds[rows[r].i];
            rows[r].exported = at_row.get(&format!("{}\x1c\x1c{}", d.path, d.par)).map(|&p| rows[p].exported.clone()).unwrap_or_default();
        }
    }
    // A const and a type of one name where one mentions the other (the type
    // is typeof the const) read as one thing: users of the name go to the
    // outer one, and the inner sits under it.
    let mut pairat: HashMap<(&str, &str, bool), usize> = HashMap::new();
    for (r, row) in rows.iter().enumerate().filter(|(_, row)| row.top) {
        let d = &ds[row.i];
        pairat.insert((&d.path, &d.bare, d.name == d.bare), r);
    }
    let mut inner: HashMap<usize, usize> = HashMap::new();
    for (r, row) in rows.iter().enumerate() {
        let d = &ds[row.i];
        if !row.top || d.name == d.bare {
            continue;
        }
        let Some(&q) = pairat.get(&(d.path.as_str(), d.bare.as_str(), true)) else { continue };
        // A declaration names itself once, so the two name each other only
        // on a second mention.
        if mentions(&d.text).0.get(&d.bare).copied().unwrap_or(0) >= 2 {
            inner.insert(q, r);
        } else if mentions(&ds[rows[q].i].text).0.get(&d.bare).copied().unwrap_or(0) >= 2 {
            inner.insert(r, q);
        }
    }
    // The changed definitions indexed by the name they are mentioned as, so
    // what one uses is a lookup rather than a pass against every other.
    let groups: HashMap<&str, String> = paths.iter().map(|p| (*p, group(p))).collect();
    let mut at_name: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for (r, row) in rows.iter().enumerate().filter(|(_, row)| row.top) {
        let d = &ds[row.i];
        if !d.bare.is_empty() && d.bare.chars().all(is_name) {
            at_name.entry((groups[d.path.as_str()].as_str(), d.bare.as_str())).or_default().push(r);
        }
    }
    let mut indeg = vec![0u32; m];
    for r in 0..m {
        if !rows[r].top {
            continue;
        }
        let d = &ds[rows[r].i];
        rows[r].sortkey = format!("{}{:08}{}", d.path, d.a, if d.new { "new" } else { "old" });
        let (cnt, first) = mentions(&d.text);
        let mut uses: Vec<(u32, usize)> = vec![];
        for (nm, &times) in &cnt {
            let Some(qs) = at_name.get(&(groups[d.path.as_str()].as_str(), nm.as_str())) else { continue };
            for &q in qs {
                if q == r || inner.get(&q).is_some_and(|&x| x != r) || (*nm == d.bare && times < 2) || !can_mean(d.new, &d.path, nm, &ds[rows[q].i].path) {
                    continue;
                }
                uses.push((first[nm], q));
                indeg[q] += 1;
            }
        }
        // In the order they are mentioned; two definitions of one name are
        // named in the same place, and take the order they are listed in.
        uses.sort();
        // An edge is new when the base side of the user did not have it: an
        // added user, or one that now calls what it did not.
        let was = old.get(&key(d)).map(|&o| mentions(&ds[o].text).0);
        let had = |q: usize| {
            let t = &ds[rows[q].i];
            was.as_ref().is_some_and(|w| w.contains_key(&t.bare) && can_mean(false, &d.path, &t.bare, &t.path))
        };
        let id = |q: usize| format!("{}#{}", ds[rows[q].i].path, ds[rows[q].i].name);
        rows[r].new_uses = uses.iter().filter(|(_, q)| !had(*q)).map(|&(_, q)| id(q)).collect::<Vec<_>>().join("|");
        rows[r].uses = uses.into_iter().map(|u| u.1).collect();
    }
    // Roots: nothing mentions them. A cycle has none, so whatever a root
    // cannot reach gets one of its own, the first by file and line.
    let mut stack: Vec<usize> = vec![];
    for r in 0..m {
        if rows[r].top && indeg[r] == 0 {
            rows[r].root = true;
            stack.push(r);
        }
    }
    let mut reach = vec![false; m];
    let mut by_key: Vec<usize> = (0..m).filter(|&r| rows[r].top).collect();
    by_key.sort_by(|&a, &b| rows[a].sortkey.cmp(&rows[b].sortkey).then(a.cmp(&b)));
    let mut next = 0;
    loop {
        while let Some(r) = stack.pop() {
            if !reach[r] {
                reach[r] = true;
                stack.extend(&rows[r].uses);
            }
        }
        while next < by_key.len() && reach[by_key[next]] {
            next += 1;
        }
        let Some(&pick) = by_key.get(next) else { break };
        rows[pick].root = true;
        stack.push(pick);
    }
    // Roots first, by file and line, then the rest the same way.
    let mut out = by_key;
    out.sort_by_key(|&r| !rows[r].root);

    // What no symbol claims: an import, a top-level statement, a file with no
    // declarations at all. It gets a file row after the symbols, with those
    // lines alone counted. A blank line belongs to nobody and counts for
    // nothing.
    let mut inrow: HashSet<(bool, &str, u32)> = HashSet::new();
    for row in &rows {
        let d = &ds[row.i];
        inrow.extend((d.a..=d.z).map(|l| (d.new, d.path.as_str(), l)));
        if d.new {
            if let Some(&o) = old.get(&key(d)) {
                inrow.extend((ds[o].a..=ds[o].z).map(|l| (false, d.path.as_str(), l)));
            }
        }
    }
    let mut loose: Vec<(&str, usize, usize)> = hunks
        .iter()
        .map(|(f, h)| {
            let f = f.as_str();
            (f, h.add.iter().filter(|l| !inrow.contains(&(true, f, **l)) && !h.blank_new.contains(l)).count(), h.del.iter().filter(|l| !inrow.contains(&(false, f, **l)) && !h.blank_old.contains(l)).count())
        })
        .filter(|x| x.1 + x.2 > 0)
        .collect();
    loose.sort();

    // Each member under the definition it belongs to.
    let mut kids: HashMap<(bool, &str, &str), Vec<usize>> = HashMap::new();
    for (q, row) in rows.iter().enumerate().filter(|(_, row)| !row.top) {
        let d = &ds[row.i];
        kids.entry((d.new, &d.path, &d.par)).or_default().push(q);
    }
    let mut t = String::new();
    let mut listed: HashSet<&str> = HashSet::new();
    let emit = |t: &mut String, r: usize| {
        let (row, d) = (&rows[r], &ds[rows[r].i]);
        let base = old.get(&key(d)).map(|&o| (ds[o].a.to_string(), ds[o].z.to_string())).unwrap_or_default();
        let uses: Vec<String> = row.uses.iter().map(|&q| format!("{}#{}", ds[rows[q].i].path, ds[rows[q].i].name)).collect();
        let side = if d.new { "new" } else { "old" };
        let nums = [d.a, d.z, row.open, row.add, row.del].map(|x| x.to_string());
        let f: [&str; 20] = [&d.path, &d.rule, &d.name, &d.par, &row.exported, row.change, side, &nums[0], &nums[1], &nums[2], &nums[3], &nums[4], &d.head, &base.0, &base.1, "", "", &uses.join("|"), if row.root { "1" } else { "0" }, &row.new_uses];
        t.push_str(&f.join("\t"));
        t.push('\n');
    };
    for &r in &out {
        let d = &ds[rows[r].i];
        listed.insert(&d.path);
        emit(&mut t, r);
        // Its members, if it has any, in line order.
        let mut ks = kids.get(&(d.new, d.path.as_str(), d.name.as_str())).cloned().unwrap_or_default();
        ks.sort_by_key(|&q| ds[rows[q].i].a);
        for q in ks {
            emit(&mut t, q);
        }
    }
    for (f, add, del) in loose {
        listed.insert(f);
        t.push_str(&format!("{f}\tfile\t\t\t\tbody\tnew\t\t\t\t{add}\t{del}\t\t\t\t\t\t\t\t\n"));
    }
    // Files this reads no symbols from, as file rows after the symbols, with
    // the file picker's counts: whatever has no row yet.
    let mut stat: HashMap<String, (&str, &str)> = HashMap::new();
    for l in c.numstat.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() >= 3 && !f[0].is_empty() && f[0].bytes().all(|b| b.is_ascii_digit()) {
            stat.insert(numstat_path(f[2]), (f[0], f[1]));
        }
    }
    for (s, p) in &status {
        if listed.contains(p) {
            continue;
        }
        let change = match *s {
            "A" | "?" => "added",
            "D" => "removed",
            _ => "body",
        };
        let (add, del) = stat.get(*p).copied().unwrap_or(("", ""));
        t.push_str(&format!("{p}\tfile\t\t\t\t{change}\tnew\t\t\t\t{add}\t{del}\t\t\t\t\t\t\t\t\n"));
    }
    t
}
