//! A review's model, from both sides of each changed file and the diff
//! between them: every file as its new text with the diff drawn on it, the
//! changed symbols as a tree of what uses what (seam's table), and for each
//! row a reviewer can rest on, the hunks it walks and the mark v leaves.
//! No IO.
use seam_core::kinds::kind_of;
use seam_core::{Lang, Rules};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::OnceLock;

/// What git says of an MR's two commits.
#[derive(Default)]
pub struct Sides {
    /// `git diff --name-status --no-renames`.
    pub status: String,
    /// `git diff -U0 --no-renames`.
    pub diff: String,
    /// Each side's text files by path; a binary or huge one is left out.
    pub old: HashMap<String, String>,
    pub new: HashMap<String, String>,
}

#[derive(Debug, Default)]
pub struct Review {
    /// By path.
    pub files: Vec<File>,
    pub symbols: Vec<Symbol>,
    /// The changed symbols nothing else changed uses, in seam's reading order.
    pub roots: Vec<usize>,
    /// Files with changed lines no symbol claims, with those lines' counts.
    pub loose: Vec<Loose>,
}

#[derive(Debug, Default)]
pub struct File {
    pub path: String,
    /// git's status letter: A, M, D.
    pub status: char,
    /// seam's word for what it is: "tests", "gen", "cfg", or "" for source.
    pub kind: &'static str,
    pub adds: u32,
    pub dels: u32,
    /// The new text with the old drawn in where it went; a removed file is
    /// all removed lines. Empty when a side it has can't be read as text.
    pub lines: Vec<Line>,
    /// Runs of changed lines, as ranges of `lines`.
    pub hunks: Vec<Range<usize>>,
    /// Both sides it has are text small enough to show.
    pub readable: bool,
}

impl File {
    /// Tests and generated files stay out of the list until asked for.
    pub fn aside(&self) -> bool {
        matches!(self.kind, "tests" | "gen")
    }

    /// The MR removes it: what is shown is its old text.
    pub fn gone(&self) -> bool {
        self.status == 'D'
    }

    /// A line by its number: on the new side if `new` is given (a line
    /// still there), else a removed one by its old number.
    pub fn line_at(&self, new: Option<u32>, old: Option<u32>) -> Option<&Line> {
        match (new, old) {
            (Some(n), _) => self
                .lines
                .iter()
                .find(|l| l.mark != Mark::Del && l.new == n),
            (None, Some(o)) => self
                .lines
                .iter()
                .find(|l| l.mark == Mark::Del && l.old == o),
            (None, None) => None,
        }
    }

    /// A line still there, with its old-side number as a diff counts it:
    /// its own when it has one, else the next old line's, which is where
    /// GitLab's line codes put an added line (0 in a file with no old side).
    pub fn old_pos(&self, new: u32) -> Option<(u32, &Line)> {
        let at = self
            .lines
            .iter()
            .position(|l| l.mark != Mark::Del && l.new == new)?;
        let line = &self.lines[at];
        let old = match line.mark {
            Mark::Add if self.lines.iter().all(|l| l.old == 0) => 0,
            Mark::Add => self.lines[..at].iter().map(|l| l.old).max().unwrap_or(0) + 1,
            _ => line.old,
        };
        Some((old, line))
    }
}

/// What the change did to a line. The numbers are fixed because they go
/// into the hash of a viewed mark, which is kept on disk: reordering these
/// would quietly unmark everything you viewed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Same = 0,
    Add = 1,
    Del = 2,
}

#[derive(Debug, PartialEq)]
pub struct Line {
    pub mark: Mark,
    /// The line's number on each side; 0 on a side it isn't on.
    pub old: u32,
    pub new: u32,
    pub text: String,
    /// The bytes of `text` that changed, when the line was changed rather
    /// than added or removed whole.
    pub words: Option<Range<usize>>,
    /// It only moved with its symbol (Symbol::moved): the same line is at
    /// the symbol's other place, so there is nothing in it to read. To git
    /// and GitLab it is still added or removed, as `mark` says.
    pub moved: bool,
}

