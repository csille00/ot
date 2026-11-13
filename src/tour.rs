//! A tour of an MR's change, which Claude writes from the diff in one read:
//! what the change does, the situations that now end differently, and a
//! walk through the code in the order it runs. Each step of the walk is a
//! line the change touched and what the code does there, and the review
//! shows it as a page read from top to bottom, each step over its own code
//! (ot.lua). It is a way in, not coverage: the list still counts what you
//! have viewed.
use crate::ask;
use crate::model::Mr;
use crate::review::{File, Mark, Review};
use serde_json::Value;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Debug, Default, PartialEq)]
pub struct Tour {
    pub summary: String,
    pub cases: Vec<Case>,
    pub steps: Vec<Step>,
    /// Where the change adds complexity it didn't need; most have none.
    pub flags: Vec<Flag>,
}

/// A design problem the change adds, where it shows.
#[derive(Debug, PartialEq)]
pub struct Flag {
    /// Where, the lines that show it, its name and what it costs.
    pub at: Step,
    /// The smallest simpler design.
    pub instead: String,
}

/// A situation the change ends differently.
#[derive(Debug, PartialEq)]
pub struct Case {
    pub when: String,
    pub before: String,
    pub now: String,
}

#[derive(Debug, PartialEq)]
pub struct Step {
    pub file: usize,
    /// On the new side.
    pub line: u32,
    /// The lines of the new file Claude would have you read with it, first
    /// and last; 0 in a tour told before steps had them.
    pub from: u32,
    pub to: u32,
    /// A few words for what happens there; none in a tour told before
    /// steps had them.
    pub title: String,
    pub says: String,
    /// What Claude would have you check there, as a question; often none.
    pub ask: String,
}

const SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"required":["summary","cases","steps","flags"],"properties":{"summary":{"type":"string"},"cases":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["when","before","now"],"properties":{"when":{"type":"string"},"before":{"type":"string"},"now":{"type":"string"}}}},"steps":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["path","line","from","to","title","says","ask"],"properties":{"path":{"type":"string"},"line":{"type":"integer"},"from":{"type":"integer"},"to":{"type":"integer"},"title":{"type":"string"},"says":{"type":"string"},"ask":{"type":"string"}}}},"flags":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["path","line","from","to","title","says","instead"],"properties":{"path":{"type":"string"},"line":{"type":"integer"},"from":{"type":"integer"},"to":{"type":"integer"},"title":{"type":"string"},"says":{"type":"string"},"instead":{"type":"string"}}}}}}"#;

/// The unchanged lines Claude sees either side of a hunk.
const CONTEXT: usize = 10;

/// More than Claude reads in one turn.
const MOST: usize = 400_000;

/// One asking of Claude, for whoever else wants the same tour meanwhile.
#[derive(Default)]
struct Asking {
    heard: Mutex<Heard>,
    more: Condvar,
}

#[derive(Default)]
struct Heard {
    /// Its answer's JSON as far as it is written.
    written: String,
    /// How it ended.
    done: Option<Result<(), String>>,
}

/// The askings under way, by where their answer is kept.
static ASKING: Mutex<Option<HashMap<PathBuf, Arc<Asking>>>> = Mutex::new(None);

