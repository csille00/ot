//! One of Claude's sessions, or one agent it started, read in full: what
//! was asked, what it said, and what it did between, a line each. It opens
//! at the end, where the newest is, and stays there as it goes on until
//! you scroll.
use super::{fg, md, put, width, wrap, Out, BAND, DIM, FAINT, PINK, TEAL, TEXT};
use crate::claude::{Session, State, Turn, Who};
use crate::keys::Place;
use crate::model::ago;
use ratatui::layout::{Rect, Size};
use ratatui::text::Line;
use ratatui::Frame;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use ratatui_image::{Image, Resize};
use std::collections::HashMap;
use std::path::Path;

/// Images drawn in conversations: the terminal's way of drawing them, and each one
/// made at a size, kept so it goes to the terminal once.
pub struct Images {
    picker: Picker,
    made: HashMap<(String, u16, u16), Option<Protocol>>,
}

impl Images {
    pub fn new(picker: Picker) -> Images {
        Images { picker, made: HashMap::new() }
    }

    /// Forgets what was drawn, so the next draw sends each image again.
    pub fn forget(&mut self) {
        self.made.clear();
    }

    fn font(&self) -> (u16, u16) {
        let f = self.picker.font_size();
        (f.width, f.height)
    }

    fn get(&mut self, p: &Placement) -> Option<&Protocol> {
        let picker = &self.picker;
        self.made
            .entry((p.path.clone(), p.w, p.h))
            .or_insert_with(|| {
                let img = image::ImageReader::open(&p.path).ok()?.with_guessed_format().ok()?.decode().ok()?;
                picker.new_protocol(img, Size::new(p.w, p.h), Resize::Fit(None)).ok()
            })
            .as_ref()
    }
}

/// Where an image goes among the body's lines: its rows start at `line`.
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    line: usize,
    col: usize,
    w: u16,
    h: u16,
    path: String,
}

/// The widest and tallest an image is drawn, in cells.
const IMAGE_W: usize = 72;
const IMAGE_H: usize = 22;

/// Files bigger than this aren't decoded.
const IMAGE_BYTES: u64 = 25 << 20;

/// Where the text starts: after the margin and the widest label.
const TEXT_COL: usize = 11;

#[derive(Debug, Default)]
pub struct View {
    scroll: usize,
    /// Stuck to the end, until you scroll away from it.
    follow: bool,
    /// How many lines the last draw had room for, which a page is.
    room: usize,
}

impl View {
    pub fn at_end() -> View {
        View {
            follow: true,
            ..Default::default()
        }
    }

    pub fn step(&mut self, by: isize) {
        self.scroll = self.scroll.saturating_add_signed(by);
        self.follow = false;
    }

    pub fn page(&mut self, by: isize) {
        self.step(by * (self.room / 2).max(1) as isize);
    }

    pub fn top(&mut self) {
        self.scroll = 0;
        self.follow = false;
    }

    pub fn bottom(&mut self) {
        self.follow = true;
    }
}

/// What is read: a title, how it stands, and the turns, with what to call
/// the two who talk at the top of them.
pub struct Transcript<'a> {
    pub title: String,
    pub facts: Vec<String>,
    pub turns: &'a [Turn],
    /// Who asks and who answers: "you" and "claude" in a session; in an
    /// agent the ticket swarm started, its lead and its name.
    pub voices: [&'a str; 2],
}

impl<'a> Transcript<'a> {
    pub fn session(s: &'a Session, now: i64) -> Transcript<'a> {
        let mut facts = vec![s.state.word().to_string(), ago(now - s.at)];
        if s.cost > 0.0 {
            facts.push(format!("${:.2}", s.cost));
        }
        Transcript { title: s.title.clone(), facts, turns: &s.turns, voices: ["you", "claude"] }
    }

    /// The agent of `s` named `name`, the last it started by that name.
    pub fn agent(s: &'a Session, name: &'a str, now: i64) -> Option<Transcript<'a>> {
        let a = s.agent(name)?;
        let state = match (s.state, a.waiting) {
            (State::Gone, _) => "done",
            (_, true) => "waiting",
            _ => "working",
        };
        let at = a.turns.last().map_or(s.at, |t| t.at);
        Some(Transcript {
            title: format!("{name}, an agent of {}", s.title),
            facts: vec![state.to_string(), ago(now - at)],
            turns: &a.turns,
            voices: ["lead", name],
        })
    }
}

