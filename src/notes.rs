//! Your day at the board's right: today's TODOs and your scratch notes, as
//! plain markdown files under one folder you can grep and sync (OT_NOTES, else
//! ~/notes). Each day's TODOs are `todo/YYYY-MM-DD.md`, a list of `- [ ]`
//! boxes; a day that has no file yet starts with what the last day left
//! unchecked, each marked with the day it was first written. Notes are any
//! other `.md` in the folder, named by their first heading.
use jiff::civil::Date;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// The folder notes and TODOs live in.
pub fn dir() -> PathBuf {
    match std::env::var_os("OT_NOTES") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("notes"),
    }
}

/// One box on a day's list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Todo {
    pub text: String,
    pub done: bool,
    /// The day it was first written, when that is before the day it is on.
    pub from: Option<Date>,
    /// Its line in the day's file.
    line: usize,
}

/// A day's TODOs, as its file says.
#[derive(Clone, Debug, Default)]
pub struct Day {
    pub date: Option<Date>,
    pub path: PathBuf,
    lines: Vec<String>,
    pub todos: Vec<Todo>,
}

const FROM: &str = "<!-- from ";

fn todo_dir(root: &Path) -> PathBuf {
    root.join("todo")
}

fn day_path(root: &Path, date: Date) -> PathBuf {
    todo_dir(root).join(format!("{date}.md"))
}

fn parse(line: &str, at: usize) -> Option<Todo> {
    let t = line.trim_start();
    let rest = t.strip_prefix("- ").or_else(|| t.strip_prefix("* "))?;
    let (done, text) = if let Some(r) = rest.strip_prefix("[ ] ") {
        (false, r)
    } else if let Some(r) = rest.strip_prefix("[x] ").or_else(|| rest.strip_prefix("[X] ")) {
        (true, r)
    } else {
        return None;
    };
    let (text, from) = match text.rfind(FROM) {
        Some(i) => {
            let tag = text[i + FROM.len()..].trim_end().trim_end_matches("-->").trim();
            (text[..i].trim_end(), tag.parse().ok())
        }
        None => (text.trim_end(), None),
    };
    Some(Todo { text: text.to_string(), done, from, line: at })
}

impl Day {
    fn of(path: PathBuf, date: Date, text: &str) -> Day {
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        let todos = lines.iter().enumerate().filter_map(|(i, l)| parse(l, i)).collect();
        Day { date: Some(date), path, lines, todos }
    }

    /// The day's list, its file started from the last day's unchecked boxes
    /// when it has none yet and there are some.
    pub fn read(root: &Path, date: Date) -> Day {
        let path = day_path(root, date);
        // Only a day with no file starts from the last; one ot can't read is
        // left as it is.
        match fs::read(&path) {
            Ok(bytes) => return Day::of(path, date, &String::from_utf8_lossy(&bytes)),
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Day::of(path, date, ""),
            Err(_) => {}
        }
        let mut day = Day::of(path, date, "");
        if let Some((was, last)) = last_before(root, date) {
            let left: Vec<String> = last
                .todos
                .iter()
                .filter(|t| !t.done)
                .map(|t| format!("- [ ] {} {FROM}{} -->", t.text, t.from.unwrap_or(was)))
                .collect();
            if !left.is_empty() {
                day.lines = heading(date).into_iter().chain(left).collect();
                let _ = day.save();
                day = Day::of(day.path.clone(), date, &day.lines.join("\n"));
            }
        }
        day
    }