/// The tour of `mr`'s change `r`, as Claude tells it from the title, the
/// `description` and the diff alone. While it writes, `told` hears the tour
/// as far as it has got. Its answer is kept in `run`, so a change is told
/// once: asked for again while Claude is still at it, this waits on that
/// same asking and hears it too.
pub fn read(mr: &Mr, description: &str, r: &Review, run: &Path, told: &mut dyn FnMut(&Tour)) -> Result<Tour, String> {
    let asked = |told: &mut dyn FnMut(&Tour)| -> Result<(), String> {
        let (asking, mine) = {
            let mut all = ASKING.lock().unwrap_or_else(|e| e.into_inner());
            let all = all.get_or_insert_with(HashMap::new);
            match all.get(run) {
                Some(a) => (a.clone(), false),
                None => (all.entry(run.to_path_buf()).or_default().clone(), true),
            }
        };
        if !mine {
            let mut heard = asking.heard.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                told(&tour(r, &whole(&heard.written)));
                if let Some(done) = &heard.done {
                    return done.clone();
                }
                heard = asking.more.wait(heard).unwrap_or_else(|e| e.into_inner());
            }
        }
        let listing = listing(r);
        let done = if listing.is_empty() {
            Err("nothing to tour: only tests, generated files or removed ones changed".to_string())
        } else if listing.len() > MOST {
            Err("too big a change for Claude to tour in one read".to_string())
        } else {
            std::fs::create_dir_all(run).map_err(|e| e.to_string()).and_then(|()| {
                let mut steps = 0;
                ask::hearing("opus", run, run, "", SCHEMA, &prompt(&mr.title, description, &listing), &mut |event| {
                    let Some(more) = event["event"]["delta"]["partial_json"].as_str() else {
                        return;
                    };
                    let mut heard = asking.heard.lock().unwrap_or_else(|e| e.into_inner());
                    heard.written.push_str(more);
                    let so_far = tour(r, &whole(&heard.written));
                    if so_far.steps.len() > steps {
                        steps = so_far.steps.len();
                        told(&so_far);
                        asking.more.notify_all();
                    }
                })
                .map(|_| ())
            })
        };
        ASKING.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|all| all.remove(run));
        asking.heard.lock().unwrap_or_else(|e| e.into_inner()).done = Some(done.clone());
        asking.more.notify_all();
        done
    };
    let said = match kept(run) {
        Some(said) => said,
        None => {
            asked(told)?;
            kept(run).ok_or("Claude stopped without an answer")?
        }
    };
    let tour = tour(r, &said);
    if tour.steps.is_empty() {
        return Err("Claude's tour named no line of the change".into());
    }
    Ok(tour)
}

/// Where the tour of `mr`'s change as it is now is kept.
pub fn run_dir(project: &str, mr: &Mr) -> PathBuf {
    crate::cache::tour_dir(project).join(format!("{}-{}", mr.base, mr.head))
}

/// Whether the tour kept in `run` is told and whole, so it opens at once.
pub fn told(run: &Path) -> bool {
    kept(run).is_some()
}

/// What Claude said the last time it was asked in `run`, unless it failed.
fn kept(run: &Path) -> Option<Value> {
    let all: Value = serde_json::from_slice(&std::fs::read(run.join("claude.json")).ok()?).ok()?;
    (all["is_error"] != true && all["structured_output"]["steps"].is_array()).then(|| all["structured_output"].clone())
}

/// An answer still being `written`, as far as its last finished case or
/// step; null before the first.
fn whole(written: &str) -> Value {
    let end = written.rfind('}').map_or(0, |i| i + 1);
    serde_json::from_str(&format!("{}]}}", &written[..end])).unwrap_or(Value::Null)
}

/// The change as Claude reads it: each source file's hunks with the lines
/// round them, every line still there under its number in the new file.
/// A line that only moved reads as unchanged, as the review draws it.
fn listing(r: &Review) -> String {
    let mut out = String::new();
    for f in r.files.iter().filter(|f| !f.aside() && f.readable && !f.gone() && !f.hunks.is_empty()) {
        out.push_str(&format!("\n== {}{}\n", f.path, if f.status == 'A' { " (a new file)" } else { "" }));
        let mut end = 0;
        for h in &f.hunks {
            let from = h.start.saturating_sub(CONTEXT).max(end);
            if from > end {
                out.push_str("       …\n");
            }
            end = (h.end + CONTEXT).min(f.lines.len()).max(end);
            for l in &f.lines[from..end] {
                match (l.mark, l.moved) {
                    (Mark::Del, true) => {}
                    (Mark::Del, false) => out.push_str(&format!("       - {}\n", l.text)),
                    (Mark::Add, false) => out.push_str(&format!("{:>6} + {}\n", l.new, l.text)),
                    _ => out.push_str(&format!("{:>6}   {}\n", l.new, l.text)),
                }
            }
        }
    }
    out
}