#[derive(Debug)]
pub struct Symbol {
    pub file: usize,
    /// As seam names it; a type that shares its name with a const is
    /// "type X".
    pub name: String,
    /// What it is, as seam's rule for the language names it: fn, class,
    /// interface, type, const, method, and so on.
    pub kind: String,
    /// What it is across reviews and pushes: path, class and name. Also
    /// the first half of its viewed mark, kept on disk.
    pub key: String,
    pub adds: u32,
    pub dels: u32,
    /// Its lines on each side, as seam found them; empty on a side it is
    /// not on. A moved symbol has two places, and what lies between them is
    /// not its.
    pub new: Range<u32>,
    pub old: Range<u32>,
    /// The line its keyword is on, on the new side: `new` begins with the
    /// doc comment above it. For a removed symbol, where its hunk lands.
    pub at: u32,
    /// The changed symbols it uses, in the order it mentions them.
    pub uses: Vec<usize>,
    /// Its changed members, for a class.
    pub members: Vec<usize>,
    /// The file of its other place, when the change took it from one file
    /// and put it in another (mark_moves).
    pub moved: Option<usize>,
}

impl Symbol {
    /// Whether a line is this symbol's: a removed line by its old number,
    /// any other by its new one. Everything that asks what a symbol changed
    /// (its hunks, its threads, its viewed mark) asks this.
    pub fn owns(&self, l: &Line) -> bool {
        match l.mark {
            Mark::Del => self.old.contains(&l.old),
            _ => self.new.contains(&l.new),
        }
    }
}

#[derive(Debug)]
pub struct Loose {
    pub file: usize,
    pub adds: u32,
    pub dels: u32,
}

/// A row of the review's list: a symbol, a file's lines outside any
/// symbol, or a whole file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListRow {
    Symbol(usize),
    Loose(usize),
    File(usize),
}

/// seam's language rules, built once; the files are compiled in.
pub fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| {
        let langs = [
            (
                "python.yml",
                include_str!("../seam-core/languages/python.yml"),
            ),
            (
                "rust.yml",
                include_str!("../seam-core/languages/rust.yml"),
            ),
            (
                "typescript.yml",
                include_str!("../seam-core/languages/typescript.yml"),
            ),
        ];
        let langs = langs
            .iter()
            .map(|(n, y)| Lang::new(n, y))
            .collect::<Result<Vec<_>, _>>()
            .expect("seam's language files");
        Rules::new(langs).expect("seam's language files")
    })
}

/// Files no bigger than this are read for symbols, as seam does.
const SEAM_MAX: usize = 262_144;

pub fn review(sides: &Sides) -> Review {
    let mut files = read_files(sides);
    let table = seam_table(sides, &files);
    let (mut symbols, mut roots, loose) = read_symbols(&table, &files);
    mark_moves(&mut files, &mut symbols, &mut roots);
    Review {
        files,
        symbols,
        roots,
        loose,
    }
}

