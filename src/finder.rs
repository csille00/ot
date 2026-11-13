//! Finding any T3 thread or open MR of yours, as T3's ctrl+k does: titles matched
//! fuzzily as you type, and the words of threads' messages. Read straight from T3's database, read-only.
use crate::model::Mr;
use rusqlite::{Connection, OpenFlags};
use std::collections::{HashMap, HashSet};

/// The most message hits one query reads, newest first.
const HITS: usize = 300;

/// Messages the preview shows, newest last.
const PREVIEW: usize = 12;

#[derive(Clone, Debug, PartialEq)]
pub struct Thread {
    pub id: String,
    pub title: String,
    pub project: String,
    pub branch: String,
    pub updated: i64,
    pub archived: bool,
    pub settled: bool,
}

/// What a row is: one of your open MRs, or a thread, by its place in the finder's lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Of {
    Mr(usize),
    Thread(usize),
}

/// A row of the list: what it is, the label's characters that matched, and the
/// message text that matched when the title didn't.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub of: Of,
    pub marks: Vec<usize>,
    pub hit: Option<Hit>,
}

/// A message that matched: who said it, all of it, and the line of it the list shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub you: bool,
    pub text: String,
    pub snippet: String,
}

pub struct Said {
    pub you: bool,
    pub text: String,
}

/// What the cursor is on.
pub enum Picked<'a> {
    Mr(&'a Mr),
    Thread(&'a Thread),
}

pub struct Finder {
    db: Option<Connection>,
    pub threads: Vec<Thread>,
    pub mrs: Vec<Mr>,
    pub query: String,
    pub rows: Vec<Row>,
    pub at: usize,
    previews: HashMap<String, Vec<Said>>,
    pub error: Option<String>,
}

/// How an MR reads in the list, and what its title is matched as: "!123 Fix the thing".
pub fn mr_label(m: &Mr) -> String {
    format!("!{} {}", m.iid, m.title)
}

impl Finder {
    /// Over `mrs`, your open MRs, and every T3 thread.
    pub fn open(mut mrs: Vec<Mr>) -> Finder {
        let (db, threads, error) = match connect().and_then(|db| threads(&db).map(|t| (db, t))) {
            Ok((db, t)) => (Some(db), t, None),
            Err(e) => (None, vec![], Some(e)),
        };
        mrs.sort_by_key(|m| std::cmp::Reverse(m.updated));
        let mut f = Finder { db, threads, mrs, query: String::new(), rows: vec![], at: 0, previews: HashMap::new(), error };
        f.search();
        f
    }

    /// The list again for the query as it is now, the cursor back on the best.
    pub fn search(&mut self) {
        let terms: Vec<&str> = self.query.split_whitespace().collect();
        self.at = 0;
        if terms.is_empty() {
            let mrs = (0..self.mrs.len()).map(Of::Mr);
            let threads = (0..self.threads.len()).map(Of::Thread);
            self.rows = mrs.chain(threads).map(|of| Row { of, marks: vec![], hit: None }).collect();
            return;
        }
        let mrs = self.mrs.iter().enumerate().filter_map(|(i, m)| {
            let (score, marks) = fuzzy_all(&terms, &mr_label(m)).or_else(|| fuzzy_all(&terms, &m.source).map(|(s, _)| (s - 20, vec![])))?;
            Some((score, Row { of: Of::Mr(i), marks, hit: None }))
        });
        let threads = self.threads.iter().enumerate().filter_map(|(i, t)| {
            let (score, marks) = fuzzy_all(&terms, &t.title)?;
            Some((score, Row { of: Of::Thread(i), marks, hit: None }))
        });
        let mut scored: Vec<(i32, Row)> = mrs.chain(threads).collect();
        // Ties go to MRs, then the newer, which each list is already in.
        scored.sort_by_key(|(s, r)| (-s, r.of));
        let mut rows: Vec<Row> = scored.into_iter().map(|(_, r)| r).collect();
        if let Some(db) = &self.db {
            let by_id: HashMap<&str, usize> = self.threads.iter().enumerate().map(|(i, t)| (t.id.as_str(), i)).collect();
            let mut seen: HashSet<Of> = rows.iter().map(|r| r.of).collect();
            for (id, you, text) in message_hits(db, &terms).unwrap_or_default() {
                let Some(&i) = by_id.get(id.as_str()) else { continue };
                if seen.insert(Of::Thread(i)) {
                    let snippet = snippet(&text, &terms);
                    rows.push(Row { of: Of::Thread(i), marks: vec![], hit: Some(Hit { you, text, snippet }) });
                }
            }
        }
        self.rows = rows;
    }

    pub fn typed(&mut self, key: &str) {
        if crate::keys::edit(&mut self.query, key) {
            self.search();
        }
    }

    pub fn step(&mut self, by: isize) {
        if self.rows.is_empty() {
            return;
        }
        let n = self.rows.len() as isize;
        self.at = (self.at as isize + by).rem_euclid(n) as usize;
    }

    pub fn item(&self, of: Of) -> Picked<'_> {
        match of {
            Of::Mr(i) => Picked::Mr(&self.mrs[i]),
            Of::Thread(i) => Picked::Thread(&self.threads[i]),
        }
    }

    pub fn picked(&self) -> Option<Picked<'_>> {
        self.rows.get(self.at).map(|r| self.item(r.of))
    }

    /// The newest of what was said in the thread under the cursor, read once.
    pub fn preview(&mut self) -> &[Said] {
        let Some(Picked::Thread(t)) = self.picked() else { return &[] };
        let id = t.id.clone();
        if !self.previews.contains_key(&id) {
            let said = self.db.as_ref().and_then(|db| said(db, &id).ok()).unwrap_or_default();
            self.previews.insert(id.clone(), said);
        }
        &self.previews[&id]
    }
}