fn prompt(title: &str, description: &str, listing: &str) -> String {
    format!(
        "Write a reviewer's tour of a merge request.\n\n\
The reviewer reads your tour from top to bottom before they review, in their editor, and when they finish it they should understand the change: what the code now does, for whom it behaves differently, and where to look hard. Under each step of your walk they see the code around the line you name, so your words and the code are read together.\n\n\
The tour has four parts.\n\n\
`summary` is two or three sentences on what the change does and why, for someone who knows the product but hasn't seen this code.\n\n\
`cases` are the situations that now end differently. Each has `when`, the situation in plain words (\"A user has never set a time zone\"), `before`, what happened then, and `now`, what happens. One short sentence each. List only real differences in what the code does or what someone sees, the ones a reviewer would want to know before reading any code, and never more than four: fold small ones of a kind into one. A change that only moves or renames things has none, and the list is empty.\n\n\
`steps` are the walk through the code, in the order it runs: where the changed code is entered first, a caller before what it calls, and something a step relies on (a type, a column, a setting) just before the step that uses it. A step says what the code does at its line when it runs: what comes in, what it decides, what it hands on. Say what the code does, not what was edited: \"Each row is read here from the cache, and only the fresh ones are kept\" and not \"the timestamp field is removed\". Where a step is the place one of your cases is decided, say so. Say why when the description or the code tells you. If something there looks wrong or left undecided, put what to check in the step's `ask`, as one question the reviewer can answer by looking; it is shown apart from the rest, so keep it out of `says`. Most steps have none, and their `ask` is empty.\n\n\
A step's `title` is what happens there in a few words, the way a chapter is named: \"The saved settings are read\", \"Only admins see the export button\". No file or function names in it.\n\n\
Write the way a teammate would talk someone through it at their desk: plain sentences, a plain word where a variable's name isn't needed, no praise and no judging. Two or three sentences a step. Put a name from the code in `backticks`.\n\n\
A step is for something the reviewer has to understand or decide. Imports, renames, formatting and lines that only moved get none, and several edits that do one thing are one step, on the line that shows it best. Fewer is better: two to five steps for a change under a hundred lines, never more than twelve.\n\n\
`path` is a file's path exactly as its == header has it. `line` is the number in front of a line marked +; where lines were only removed (marked -, no number), it is the first numbered line after them. A step anywhere else is thrown away. `from` and `to` are the first and last numbered lines the reviewer is shown under the step, `line` among them: everything your words point at has to be in them, so if you name a variable, show the line that sets it. Take what the step needs and no more, sixty lines at the very most. Name nothing in a step that those lines don't show.\n\n\
`flags`, last, are the places where this change adds complexity it didn't have to. Reducing complexity is what this reviewer cares about most, and they mean it as John Ousterhout defines it in A Philosophy of Software Design: complexity is anything in the structure of a system that makes it hard to understand and to modify. What counts is what the next reader meets, not what the author found easy to write, and code that is read and changed often counts for more than code that is rarely touched. It shows as three symptoms. Change amplification: a change that looks simple has to be made in many places. Cognitive load: how much someone must know to make a change safely, which fewer lines can make worse. Unknown unknowns, the worst of the three: nothing tells you which code a change must touch or what you have to know to make it, so you find out when it breaks. It has two causes. Dependencies: code that can't be understood or changed without also knowing other code. Obscurity: something important that isn't obvious, like a vague name, an undocumented rule, or two things that are inconsistent. And it comes in small pieces, each one easy to wave through, so judge this change's own piece.\n\n\
Flags are the part the reviewer acts on, so the bar is high. None is the normal answer and most changes get none; never more than three, worst first. Keep one only if you can point at it in the lines shown, it is this change's doing (an older problem counts only if the change now leans on it), and it will cost the next person one of those three symptoms, which you can name in this code. Drop style, \"could be nicer\", anything that depends on code you can't see, and anything that might matter only if the code changes some day.\n\n\
The red flags Ousterhout lists are where to look: a shallow module, whose interface is no simpler than what it hides; information leakage, one decision showing up in two places that must now change together; a pass-through parameter, threaded through code that doesn't use it; special-case code mixed into general code; the same nontrivial code twice; two functions you can't understand apart; a name that is vague or was plainly hard to pick; code whose behavior you can't see quickly. Look for the shortcuts: a symptom patched where it showed up (an `if` for one case, a new flag or boolean parameter) instead of where it comes from; an option that pushes a decision onto callers; a one-use helper or a wrapper that hides nothing; a check for a case that can't happen; something the description never asked for. Look for what Rich Hickey calls complecting: state, time and control flow braided together, or a flag that changes what one function does. And look for one thing given two names, or a new name for something the code around it already calls something else.\n\n\
A flag has `path`, `line`, `from` and `to` as a step does, showing the lines that prove it. Its `title` names the problem in plain words (\"A parameter passed through three layers\", \"One thing, two names\"). Its `says` is what is wrong and which symptom it will cost the next person, shown in this code: the places that now change together, the thing they must know, or the thing nothing tells them. One or two sentences in plain words, not a principle and not the symptom's name alone. Its `instead` is the smallest simpler design, in one sentence, and it must take out more than it puts in: a fix that adds a layer or an option is not one.\n\n\
The merge request: {title}\n\n\
Its author's description:\n{description}\n\n\
The change. Tests, generated files and removed files are left out. Each line still in the file has its number in the new file; + is a line the change added or rewrote, - one it took out, … lines not shown.\n{listing}"
    )
}