/// Every changed file, by path, with the diff drawn on it.
fn read_files(sides: &Sides) -> Vec<File> {
    // Per file, which old lines went and which new ones came.
    let changed = seam_core::hunks::read(&sides.diff);
    let mut files: Vec<File> = sides
        .status
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(letter, path)| {
            let status = letter.chars().next().unwrap_or('M');
            let (old, new) = (sides.old.get(path), sides.new.get(path));
            // A side the status says is there but git couldn't give as text
            // leaves nothing to draw the diff on.
            let readable = (status == 'A' || old.is_some()) && (status == 'D' || new.is_some());
            let changed_here = changed.get(path);
            let (adds, dels) =
                changed_here.map_or((0, 0), |c| (c.add.len() as u32, c.del.len() as u32));
            let (lines, hunks) = match changed_here {
                Some(c) if readable => interleave(
                    old.map_or("", String::as_str),
                    new.map_or("", String::as_str),
                    &c.add,
                    &c.del,
                ),
                _ => (vec![], vec![]),
            };
            File {
                path: path.to_string(),
                status,
                kind: kind_of(path),
                adds,
                dels,
                lines,
                hunks,
                readable,
            }
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// seam's table of the change: a tab-separated line per changed symbol,
/// then one per file with lines no symbol claims.
fn seam_table(sides: &Sides, files: &[File]) -> String {
    // seam wants numstat only for the files it reads no symbols from.
    let numstat: String = files
        .iter()
        .map(|f| format!("{}\t{}\t{}\n", f.adds, f.dels, f.path))
        .collect();
    seam_core::table(
        rules(),
        &seam_core::Change {
            status: &sides.status,
            numstat: &numstat,
            diff: &sides.diff,
            old: &sides.old,
            new: &sides.new,
            kinds: None,
            max_bytes: SEAM_MAX,
        },
    )
}

/// A line of seam's table, its columns named (seam-core's table.rs writes
/// them, in `emit`).
struct TableRow<'a> {
    path: &'a str,
    /// "file" for lines no symbol claims; else the kind of declaration.
    rule: &'a str,
    name: &'a str,
    /// The class a member is in; empty for anything else.
    parent: &'a str,
    /// Its lines on the side it is on: the new side, unless it was removed.
    on_new_side: bool,
    lines: Range<u32>,
    /// Where its keyword is, on the new side.
    open: u32,
    /// For a symbol still there, its lines before the change.
    base_lines: Range<u32>,
    adds: u32,
    dels: u32,
    /// The symbols it uses, as "path#name" ids joined by "|".
    uses: &'a str,
    /// Nothing else changed uses it, so it heads the list.
    root: bool,
}

impl<'a> TableRow<'a> {
    fn parse(line: &'a str) -> Option<TableRow<'a>> {
        let c: Vec<&str> = line.split('\t').collect();
        if c.len() < 19 {
            return None;
        }
        let num = |i: usize| c[i].parse::<u32>().unwrap_or(0);
        // A first and last line, or none for a side it isn't on.
        let lines = |first: usize, last: usize| {
            if c[first].is_empty() {
                0..0
            } else {
                num(first)..num(last) + 1
            }
        };
        Some(TableRow {
            path: c[0],
            rule: c[1],
            name: c[2],
            parent: c[3],
            on_new_side: c[6] == "new",
            lines: lines(7, 8),
            open: num(9),
            base_lines: lines(13, 14),
            adds: num(10),
            dels: num(11),
            uses: c[17],
            root: c[18] == "1",
        })
    }
}

/// The symbols in seam's table, which of them head the list, and the files
/// with lines no symbol claims.
fn read_symbols(table: &str, files: &[File]) -> (Vec<Symbol>, Vec<usize>, Vec<Loose>) {
    let file_index: HashMap<&str, usize> = files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.path.as_str(), i))
        .collect();
    let mut symbols: Vec<Symbol> = vec![];
    let mut roots = vec![];
    let mut loose = vec![];
    // Top-level symbols by the id seam names them by in a row's uses.
    let mut by_id: HashMap<String, usize> = HashMap::new();
    let mut uses: Vec<(usize, &str)> = vec![];
    let mut class: Option<usize> = None;
    for row in table.lines().filter_map(TableRow::parse) {
        let Some(&file) = file_index.get(row.path) else {
            continue;
        };
        if row.rule == "file" {
            let (adds, dels) = (row.adds, row.dels);
            loose.push(Loose { file, adds, dels });
            continue;
        }
        let (new, old) = if row.on_new_side {
            (row.lines.clone(), row.base_lines.clone())
        } else {
            (0..0, row.lines.clone())
        };
        let key = if row.parent.is_empty() {
            format!("{}#{}", row.path, row.name)
        } else {
            format!("{}#{}.{}", row.path, row.parent, row.name)
        };
        let i = symbols.len();
        symbols.push(Symbol {
            file,
            name: row.name.to_string(),
            kind: row.rule.to_string(),
            key: key.clone(),
            adds: row.adds,
            dels: row.dels,
            new,
            old,
            at: row.open,
            uses: vec![],
            members: vec![],
            moved: None,
        });
        // seam lists a class's members right after it.
        if row.parent.is_empty() {
            by_id.insert(key, i);
            class = Some(i);
            uses.push((i, row.uses));
            if row.root {
                roots.push(i);
            }
        } else if let Some(c) = class {
            symbols[c].members.push(i);
        }
    }
    for (i, ids) in uses {
        symbols[i].uses = ids
            .split('|')
            .filter_map(|id| by_id.get(id).copied())
            .collect();
    }
    (symbols, roots, loose)
}