fn connect() -> Result<Connection, String> {
    let path = crate::t3::home().ok_or("no T3 home")?.join("userdata/statev2.sqlite");
    if !path.exists() {
        return Err(format!("no T3 database at {}", path.display()));
    }
    let db = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| format!("T3's database: {e}"))?;
    let _ = db.busy_timeout(std::time::Duration::from_millis(500));
    Ok(db)
}

/// Every thread T3 still has, newest first.
fn threads(db: &Connection) -> Result<Vec<Thread>, String> {
    let sql = "SELECT t.thread_id, t.title, COALESCE(p.title, ''), t.updated_at, t.archived_at IS NOT NULL,
                      COALESCE(json_extract(t.payload_json, '$.branch'), ''),
                      json_extract(t.payload_json, '$.settledAt') IS NOT NULL
                 FROM orchestration_v2_projection_threads t
                 LEFT JOIN projection_projects p ON p.project_id = t.project_id
                WHERE t.deleted_at IS NULL
                ORDER BY t.updated_at DESC";
    let mut stmt = db.prepare(sql).map_err(|e| format!("T3's database: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            let updated: String = r.get(3)?;
            Ok(Thread {
                id: r.get(0)?,
                title: r.get(1)?,
                project: r.get(2)?,
                updated: updated.parse::<jiff::Timestamp>().map_or(0, |t| t.as_second()),
                archived: r.get(4)?,
                branch: r.get(5)?,
                settled: r.get(6)?,
            })
        })
        .map_err(|e| format!("T3's database: {e}"))?;
    rows.collect::<Result<_, _>>().map_err(|e| format!("T3's database: {e}"))
}