/// What Claude `said`, with only the steps and flags that are on a line
/// the change touched, in its order.
fn tour(r: &Review, said: &Value) -> Tour {
    let list = |of: &str| said[of].as_array().map(Vec::as_slice).unwrap_or_default();
    let text = |v: &Value| v.as_str().unwrap_or_default().trim().to_string();
    let number = |v: &Value| v.as_u64().and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
    let place = |s: &Value| {
        let file = r.files.iter().position(|f| Some(f.path.as_str()) == s["path"].as_str())?;
        let line = u32::try_from(s["line"].as_u64()?).ok()?;
        let says = text(&s["says"]);
        (!says.is_empty() && at(&r.files[file], line).is_some()).then_some(Step { file, line, from: number(&s["from"]), to: number(&s["to"]), title: text(&s["title"]), says, ask: text(&s["ask"]) })
    };
    Tour {
        summary: text(&said["summary"]),
        cases: list("cases").iter().map(|c| Case { when: text(&c["when"]), before: text(&c["before"]), now: text(&c["now"]) }).collect(),
        steps: list("steps").iter().filter_map(place).collect(),
        flags: list("flags").iter().filter_map(|f| Some(Flag { at: place(f)?, instead: text(&f["instead"]) })).collect(),
    }
}

/// Where `line` of the new file is in `f.lines` and the hunk it is in, or
/// that removed lines hang on it from; none for a line the change left.
fn at(f: &File, line: u32) -> Option<(usize, &Range<usize>)> {
    let at = f.lines.iter().position(|l| l.mark != Mark::Del && l.new == line)?;
    Some((at, f.hunks.iter().find(|h| h.start <= at && at <= h.end)?))
}

/// The most lines a step is read over.
const MOST_SHOWN: usize = 80;

/// How far from what a step shows a name it uses may be and still be
/// brought into view.
const NEAR: usize = 40;

/// The lines of `f` step `s` is read over: the ones Claude asked for, or
/// without them its hunk and two lines either side, kept close round its
/// line. Then widened to the nearest line with each name its words put in
/// backticks, when that is near, so what you read about is on the screen.
pub fn excerpt(f: &File, s: &Step) -> Range<usize> {
    let Some((at, hunk)) = at(f, s.line) else {
        return 0..0;
    };
    let place = |n: u32| f.lines.iter().position(|l| l.mark != Mark::Del && l.new == n);
    let mut shown = match (place(s.from), place(s.to)) {
        (Some(from), Some(to)) if from <= at && at <= to && to - from < MOST_SHOWN => {
            // With the lines taken out just above its first.
            let from = (0..from).rev().take_while(|i| f.lines[*i].mark == Mark::Del).last().unwrap_or(from);
            from..to + 1
        }
        _ => hunk.start.saturating_sub(2).max(at.saturating_sub(6))..(hunk.end + 2).min(at + 10).min(f.lines.len()),
    };
    let named = [&s.says, &s.ask].into_iter().flat_map(|t| t.split('`').skip(1).step_by(2)).filter(|n| n.len() > 2);
    for name in named {
        let has = |i: usize| f.lines[i].text.contains(name) && !(f.lines[i].mark == Mark::Del && f.lines[i].moved);
        if shown.clone().any(has) {
            continue;
        }
        let away = |i: usize| if i < shown.start { shown.start - i } else { i + 1 - shown.end };
        if let Some(i) = (0..f.lines.len()).filter(|i| has(*i)).min_by_key(|i| away(*i)) {
            let wider = shown.start.min(i)..shown.end.max(i + 1);
            if away(i) <= NEAR && wider.len() <= MOST_SHOWN {
                shown = wider;
            }
        }
    }
    shown
}