/// The new text with the old lines put back where they went, each run of
/// changes a hunk, and the words of a changed line marked when a hunk
/// changed as many lines as it took out.
fn interleave(
    old: &str,
    new: &str,
    added: &HashSet<u32>,
    removed: &HashSet<u32>,
) -> (Vec<Line>, Vec<Range<usize>>) {
    let (was, now): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    let text = |side: &[&str], n: u32| side.get(n as usize - 1).unwrap_or(&"").to_string();
    let mut lines = vec![];
    // The next line's number on each side.
    let (mut o, mut n) = (1u32, 1u32);
    while (o as usize) <= was.len() || (n as usize) <= now.len() {
        let (mark, old, new, text) = if removed.contains(&o) {
            (Mark::Del, o, 0, text(&was, o))
        } else if added.contains(&n) {
            (Mark::Add, 0, n, text(&now, n))
        } else {
            (Mark::Same, o, n, text(&now, n))
        };
        if mark != Mark::Add {
            o += 1;
        }
        if mark != Mark::Del {
            n += 1;
        }
        lines.push(Line {
            mark,
            old,
            new,
            text,
            words: None,
            moved: false,
        });
    }
    let hunks = runs(&lines, |_| true);
    // In a hunk that took out as many lines as it put in, the n-th line out
    // was changed into the n-th line in.
    for h in &hunks {
        let out = lines[h.clone()]
            .iter()
            .filter(|l| l.mark == Mark::Del)
            .count();
        if out * 2 != h.len() {
            continue;
        }
        for k in 0..out {
            let (gone, came) = (h.start + k, h.start + out + k);
            if let Some((was, now)) = middle(&lines[gone].text, &lines[came].text) {
                lines[gone].words = Some(was);
                lines[came].words = Some(now);
            }
        }
    }
    (lines, hunks)
}

/// A symbol the change took from one place and put at another is read as
/// one move: the lines both places share, in order, are marked `moved`, so
/// they are in no hunk and no count, and only what differs is left to read.
/// A place is another part of its file, as seam found it, or another file,
/// when one symbol of its name and kind went and one came and most of the
/// text is the same. The place it left in another file then leaves the
/// list, unless lines were dropped on the way.
fn mark_moves(files: &mut [File], symbols: &mut [Symbol], roots: &mut Vec<usize>) {
    // Its lines of one side, by where they are in its file's.
    let side = |files: &[File], s: &Symbol, mark: Mark| -> Vec<usize> {
        let lines = files[s.file].lines.iter().enumerate();
        lines.filter(|(_, l)| l.mark == mark && s.owns(l)).map(|(i, _)| i).collect()
    };
    let named = |s: &Symbol| (s.kind.clone(), s.key.split_once('#').map_or("", |(_, name)| name).to_string());
    let (mut went, mut came): (HashMap<_, Vec<usize>>, HashMap<_, Vec<usize>>) = Default::default();
    // From where to where, as symbols: one, twice, for a move in its file.
    let mut pairs = vec![];
    for (i, s) in symbols.iter().enumerate() {
        if s.new.is_empty() {
            went.entry(named(s)).or_default().push(i);
        } else if s.old.is_empty() {
            came.entry(named(s)).or_default().push(i);
        } else if !files[s.file].lines.iter().any(|l| l.mark == Mark::Same && s.new.contains(&l.new) && s.old.contains(&l.old)) {
            // None of it stayed where it was.
            pairs.push((i, i));
        }
    }
    for (name, to) in &came {
        if let ([to], Some([from])) = (to.as_slice(), went.get(name).map(Vec::as_slice)) {
            pairs.push((*from, *to));
        }
    }
    let mut gone = vec![];
    for (from, to) in pairs {
        let (out, into) = (side(files, &symbols[from], Mark::Del), side(files, &symbols[to], Mark::Add));
        let text = |file: usize, at: &[usize]| -> Vec<String> { at.iter().map(|&i| files[file].lines[i].text.trim_end().to_string()).collect() };
        let (was, now) = (text(symbols[from].file, &out), text(symbols[to].file, &into));
        let same = common(&was, &now);
        // Counted in lines with something on them: blank ones always match.
        let solid = |t: &[String]| t.iter().filter(|l| !l.is_empty()).count();
        let kept = same.iter().filter(|(i, _)| !was[*i].is_empty()).count();
        if kept == 0 || kept * 2 < solid(&was).min(solid(&now)) {
            continue;
        }
        for (i, j) in same {
            files[symbols[from].file].lines[out[i]].moved = true;
            files[symbols[to].file].lines[into[j]].moved = true;
        }
        if from != to {
            (symbols[from].moved, symbols[to].moved) = (Some(symbols[to].file), Some(symbols[from].file));
        }
        for i in [from, to] {
            let s = &symbols[i];
            let left = |mark: Mark| files[s.file].lines.iter().filter(|l| l.mark == mark && !l.moved && s.owns(l)).count() as u32;
            (symbols[i].adds, symbols[i].dels) = (left(Mark::Add), left(Mark::Del));
            files[symbols[i].file].hunks = runs(&files[symbols[i].file].lines, |_| true);
        }
        if from != to && symbols[from].dels == 0 {
            gone.push(from);
        }
    }
    roots.retain(|i| !gone.contains(i));
    for s in symbols.iter_mut() {
        s.uses.retain(|i| !gone.contains(i));
        s.members.retain(|i| !gone.contains(i));
    }
}