/// Messages holding every term, newest first, as (thread, said by you, text).
fn message_hits(db: &Connection, terms: &[&str]) -> rusqlite::Result<Vec<(String, bool, String)>> {
    let terms: Vec<String> = terms.iter().filter(|t| t.chars().count() >= 2).map(|t| format!("%{}%", like_escape(t))).collect();
    if terms.is_empty() {
        return Ok(vec![]);
    }
    let text = "json_extract(payload_json, '$.text')";
    let wheres: Vec<String> = (1..=terms.len()).map(|i| format!("{text} LIKE ?{i} ESCAPE '\\'")).collect();
    let sql = format!(
        "SELECT thread_id, role = 'user', {text} FROM orchestration_v2_projection_messages
          WHERE role IN ('user', 'assistant') AND {} ORDER BY created_at DESC LIMIT {HITS}",
        wheres.join(" AND ")
    );
    let mut stmt = db.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(terms.iter()), |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, Option<String>>(2)?.unwrap_or_default())))?;
    rows.collect()
}

fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// The thread's newest messages, oldest of them first.
fn said(db: &Connection, id: &str) -> rusqlite::Result<Vec<Said>> {
    let mut stmt = db.prepare(
        "SELECT role, json_extract(payload_json, '$.text') FROM orchestration_v2_projection_messages
          WHERE thread_id = ?1 AND role IN ('user', 'assistant') ORDER BY created_at DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![id, PREVIEW as i64], |r| {
        Ok(Said { you: r.get::<_, String>(0)? == "user", text: r.get::<_, Option<String>>(1)?.unwrap_or_default().trim().to_string() })
    })?;
    let mut said: Vec<Said> = rows.collect::<rusqlite::Result<_>>()?;
    said.retain(|s| !s.text.is_empty());
    said.reverse();
    Ok(said)
}

/// One line of `text` from a little before the first term it holds.
pub fn snippet(text: &str, terms: &[&str]) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = flat.to_lowercase();
    let at = terms.iter().filter_map(|t| lower.find(&t.to_lowercase())).min().unwrap_or(0);
    let at = if flat.is_char_boundary(at) { at } else { 0 };
    let mut start = at.saturating_sub(24);
    while !flat.is_char_boundary(start) {
        start -= 1;
    }
    if start > 0 {
        if let Some(sp) = flat[start..at].find(' ') {
            start += sp + 1;
        }
    }
    let cut: String = flat[start..].chars().take(200).collect();
    if start > 0 {
        format!("…{cut}")
    } else {
        cut
    }
}

/// Every term matched in `text`: their scores summed and the characters they hit.
fn fuzzy_all(terms: &[&str], text: &str) -> Option<(i32, Vec<usize>)> {
    let mut total = 0;
    let mut marks = vec![];
    for t in terms {
        let (s, m) = fuzzy(t, text)?;
        total += s;
        marks.extend(m);
    }
    marks.sort_unstable();
    marks.dedup();
    Some((total, marks))
}