/// How many changed lines of the step's hunk `excerpt` leaves out, to say
/// so under it.
pub fn beyond(f: &File, s: &Step) -> usize {
    let (Some((_, hunk)), shown) = (at(f, s.line), excerpt(f, s)) else {
        return 0;
    };
    hunk.clone().filter(|i| !shown.contains(i) && f.lines[*i].mark != Mark::Same && !f.lines[*i].moved).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::{fixture, review};
    use serde_json::json;

    #[test]
    fn claude_reads_the_source_hunks_by_their_new_line_numbers() {
        let listing = listing(&review(&fixture::sides()));
        insta::assert_snapshot!(listing);
    }

    #[test]
    fn a_step_off_the_change_is_dropped() {
        let r = review(&fixture::sides());
        let f = r.files.iter().position(|f| f.path == "src/sum.ts").unwrap();
        let added = r.files[f].lines.iter().find(|l| l.mark == Mark::Add && !l.moved).unwrap().new;
        let same = r.files[f].lines.iter().enumerate().find(|(i, l)| l.mark == Mark::Same && !r.files[f].hunks.iter().any(|h| h.start <= *i && *i <= h.end)).unwrap().1.new;
        let said = json!({ "summary": "It sums.", "cases": [], "steps": [
            { "path": "src/sum.ts", "line": added, "title": "Counting", "says": " It counts now. " },
            { "path": "src/sum.ts", "line": same, "says": "Nothing changed here." },
            { "path": "src/nowhere.ts", "line": 1, "says": "No such file." },
            { "path": "src/sum.ts", "line": 9999, "says": "No such line." },
        ], "flags": [
            { "path": "src/sum.ts", "line": added, "title": "Two names", "says": "It is total here and sum there.", "instead": "Call it total." },
            { "path": "src/sum.ts", "line": same, "title": "Old", "says": "This was here before.", "instead": "Leave it." },
        ] });
        assert_eq!(tour(&r, &said).flags.len(), 1);
        assert_eq!(tour(&r, &said).steps, [Step { file: f, line: added, from: 0, to: 0, title: "Counting".into(), says: "It counts now.".into(), ask: String::new() }]);
    }

    #[test]
    fn a_step_shows_the_lines_claude_asked_for_and_any_name_it_uses_nearby() {
        let r = review(&fixture::sides());
        let f = &r.files[r.files.iter().position(|f| f.path == "src/sum.ts").unwrap()];
        let shown = |s: &Step| f.lines[excerpt(f, s)].iter().filter(|l| l.mark != Mark::Del).map(|l| l.new).collect::<Vec<_>>();
        let step = |from, to, says: &str| Step { file: 0, line: 8, from, to, title: String::new(), says: says.into(), ask: String::new() };
        assert_eq!(shown(&step(7, 9, "It reports.")), [7, 8, 9]);
        // `reduce` is on line 4, which the step didn't ask for.
        assert_eq!(shown(&step(7, 9, "It calls what uses `reduce`.")), [4, 5, 6, 7, 8, 9]);
        // Lines that don't hold the step's own give way to its hunk.
        assert_eq!(shown(&step(1, 2, "It reports.")), shown(&step(0, 0, "It reports.")));
    }

    #[test]
    fn a_tour_still_being_written_reads_to_its_last_whole_step() {
        let r = review(&fixture::sides());
        let written = r#"{"summary":"It sums.","cases":[{"when":"No items","before":"0","now":"0"}],"steps":[{"path":"src/sum.ts","line":4,"says":"One call now."},{"path":"src/sum.ts","line":8,"says":"The rep"#;
        let so_far = tour(&r, &whole(written));
        assert_eq!((so_far.summary.as_str(), so_far.cases.len(), so_far.steps.len()), ("It sums.", 1, 1));
        assert_eq!(whole(r#"{"summary":"It su"#), Value::Null);
    }
}