    fn save(&self) -> Result<(), String> {
        if let Some(d) = self.path.parent() {
            fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        let mut text = self.lines.join("\n");
        text.push('\n');
        fs::write(&self.path, text).map_err(|e| format!("{}: {e}", self.path.display()))
    }

    /// The boxes in the order they show: unchecked first, as the file has
    /// them, then those checked.
    pub fn shown(&self) -> Vec<&Todo> {
        let mut v: Vec<&Todo> = self.todos.iter().collect();
        v.sort_by_key(|t| t.done);
        v
    }

    /// The list as its file says now, so a write keeps what nvim or anything
    /// else saved since ot read it.
    fn fresh(&mut self) {
        if let (Some(date), Ok(text)) = (self.date, fs::read_to_string(&self.path)) {
            *self = Day::of(self.path.clone(), date, &text);
        }
    }

    pub fn add(&mut self, text: &str) -> Result<(), String> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        self.fresh();
        if self.lines.is_empty() {
            if let Some(d) = self.date {
                self.lines = heading(d);
            }
        }
        // After the last box, so a list keeps together under its heading.
        let at = self.todos.iter().map(|t| t.line + 1).max().unwrap_or(self.lines.len());
        self.lines.insert(at, format!("- [ ] {text}"));
        self.save()?;
        *self = Day::of(self.path.clone(), self.date.unwrap(), &self.lines.join("\n"));
        Ok(())
    }

    /// Checks the `i`th box shown, or unchecks it.
    pub fn toggle(&mut self, i: usize) -> Result<(), String> {
        let Some(t) = self.shown().get(i).map(|t| (*t).clone()) else { return Ok(()) };
        self.fresh();
        let Some(t) = self.todos.iter().find(|n| n.text == t.text && n.done == t.done).cloned() else { return Ok(()) };
        let l = &mut self.lines[t.line];
        *l = if t.done {
            l.replacen("[x]", "[ ]", 1).replacen("[X]", "[ ]", 1)
        } else {
            l.replacen("[ ]", "[x]", 1)
        };
        self.save()?;
        *self = Day::of(self.path.clone(), self.date.unwrap(), &self.lines.join("\n"));
        Ok(())
    }

    /// The day's file, there for nvim to open even before the day has a box.
    pub fn file(&self) -> Result<PathBuf, String> {
        if !self.path.exists() {
            let mut d = self.clone();
            d.lines = self.date.map(heading).unwrap_or_default();
            d.save()?;
        }
        Ok(self.path.clone())
    }
}

fn heading(date: Date) -> Vec<String> {
    vec![format!("# {}", date.strftime("%A, %B %-d")), String::new()]
}

/// The last day before `date` that has a list.
fn last_before(root: &Path, date: Date) -> Option<(Date, Day)> {
    let was = fs::read_dir(todo_dir(root))
        .ok()?
        .filter_map(|e| e.ok()?.path().file_stem()?.to_str()?.parse::<Date>().ok())
        .filter(|d| *d < date)
        .max()?;
    let path = day_path(root, was);
    let text = fs::read_to_string(&path).ok()?;
    Some((was, Day::of(path, was, &text)))
}

/// A scratch note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub path: PathBuf,
    pub title: String,
    /// When it was last written, in seconds.
    pub at: i64,
}

/// Every note in the folder, the one written last first.
pub fn notes(root: &Path) -> Vec<Note> {
    let Ok(entries) = fs::read_dir(root) else { return vec![] };
    let mut v: Vec<Note> = entries
        .filter_map(|e| {
            let path = e.ok()?.path();
            if path.extension()? != "md" || !path.is_file() {
                return None;
            }
            let at = fs::metadata(&path).ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
            let text = fs::read_to_string(&path).unwrap_or_default();
            Some(Note { title: title(&text, &path), path, at })
        })
        .collect();
    v.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.title.cmp(&b.title)));
    v
}

fn title(text: &str, path: &Path) -> String {
    let first = text.lines().map(str::trim).find(|l| !l.is_empty());
    match first {
        Some(l) => l.trim_start_matches('#').trim().to_string(),
        None => path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
    }
}