/// `pattern` as a subsequence of `text`, fzf's way, scored for word starts and runs;
/// the char indexes it hit. An upper-case letter in it makes it case-sensitive.
pub fn fuzzy(pattern: &str, text: &str) -> Option<(i32, Vec<usize>)> {
    let exact = pattern.chars().any(char::is_uppercase);
    let fold = |c: char| if exact { c } else { c.to_lowercase().next().unwrap_or(c) };
    let p: Vec<char> = pattern.chars().map(fold).collect();
    let t: Vec<char> = text.chars().collect();
    let tf: Vec<char> = t.iter().map(|c| fold(*c)).collect();
    if p.is_empty() {
        return Some((0, vec![]));
    }
    // The first match's end, then back from it to the latest start: the tightest.
    let mut pi = 0;
    let mut end = None;
    for (i, c) in tf.iter().enumerate() {
        if *c == p[pi] {
            pi += 1;
            if pi == p.len() {
                end = Some(i);
                break;
            }
        }
    }
    let end = end?;
    let mut pi = p.len();
    let mut start = end;
    for i in (0..=end).rev() {
        if tf[i] == p[pi - 1] {
            pi -= 1;
            if pi == 0 {
                start = i;
                break;
            }
        }
    }
    let mut marks = Vec::with_capacity(p.len());
    let mut pi = 0;
    for (i, c) in tf.iter().enumerate().take(end + 1).skip(start) {
        if pi < p.len() && *c == p[pi] {
            marks.push(i);
            pi += 1;
        }
    }
    // Letters strewn across a long title aren't a match anyone means.
    let gaps: usize = marks.windows(2).map(|w| w[1] - w[0] - 1).sum();
    if gaps > p.len().max(3) {
        return None;
    }
    let boundary = |i: usize| i == 0 || !t[i - 1].is_alphanumeric() || (t[i - 1].is_lowercase() && t[i].is_uppercase());
    let mut score = 0;
    let mut last: Option<usize> = None;
    for (k, &i) in marks.iter().enumerate() {
        score += 16;
        if boundary(i) {
            score += if k == 0 { 16 } else { 8 };
        }
        match last {
            Some(l) if l + 1 == i => score += 6,
            Some(l) => score -= 3 + (i - l - 1).min(10) as i32,
            None => score -= (i as i32).min(15),
        }
        last = Some(i);
    }
    Some((score, marks))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_matches_a_subsequence_and_marks_it() {
        assert_eq!(fuzzy("mnm", "fix neominimap border").map(|(_, m)| m), Some(vec![7, 9, 11]));
        assert!(fuzzy("xyz", "fix neominimap border").is_none());
        assert!(fuzzy("Fix", "fix it").is_none(), "an upper-case letter makes it case-sensitive");
        assert!(fuzzy("fix", "Fix it").is_some());
    }

    #[test]
    fn word_starts_and_runs_beat_scattered_letters() {
        let (start, _) = fuzzy("rev", "review the MR").unwrap();
        let (scattered, _) = fuzzy("rev", "brake via").unwrap();
        assert!(start > scattered);
        let (tight, _) = fuzzy("settle", "settle threads").unwrap();
        let (loose, _) = fuzzy("settle", "set tile").unwrap();
        assert!(tight > loose);
    }

    #[test]
    fn letters_strewn_across_a_title_are_no_match() {
        assert!(fuzzy("settle", "$full-send https://linear.app/redo/issue/HELP-1126").is_none());
        assert!(fuzzy("setl", "settle threads").is_some());
        assert!(fuzzy("hlpdsk", "helpdesk api review").is_some());
    }

    #[test]
    fn your_mrs_match_by_number_title_or_branch_and_come_first_when_nothing_is_typed() {
        let mut a = crate::model::fixture::mr(78877, "me", "connor/billing-default", "main");
        a.title = "Default billing to simple".into();
        let mut f = Finder { db: None, threads: vec![], mrs: vec![a], query: String::new(), rows: vec![], at: 0, previews: HashMap::new(), error: None };
        f.threads.push(Thread { id: "t".into(), title: "billing".into(), project: String::new(), branch: String::new(), updated: 0, archived: false, settled: false });
        f.search();
        assert_eq!(f.rows.iter().map(|r| r.of).collect::<Vec<_>>(), [Of::Mr(0), Of::Thread(0)]);
        for q in ["!78877", "78877", "simple", "billing-def"] {
            f.query = q.into();
            f.search();
            assert!(f.rows.iter().any(|r| r.of == Of::Mr(0)), "{q}");
        }
        f.query = "!78877".into();
        f.search();
        assert_eq!(f.rows[0].marks, [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn every_term_must_match() {
        assert!(fuzzy_all(&["mini", "border"], "fix neominimap border").is_some());
        assert!(fuzzy_all(&["mini", "zzz"], "fix neominimap border").is_none());
    }

    #[test]
    fn a_snippet_starts_a_few_words_before_its_match() {
        let text = "one two three four five six seven eight nine ten eleven twelve thirteen linker fails here";
        let s = snippet(text, &["linker"]);
        assert!(s.starts_with('…') && s.contains("linker fails"), "{s}");
        assert!(s.find("linker").unwrap() < 30, "{s}");
        assert_eq!(snippet("linker\n\nfails", &["linker"]), "linker fails");
    }
}