pub fn draw(f: &mut Frame, t: &Transcript, now: i64, v: &mut View, places: &[Place], note: Option<&str>, mut images: Option<&mut Images>) {
    let area = f.area();
    let right = (area.width as usize).saturating_sub(1);
    let head = header(t, right);
    let font = images.as_ref().map(|i| i.font());
    let (body, placed) = lines(t, now, right, font);
    let foot = match note {
        Some(n) => {
            let mut o = Out::default();
            o.put("  ", fg(DIM));
            o.put(n, fg(DIM));
            o.line()
        }
        None => super::footer(places, right),
    };
    v.room = (area.height as usize).saturating_sub(head.len() + 3);
    let most = body.len().saturating_sub(v.room);
    if v.follow {
        v.scroll = most;
    }
    v.scroll = v.scroll.min(most);
    let mut y = 0;
    let top = head.len() + 2;
    let rule = Line::styled("─".repeat(right + 1), fg(FAINT));
    for l in head
        .into_iter()
        .chain([rule, Line::default()])
        .chain(body.into_iter().skip(v.scroll).take(v.room))
    {
        put(f, area, &mut y, l);
    }
    // An image is drawn only while all of it is on screen; its caption stays either way.
    if let Some(images) = images.as_deref_mut() {
        for p in placed.iter().filter(|p| p.line >= v.scroll && p.line + p.h as usize <= v.scroll + v.room) {
            let at = Rect::new(area.x + p.col as u16, area.y + (top + p.line - v.scroll) as u16, p.w, p.h);
            if at.right() <= area.right() && at.bottom() <= area.bottom() {
                if let Some(proto) = images.get(p) {
                    f.render_widget(Image::new(proto), at);
                }
            }
        }
    }
    y = (area.height as usize).saturating_sub(1);
    put(f, area, &mut y, foot);
}

/// The title, with how it stands at the right; a title that doesn't fit
/// beside that wraps under itself.
fn header(t: &Transcript, right: usize) -> Vec<Line<'static>> {
    let facts = t.facts.join("  ·  ");
    let room = right.saturating_sub(5 + width(&facts) + 2).max(8);
    let mut out = vec![];
    for (i, part) in wrap(&t.title, room).into_iter().enumerate() {
        let mut o = Out::default();
        if i == 0 {
            o.put(" ot", fg(TEAL).bold());
        }
        o.pad_to(5);
        o.put(&part, fg(TEXT).bold());
        if i == 0 {
            o.pad_to(right.saturating_sub(width(&facts)).max(o.w + 2));
            o.put(&facts, fg(DIM));
        }
        out.push(o.line());
    }
    out
}

/// How wide text runs at most: a line longer than this is hard to read.
const READ: usize = 100;

/// The turns as lines: a label on the first line of what was asked or
/// said, the text as markdown under itself (yours on a band), and what was
/// done as dim lines. An agent's turns sit under the call that started it.
fn lines(t: &Transcript, now: i64, right: usize, font: Option<(u16, u16)>) -> (Vec<Line<'static>>, Vec<Placement>) {
    let (mut out, mut placed) = (vec![], vec![]);
    turn_lines(&mut out, &mut placed, t.turns, t.voices, now, right, 0, font);
    (out, placed)
}

/// What a line that is only `![alt](src)` shows.
enum Shows {
    Picture { path: String, w: u16, h: u16 },
    Video,
    Elsewhere,
}

