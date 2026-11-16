//! Your day at the board's right, on a terminal wide enough: today's TODOs,
//! then your notes (notes.rs). tab takes the keys there and on round.
use super::{fg, put, width, Out, BAND, DIM, FAINT, TEAL, TEXT};
use crate::model::ago;
use crate::notes::{Day, Note};
use jiff::civil::Date;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::Frame;

/// The terminal width the pane shows from.
pub const FROM: u16 = 140;

/// The part of the pane that has the keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Todos,
    Notes,
}

/// The pane's own state: hidden by D, which part has the keys, and where the
/// cursor is in each.
#[derive(Debug, Default)]
pub struct View {
    pub hidden: bool,
    pub focus: Option<Part>,
    pub todo_at: usize,
    pub note_at: usize,
}

/// How wide the pane is beside a board `total` columns wide.
pub fn cols(total: u16) -> u16 {
    (total * 2 / 5).max(48).min(total)
}

/// The day of the week a box was carried from, or its date past a week.
fn since(from: Date, today: Date) -> String {
    let days = (today - from).get_days();
    if (0..7).contains(&days) {
        from.strftime("%a").to_string().to_lowercase()
    } else {
        from.strftime("%b %-d").to_string().to_lowercase()
    }
}

pub fn draw(f: &mut Frame, area: Rect, day: &Day, notes: &[Note], v: &View, today: Date, now: i64) {
    let w = (area.width as usize).saturating_sub(3);
    let todos = day.shown();
    let row = |o: &mut Out, on: bool, left: &[(String, Style)], right: (String, Style)| {
        o.put("│", fg(FAINT));
        let bg = if on { Style::new().bg(BAND) } else { Style::new() };
        o.put(if on { "▌" } else { " " }, fg(TEAL));
        let room = w.saturating_sub(width(&right.0) + 1);
        let start = o.w;
        for (text, style) in left {
            let left = room.saturating_sub(o.w - start);
            o.put(&super::side::cut(text, left), bg.patch(*style));
        }
        o.put(&" ".repeat((start + room + 1).saturating_sub(o.w)), bg);
        o.put(&right.0, bg.patch(right.1));
        o.put(" ", bg);
    };
    let head = |title: &str, count: String, on: bool| {
        let mut o = Out::default();
        o.put("│ ", fg(FAINT));
        o.put(title, if on { fg(TEAL).bold() } else { fg(TEXT).bold() });
        let date = if title == "Today" { format!("  {}", today.strftime("%a %b %-d")) } else { String::new() };
        o.put(&date, fg(DIM));
        o.pad_to(w + 2 - width(&count));
        o.put(&count, fg(DIM));
        o.line()
    };
    let blank = || {
        let mut o = Out::default();
        o.put("│", fg(FAINT));
        o.line()
    };
    let h = area.height as usize;
    let mut lines: Vec<Line<'static>> = vec![];
    let on_todos = v.focus == Some(Part::Todos);
    let done = todos.iter().filter(|t| t.done).count();
    lines.push(head("Today", format!("{done}/{}", todos.len()), on_todos));
    // The TODOs keep half the pane at most; a long list scrolls with its cursor.
    let room = todos.len().max(1).min(h.saturating_sub(6) / 2).max(1);
    let skip = v.todo_at.saturating_sub(room - 1).min(todos.len().saturating_sub(room));
    if todos.is_empty() {
        let mut o = Out::default();
        row(&mut o, false, &[("nothing for today".into(), fg(DIM))], ((if on_todos { "a adds one" } else { "" }).into(), fg(DIM)));
        lines.push(o.line());
    }
    for (i, t) in todos.iter().enumerate().skip(skip).take(room) {
        let mut o = Out::default();
        let on = on_todos && i == v.todo_at;
        let (mark, text) = if t.done { ("[x] ", fg(FAINT)) } else { ("[ ] ", fg(TEXT)) };
        let from = t.from.map(|d| since(d, today)).unwrap_or_default();
        row(&mut o, on, &[(mark.into(), fg(if t.done { TEAL } else { DIM })), (t.text.clone(), text)], (from, fg(DIM)));
        lines.push(o.line());
    }
    lines.push(blank());
    let mut o = Out::default();
    o.put("├", fg(FAINT));
    o.put(&"─".repeat(w + 1), fg(FAINT));
    lines.push(o.line());
    let on_notes = v.focus == Some(Part::Notes);
    lines.push(head("Notes", notes.len().to_string(), on_notes));
    let room = h.saturating_sub(lines.len()).max(1);
    let skip = v.note_at.saturating_sub(room - 1);
    if notes.is_empty() {
        let mut o = Out::default();
        row(&mut o, false, &[("no notes yet".into(), fg(DIM))], ((if on_notes { "n starts one" } else { "" }).into(), fg(DIM)));
        lines.push(o.line());
    }
    for (i, n) in notes.iter().enumerate().skip(skip).take(room) {
        let mut o = Out::default();
        row(&mut o, on_notes && i == v.note_at, &[(n.title.clone(), fg(TEXT))], (ago(now - n.at), fg(DIM)));
        lines.push(o.line());
    }
    while lines.len() < h {
        lines.push(blank());
    }
    let mut y = 0;
    for l in lines.into_iter().take(h) {
        put(f, area, &mut y, l);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::fs;

    #[test]
    fn todays_boxes_then_the_notes() {
        let root = std::env::temp_dir().join(format!("ot-day-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("todo")).unwrap();
        fs::write(
            root.join("todo/2026-11-15.md"),
            "# Sunday\n\n- [x] reply to alice on !6\n- [ ] rebase cart-ui on main\n- [ ] ask bob about the login copy <!-- from 2026-11-14 -->\n- [ ] a box from long ago <!-- from 2026-10-30 -->\n",
        )
        .unwrap();
        let today: Date = "2026-11-15".parse().unwrap();
        let day = Day::read(&root, today);
        let now = 1_794_800_000;
        let note = |title: &str, ago: i64| Note { path: root.join(title), title: title.into(), at: now - ago };
        let notes = [note("coupon edge cases", 7200), note("rate-limit numbers", 90_000)];
        let v = View { focus: Some(Part::Todos), todo_at: 1, ..View::default() };
        let mut t = Terminal::new(TestBackend::new(48, 14)).unwrap();
        t.draw(|f| draw(f, f.area(), &day, &notes, &v, today, now)).unwrap();
        insta::assert_snapshot!(crate::ui::text(&t));
    }
}