/// The lines two texts share, in order, as each one's place in both: their
/// longest common subsequence. None for texts too long to be worth it.
fn common(a: &[String], b: &[String]) -> Vec<(usize, usize)> {
    const MOST: usize = 4_000_000;
    if a.len() * b.len() > MOST {
        return vec![];
    }
    // best[i][j]: how many a[i..] and b[j..] share.
    let mut best = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            best[i][j] = if a[i] == b[j] { best[i + 1][j + 1] + 1 } else { best[i + 1][j].max(best[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, vec![]);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            out.push((i, j));
            (i, j) = (i + 1, j + 1);
        } else if best[i + 1][j] >= best[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// The runs of changed lines that `keep` keeps; a line that only moved is
/// no change.
fn runs(lines: &[Line], keep: impl Fn(&Line) -> bool) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = vec![];
    for (i, l) in lines.iter().enumerate() {
        if l.mark == Mark::Same || l.moved || !keep(l) {
            continue;
        }
        match out.last_mut() {
            Some(r) if r.end == i => r.end = i + 1,
            _ => out.push(i..i + 1),
        }
    }
    out
}

/// The middles of two lines that differ, between what they start and end
/// with in common; None when they share neither end, since marking the
/// words would only say the whole line again.
fn middle(a: &str, b: &str) -> Option<(Range<usize>, Range<usize>)> {
    let same_start: usize = a
        .chars()
        .zip(b.chars())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum();
    let same_end: usize = a[same_start..]
        .chars()
        .rev()
        .zip(b[same_start..].chars().rev())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum();
    (same_start + same_end > 0).then(|| {
        (
            same_start..a.len() - same_end,
            same_start..b.len() - same_end,
        )
    })
}

/// What has been said on an MR: its description, its threads, and the
/// drafts of yours not sent yet.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Talk {
    pub description: String,
    pub threads: Vec<Thread>,
    pub drafts: Vec<Draft>,
}

/// A comment thread, as GitLab has it: on a line of the diff, or on the
/// MR itself when it has no path.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Thread {
    pub id: String,
    pub path: String,
    /// The line it is on: on the new side, or else a removed line's old one.
    pub new_line: Option<u32>,
    pub old_line: Option<u32>,
    pub resolved: bool,
    /// A plain comment on the MR can't be; a thread on a line can.
    pub resolvable: bool,
    pub notes: Vec<Note>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Note {
    pub author: String,
    pub body: String,
}

/// A comment of yours waiting to be sent: on a line, under a thread, or on
/// the MR itself when it has neither.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Draft {
    pub id: u64,
    pub body: String,
    pub path: String,
    pub new_line: Option<u32>,
    pub old_line: Option<u32>,
    pub reply_to: Option<String>,
}

impl Review {
    pub fn file_of(&self, row: ListRow) -> usize {
        match row {
            ListRow::Symbol(i) => self.symbols[i].file,
            ListRow::Loose(i) => self.loose[i].file,
            ListRow::File(f) => f,
        }
    }