/// The alt and source of a line that is an image and nothing else.
fn image_line(line: &str) -> Option<(&str, &str)> {
    let rest = line.trim().strip_prefix("![")?;
    let (alt, rest) = rest.split_once("](")?;
    let src = rest.strip_suffix(')')?;
    (!src.contains(' ')).then_some((alt, src))
}

/// How `src` is shown in `w` columns, with cells of `font` pixels when images draw.
fn shows(src: &str, w: usize, font: Option<(u16, u16)>) -> Shows {
    let ext = src.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    if ["mp4", "mov", "webm", "mkv", "m4v"].contains(&ext.as_str()) {
        return Shows::Video;
    }
    let path = Path::new(src);
    let (Some((fw, fh)), true) = (font, path.is_absolute()) else { return Shows::Elsewhere };
    if std::fs::metadata(path).map_or(true, |m| !m.is_file() || m.len() > IMAGE_BYTES) {
        return Shows::Elsewhere;
    }
    let Ok((pw, ph)) = image::image_dimensions(path) else { return Shows::Elsewhere };
    let cols = w.min(IMAGE_W).min((pw / fw.max(1) as u32).max(1) as usize).max(1);
    let rows_for = |cols: usize| ((cols as f64 * fw as f64 * ph as f64) / (pw as f64 * fh.max(1) as f64)).ceil() as usize;
    let (mut cols, mut rows) = (cols, rows_for(cols));
    if rows > IMAGE_H {
        cols = ((cols * IMAGE_H) / rows).max(1);
        rows = rows_for(cols).min(IMAGE_H);
    }
    Shows::Picture { path: src.to_string(), w: cols as u16, h: rows.max(1) as u16 }
}

/// A message's text as rows, with each line that is an image set apart: a caption,
/// then room for the picture, whose place is pushed to `pics` (by row, from 0).
fn message_rows(text: &str, w: usize, base: ratatui::style::Style, font: Option<(u16, u16)>, pics: &mut Vec<(usize, u16, u16, String)>) -> Vec<md::Row> {
    let mut rows: Vec<md::Row> = vec![];
    let mut chunk = String::new();
    let flush = |chunk: &mut String, rows: &mut Vec<md::Row>| {
        if !chunk.trim().is_empty() {
            if rows.last().is_some_and(|r| !r.is_empty()) {
                rows.push(vec![]);
            }
            rows.extend(md::render(chunk, w, base));
        }
        chunk.clear();
    };
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        let Some((alt, src)) = image_line(line).filter(|_| !fenced) else {
            chunk.push_str(line);
            chunk.push('\n');
            continue;
        };
        flush(&mut chunk, &mut rows);
        let name = src.rsplit('/').next().unwrap_or(src);
        let alt = if alt.trim().is_empty() { name } else { alt };
        match shows(src, w, font) {
            Shows::Picture { path, w: pw, h: ph } => {
                rows.push(vec![("▣ ".into(), base.fg(TEAL)), (alt.to_string(), base.fg(DIM))]);
                pics.push((rows.len(), pw, ph, path));
                rows.extend((0..ph).map(|_| vec![]));
            }
            Shows::Video => rows.push(vec![("▶ ".into(), base.fg(PINK)), (alt.to_string(), base), (format!("  {name}"), base.fg(FAINT))]),
            Shows::Elsewhere => rows.push(vec![("▣ ".into(), base.fg(TEAL)), (alt.to_string(), base), (format!("  {src}"), base.fg(FAINT))]),
        }
    }
    flush(&mut chunk, &mut rows);
    rows
}