/// A new note titled `title`, its file named for it.
pub fn create(root: &Path, title: &str) -> Result<PathBuf, String> {
    let title = title.trim();
    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.is_empty() { "note".to_string() } else { slug };
    fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let mut path = root.join(format!("{slug}.md"));
    let mut n = 2;
    while path.exists() {
        path = root.join(format!("{slug}-{n}.md"));
        n += 1;
    }
    fs::write(&path, format!("# {title}\n\n")).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// What nvim is started with to write in a note: the file, its last line,
/// and the markdown rendered as you type when render-markdown.nvim is there.
pub fn nvim_args(path: &Path) -> Vec<String> {
    vec![
        "+".into(),
        "-c".into(),
        format!("lua {}", NVIM_LUA.replace('\n', " ")),
        path.to_string_lossy().into_owned(),
    ]
}

const NVIM_LUA: &str = r#"vim.opt_local.wrap = true
vim.opt_local.linebreak = true
local ok, rm = pcall(require, "render-markdown")
if ok then pcall(rm.setup, { render_modes = true }) end"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ot-notes-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn date(s: &str) -> Date {
        s.parse().unwrap()
    }

    #[test]
    fn a_new_day_starts_with_what_the_last_one_left_unchecked() {
        let r = root("carry");
        fs::create_dir_all(r.join("todo")).unwrap();
        fs::write(
            r.join("todo/2026-11-12.md"),
            "# Thursday\n\n- [x] standup\n- [ ] rebase cart-ui <!-- from 2026-11-10 -->\n- [ ] write coupon tests\n",
        )
        .unwrap();
        let d = Day::read(&r, date("2026-11-15"));
        let shown: Vec<(&str, bool, Option<Date>)> = d.shown().iter().map(|t| (t.text.as_str(), t.done, t.from)).collect();
        assert_eq!(
            shown,
            [
                ("rebase cart-ui", false, Some(date("2026-11-10"))),
                ("write coupon tests", false, Some(date("2026-11-12"))),
            ]
        );
        assert!(fs::read_to_string(r.join("todo/2026-11-15.md")).unwrap().starts_with("# Sunday, November 15\n"));
        // A day already written isn't started again.
        fs::write(r.join("todo/2026-11-16.md"), "- [ ] only this\n").unwrap();
        assert_eq!(Day::read(&r, date("2026-11-16")).todos.len(), 1);
    }

    #[test]
    fn boxes_add_under_the_list_and_checked_ones_sink() {
        let r = root("toggle");
        let mut d = Day::read(&r, date("2026-11-15"));
        assert!(d.todos.is_empty() && !d.path.exists());
        d.add("reply to alice").unwrap();
        d.add("ask bob").unwrap();
        d.toggle(0).unwrap();
        let shown: Vec<(&str, bool)> = d.shown().iter().map(|t| (t.text.as_str(), t.done)).collect();
        assert_eq!(shown, [("ask bob", false), ("reply to alice", true)]);
        assert_eq!(
            fs::read_to_string(&d.path).unwrap(),
            "# Sunday, November 15\n\n- [x] reply to alice\n- [ ] ask bob\n"
        );
        d.toggle(1).unwrap();
        assert!(d.todos.iter().all(|t| !t.done));
    }

    #[test]
    fn notes_are_named_by_their_heading_and_new_ones_by_a_slug() {
        let r = root("notes");
        let a = create(&r, "Coupon edge cases!").unwrap();
        let b = create(&r, "Coupon edge cases").unwrap();
        assert_eq!(a.file_name().unwrap(), "coupon-edge-cases.md");
        assert_eq!(b.file_name().unwrap(), "coupon-edge-cases-2.md");
        fs::write(r.join("plain.md"), "\n\nrate-limit numbers\n").unwrap();
        fs::create_dir_all(r.join("todo")).unwrap();
        let mut titles: Vec<String> = notes(&r).into_iter().map(|n| n.title).collect();
        titles.sort();
        assert_eq!(titles, ["Coupon edge cases", "Coupon edge cases!", "rate-limit numbers"]);
    }
}