    /// The hunks ] and [ walk on a row, as ranges of its file's lines: a
    /// symbol's are the runs of changes it owns; a loose row's, the runs no
    /// symbol owns; a file's, all of them.
    pub fn hunks(&self, row: ListRow) -> Vec<Range<usize>> {
        let f = self.file_of(row);
        let lines = &self.files[f].lines;
        match row {
            ListRow::Symbol(i) => runs(lines, |l| self.symbols[i].owns(l)),
            ListRow::Loose(_) => {
                let here: Vec<&Symbol> = self.symbols.iter().filter(|s| s.file == f).collect();
                runs(lines, |l| !here.iter().any(|s| s.owns(l)))
            }
            ListRow::File(_) => self.files[f].hunks.clone(),
        }
    }

    /// What v marks: the row's key and a hash of its changes, so a mark
    /// lasts until the change under it does. The hash is FNV-1a, written
    /// out, because marks live on disk and std's hasher may change between
    /// builds.
    pub fn viewed_mark(&self, row: ListRow) -> String {
        const FNV_OFFSET: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x100000001b3;
        let f = self.file_of(row);
        let key = match row {
            ListRow::Symbol(i) => self.symbols[i].key.clone(),
            ListRow::Loose(_) => format!("{}#outside", self.files[f].path),
            ListRow::File(_) => format!("{}#file", self.files[f].path),
        };
        let mut hash = FNV_OFFSET;
        for hunk in self.hunks(row) {
            for l in &self.files[f].lines[hunk] {
                for b in [l.mark as u8].iter().chain(l.text.as_bytes()).chain(b"\n") {
                    hash = (hash ^ *b as u64).wrapping_mul(FNV_PRIME);
                }
            }
        }
        format!("{key}\t{hash:016x}")
    }
}

#[cfg(test)]
pub mod fixture {
    //! A made-up change to a made-up TypeScript app.
    use super::*;

    pub const OLD: &str = "\
import { a } from \"./a\";

export function total(items: number[]) {
  let sum = 0;
  for (const i of items) sum += i;
  return sum;
}

function unused() {
  return 1;
}

export function report(items: number[]) {
  return \"total \" + total(items);
}
";

    pub const NEW: &str = "\
import { a, b } from \"./a\";

export function total(items: number[]) {
  return items.reduce((s, i) => s + i, 0);
}

export function report(items: number[]) {
  return `total ${total(items)} of ${count(items)}`;
}

export function count(items: number[]) {
  return items.length;
}
";