#[allow(clippy::too_many_arguments)]
fn turn_lines(out: &mut Vec<Line<'static>>, placed: &mut Vec<Placement>, turns: &[Turn], voices: [&str; 2], now: i64, right: usize, depth: usize, font: Option<(u16, u16)>) {
    let col = TEXT_COL + 2 * depth;
    // Room on the right for when it was said.
    let w = right.saturating_sub(col + 6).clamp(8, READ);
    let [asks, says] = voices;
    for t in turns {
        let (label, color) = match (t.who, depth) {
            (Who::You, 0) => (asks, TEAL),
            (Who::You, _) => ("asked", FAINT),
            (Who::Claude, 0) => (says, PINK),
            (Who::Claude, _) => ("agent", PINK),
            (Who::Tool, _) => ("", DIM),
        };
        if t.who == Who::Tool {
            let times = (t.times > 1).then(|| format!(" ×{}", t.times));
            for (i, part) in wrap(&t.text, w.saturating_sub(2)).into_iter().enumerate() {
                let mut o = Out::default();
                o.pad_to(col);
                o.put(if i == 0 { "· " } else { "  " }, fg(FAINT));
                o.put(&part, fg(DIM));
                if let (0, Some(times)) = (i, &times) {
                    o.put(times, fg(FAINT));
                }
                out.push(o.line());
            }
            if let Some(a) = &t.agent {
                turn_lines(out, placed, &a.turns, voices, now, right, depth + 1, font);
            }
            continue;
        }
        if !out.is_empty() {
            out.push(Line::default());
        }
        let mut pics = vec![];
        let (rows, inset) = if t.who == Who::You {
            let base = fg(TEXT).bg(BAND);
            let rows = message_rows(&t.text, w.saturating_sub(2), base, font, &mut pics);
            let rows = rows.into_iter().map(|mut r| {
                r.insert(0, (" ".to_string(), base));
                r
            });
            (md::banded(rows.collect(), w), 1)
        } else {
            (message_rows(&t.text, w, fg(TEXT), font, &mut pics), 0)
        };
        let first = out.len();
        placed.extend(pics.into_iter().map(|(row, w, h, path)| Placement { line: first + row, col: col + inset, w, h, path }));
        let when = (t.at > 0).then(|| ago(now - t.at));
        for (i, row) in rows.into_iter().enumerate() {
            let mut o = Out::default();
            if i == 0 {
                o.pad_to(2 + 2 * depth);
                o.put(label, fg(color).bold());
            }
            o.pad_to(col);
            for (text, style) in row {
                o.put(&text, style);
            }
            if let (0, Some(when)) = (i, &when) {
                o.pad_to(col + w + 2);
                o.put(when, fg(FAINT));
            }
            out.push(o.line());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixture;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn frame(w: u16, h: u16, step: impl Fn(&mut View)) -> String {
        frame_of(1, w, h, step)
    }

    fn frame_of(which: usize, w: u16, h: u16, step: impl Fn(&mut View)) -> String {
        let s = &fixture::sessions()[which];
        let mut v = View::at_end();
        step(&mut v);
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, &Transcript::session(s, fixture::NOW), fixture::NOW, &mut v, &[Place::Transcript], None, None)).unwrap();
        crate::ui::text(&t)
    }

    #[test]
    fn a_session_at_80x16_opens_at_its_end() {
        insta::assert_snapshot!(frame(80, 16, |_| {}));
    }

    #[test]
    fn scrolled_to_the_top_it_starts_with_what_you_asked() {
        insta::assert_snapshot!(frame(80, 12, |v| v.top()));
    }

    #[test]
    fn an_agents_turns_sit_under_its_call() {
        insta::assert_snapshot!(frame_of(0, 80, 14, |_| {}));
    }

    #[test]
    fn a_line_that_is_only_an_image_is_one() {
        assert_eq!(image_line("  ![Part A](/tmp/a.mp4)  "), Some(("Part A", "/tmp/a.mp4")));
        assert_eq!(image_line("see ![a](/b.png)"), None);
        assert!(matches!(shows("/tmp/x/Part.MP4", 80, Some((8, 16))), Shows::Video));
        assert!(matches!(shows("t3-context://v1/image/x", 80, Some((8, 16))), Shows::Elsewhere));
    }

    #[test]
    fn a_tiny_terminal_still_draws() {
        for (w, h) in [(1, 1), (12, 4), (30, 6)] {
            frame(w, h, |v| v.step(3));
        }
    }
}