    pub const DIFF: &str = "\
diff --git a/src/sum.ts b/src/sum.ts
--- a/src/sum.ts
+++ b/src/sum.ts
@@ -1 +1 @@
-import { a } from \"./a\";
+import { a, b } from \"./a\";
@@ -4,3 +4 @@ export function total(items: number[]) {
-  let sum = 0;
-  for (const i of items) sum += i;
-  return sum;
+  return items.reduce((s, i) => s + i, 0);
@@ -9,2 +7,2 @@ export function total(items: number[]) {
-function unused() {
-  return 1;
+export function report(items: number[]) {
+  return `total ${total(items)} of ${count(items)}`;
@@ -13,2 +11,2 @@ function unused() {
-export function report(items: number[]) {
-  return \"total \" + total(items);
+export function count(items: number[]) {
+  return items.length;
diff --git a/src/sum.test.ts b/src/sum.test.ts
new file mode 100644
--- /dev/null
+++ b/src/sum.test.ts
@@ -0,0 +1,2 @@
+import { total } from \"./sum\";
+test(\"adds\", () => expect(total([1, 2])).toBe(3));
";

    pub fn sides() -> Sides {
        let mut s = Sides {
            status: "M\tsrc/sum.ts\nA\tsrc/sum.test.ts\n".into(),
            diff: DIFF.into(),
            ..Default::default()
        };
        s.old.insert("src/sum.ts".into(), OLD.into());
        s.new.insert("src/sum.ts".into(), NEW.into());
        s.new.insert("src/sum.test.ts".into(), "import { total } from \"./sum\";\ntest(\"adds\", () => expect(total([1, 2])).toBe(3));\n".into());
        s
    }

    pub fn symbol(r: &Review, name: &str) -> usize {
        r.symbols.iter().position(|s| s.name == name).unwrap()
    }

    /// The loose row of a file.
    pub fn loose(r: &Review, path: &str) -> usize {
        r.loose
            .iter()
            .position(|l| r.files[l.file].path == path)
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;

    fn sum(r: &Review) -> usize {
        r.files.iter().position(|f| f.path == "src/sum.ts").unwrap()
    }

    fn texts(r: &Review, of: ListRow) -> Vec<Vec<String>> {
        let lines = &r.files[r.file_of(of)].lines;
        r.hunks(of)
            .into_iter()
            .map(|h| lines[h].iter().map(|l| l.text.trim().to_string()).collect())
            .collect()
    }

    #[test]
    fn the_diff_is_drawn_on_the_new_file() {
        let r = review(&sides());
        let f = &r.files[sum(&r)];
        let drawn: Vec<String> = f.lines[..8]
            .iter()
            .map(|l| {
                format!(
                    "{}{}",
                    match l.mark {
                        Mark::Same => ' ',
                        Mark::Add => '+',
                        Mark::Del => '-',
                    },
                    l.text
                )
            })
            .collect();
        assert_eq!(
            drawn,
            [
                "-import { a } from \"./a\";",
                "+import { a, b } from \"./a\";",
                " ",
                " export function total(items: number[]) {",
                "-  let sum = 0;",
                "-  for (const i of items) sum += i;",
                "-  return sum;",
                "+  return items.reduce((s, i) => s + i, 0);",
            ]
        );
        assert_eq!(f.hunks.len(), 5);
        // Same count out as in: the words that changed are marked.
        assert_eq!(f.lines[1].words, Some(10..13));
        assert_eq!(&f.lines[1].text[10..13], ", b");
        // Three out and one in: nothing to pair them by.
        assert_eq!(f.lines[7].words, None);
    }

    #[test]
    fn symbols_are_a_tree_of_what_uses_what() {
        let r = review(&sides());
        let names = |is: &[usize]| {
            is.iter()
                .map(|&i| r.symbols[i].name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&r.roots), ["report", "unused"]);
        assert_eq!(
            names(&r.symbols[symbol(&r, "report")].uses),
            ["total", "count"]
        );
    }

    #[test]
    fn a_symbols_hunks_are_the_changes_it_owns() {
        let r = review(&sides());
        // git put report's new lines where unused was, and its old ones
        // further down: two hunks, neither with unused's lines in it, nor
        // the line that is the same at both places.
        assert_eq!(
            texts(&r, ListRow::Symbol(symbol(&r, "report"))),
            [vec!["return `total ${total(items)} of ${count(items)}`;"], vec!["return \"total \" + total(items);"]]
        );
    }

    #[test]
    fn a_moved_symbol_does_not_own_what_lies_between_its_places() {
        let diff = "\
diff --git a/src/m.ts b/src/m.ts
--- a/src/m.ts
+++ b/src/m.ts
@@ -0,0 +1,4 @@
+export function c() {
+  return 3;
+}
+
@@ -6,5 +10 @@ export function b() {
-  return 2;
-}
-
-export function c() {
-  return 3;
+  return 20;
";
        let mut s = Sides {
            status: "M\tsrc/m.ts\n".into(),
            diff: diff.into(),
            ..Default::default()
        };
        s.old.insert("src/m.ts".into(), "export function a() {\n  return 1;\n}\n\nexport function b() {\n  return 2;\n}\n\nexport function c() {\n  return 3;\n}\n".into());
        s.new.insert("src/m.ts".into(), "export function c() {\n  return 3;\n}\n\nexport function a() {\n  return 1;\n}\n\nexport function b() {\n  return 20;\n}\n".into());
        let r = review(&s);
        // c only moved: what is left of it is the brace git matched with
        // b's instead of its own.
        let c = texts(&r, ListRow::Symbol(symbol(&r, "c")));
        assert_eq!(c, [vec!["}"]]);
        let b = texts(&r, ListRow::Symbol(symbol(&r, "b")));
        assert_eq!(b, [vec!["return 2;", "}"], vec!["return 20;"]]);
    }

    /// total goes from a.ts to a new b.ts, its last line as `returns`.
    fn moved_to_another_file(returns: &str) -> Review {
        let body = |returns: &str| format!("export function total(items: number[]) {{\n  let sum = 0;\n  for (const i of items) sum += i;\n  {returns}\n}}\n");
        let keep = "export function keep() {\n  return 1;\n}\n";
        let minus: String = body("return sum;").lines().map(|l| format!("-{l}\n")).collect();
        let plus: String = body(returns).lines().map(|l| format!("+{l}\n")).collect();
        let diff = format!("diff --git a/src/a.ts b/src/a.ts\n--- a/src/a.ts\n+++ b/src/a.ts\n@@ -4,5 +3,0 @@ export function keep() {{\n{minus}diff --git a/src/b.ts b/src/b.ts\nnew file mode 100644\n--- /dev/null\n+++ b/src/b.ts\n@@ -0,0 +1,5 @@\n{plus}");
        let mut s = Sides {
            status: "M\tsrc/a.ts\nA\tsrc/b.ts\n".into(),
            diff,
            ..Default::default()
        };
        s.old.insert("src/a.ts".into(), format!("{keep}{}", body("return sum;")));
        s.new.insert("src/a.ts".into(), keep.into());
        s.new.insert("src/b.ts".into(), body(returns));
        review(&s)
    }

    #[test]
    fn a_symbol_moved_to_another_file_unchanged_is_one_row_with_nothing_to_read() {
        let r = moved_to_another_file("return sum;");
        let listed: Vec<usize> = r.roots.iter().copied().filter(|&i| r.symbols[i].name == "total").collect();
        let [total] = listed[..] else { panic!("{listed:?}") };
        let s = &r.symbols[total];
        assert_eq!((r.files[s.file].path.as_str(), s.adds, s.dels), ("src/b.ts", 0, 0));
        assert_eq!(s.moved.map(|f| r.files[f].path.as_str()), Some("src/a.ts"));
        assert!(r.hunks(ListRow::Symbol(total)).is_empty());
    }

    #[test]
    fn a_symbol_changed_on_its_way_shows_only_what_changed_at_each_place() {
        let r = moved_to_another_file("return sum + 1;");
        let listed = r.roots.iter().filter(|&&i| r.symbols[i].name == "total");
        let rows: Vec<(&str, Vec<Vec<String>>)> = listed.map(|&i| (r.files[r.symbols[i].file].path.as_str(), texts(&r, ListRow::Symbol(i)))).collect();
        assert_eq!(
            rows,
            [
                ("src/a.ts", vec![vec!["return sum;".to_string()]]),
                ("src/b.ts", vec![vec!["return sum + 1;".to_string()]])
            ]
        );
    }

    #[test]
    fn a_symbol_begins_with_its_doc_comment_but_is_at_its_keyword() {
        let new = "/**\n * Adds them up.\n */\nexport function total(items: number[]) {\n  return 1;\n}\n";
        let mut s = Sides {
            status: "A\tsrc/t.ts\n".into(),
            diff: format!("diff --git a/src/t.ts b/src/t.ts\n--- /dev/null\n+++ b/src/t.ts\n@@ -0,0 +1,6 @@\n{}", new.lines().map(|l| format!("+{l}\n")).collect::<String>()),
            ..Default::default()
        };
        s.new.insert("src/t.ts".into(), new.into());
        let r = review(&s);
        let t = &r.symbols[symbol(&r, "total")];
        assert_eq!((t.new.clone(), t.at), (1..7, 4));
    }

    #[test]
    fn the_import_is_the_loose_change() {
        let r = review(&sides());
        assert_eq!(
            texts(&r, ListRow::Loose(loose(&r, "src/sum.ts"))),
            [vec![
                "import { a } from \"./a\";",
                "import { a, b } from \"./a\";"
            ]]
        );
    }

    #[test]
    fn a_viewed_mark_follows_the_change() {
        let r = review(&sides());
        let report = ListRow::Symbol(symbol(&r, "report"));
        let before = r.viewed_mark(report);
        let mut s = sides();
        s.new.insert(
            "src/sum.ts".into(),
            NEW.replace("of ${count", "from ${count"),
        );
        s.diff = DIFF.replace("of ${count", "from ${count");
        let after = review(&s).viewed_mark(report);
        assert_eq!(before.split('\t').next(), after.split('\t').next());
        assert_ne!(before, after);
    }

    #[test]
    fn a_side_that_is_not_text_leaves_the_file_undrawn() {
        let mut s = sides();
        s.old.remove("src/sum.ts");
        let r = review(&s);
        let f = &r.files[sum(&r)];
        assert!(!f.readable);
        assert!(f.lines.is_empty() && f.hunks.is_empty());
    }
}
