//! One of Claude's sessions, or one agent it started, read in full: what
//! was asked, what it said, and what it did between, a line each. It opens
//! at the end, where the newest is, and stays there as it goes on until
//! you scroll.
use super::{fg, put, width, wrap, Out, DIM, FAINT, PINK, TEAL, TEXT};
use crate::claude::{Session, State, Turn, Who};
use crate::keys::Place;
use crate::model::ago;
use ratatui::text::Line;
use ratatui::Frame;

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

pub fn draw(f: &mut Frame, t: &Transcript, now: i64, v: &mut View) {
    let area = f.area();
    let right = (area.width as usize).saturating_sub(1);
    let head = header(t, right);
    let body = lines(t, now, right);
    let foot = super::footer(&[Place::Transcript], right);
    v.room = (area.height as usize).saturating_sub(head.len() + 2);
    let most = body.len().saturating_sub(v.room);
    if v.follow {
        v.scroll = most;
    }
    v.scroll = v.scroll.min(most);
    let mut y = 0;
    for l in head
        .into_iter()
        .chain([Line::default()])
        .chain(body.into_iter().skip(v.scroll).take(v.room))
    {
        put(f, area, &mut y, l);
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
        o.put(&part, fg(TEXT));
        if i == 0 {
            o.pad_to(right.saturating_sub(width(&facts)).max(o.w + 2));
            o.put(&facts, fg(DIM));
        }
        out.push(o.line());
    }
    out
}

/// The turns as lines: a label on the first line of what was asked or
/// said, the text wrapped under itself, and what was done as dim lines. An
/// agent's turns sit under the call that started it, a step in.
fn lines(t: &Transcript, now: i64, right: usize) -> Vec<Line<'static>> {
    let mut out = vec![];
    turn_lines(&mut out, t.turns, t.voices, now, right, 0);
    out
}

fn turn_lines(out: &mut Vec<Line<'static>>, turns: &[Turn], voices: [&str; 2], now: i64, right: usize, depth: usize) {
    let col = TEXT_COL + 2 * depth;
    let w = right.saturating_sub(col).max(8);
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
                o.put("· ", fg(FAINT));
                o.put(&part, fg(DIM));
                if let (0, Some(times)) = (i, &times) {
                    o.put(times, fg(FAINT));
                }
                out.push(o.line());
            }
            if let Some(a) = &t.agent {
                turn_lines(out, &a.turns, voices, now, right, depth + 1);
            }
            continue;
        }
        if !out.is_empty() {
            out.push(Line::default());
        }
        let when = ago(now - t.at);
        for (i, para) in t.text.lines().enumerate() {
            let parts = if para.trim().is_empty() {
                vec![String::new()]
            } else {
                wrap(
                    para,
                    w.saturating_sub(if i == 0 { width(&when) + 2 } else { 0 }),
                )
            };
            for (j, part) in parts.into_iter().enumerate() {
                let mut o = Out::default();
                if i == 0 && j == 0 {
                    o.pad_to(2 + 2 * depth);
                    o.put(label, fg(color));
                }
                o.pad_to(col);
                o.put(&part, fg(TEXT));
                if i == 0 && j == 0 {
                    o.pad_to(right.saturating_sub(width(&when)).max(o.w + 2));
                    o.put(&when, fg(FAINT));
                }
                out.push(o.line());
            }
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
        t.draw(|f| draw(f, &Transcript::session(s, fixture::NOW), fixture::NOW, &mut v)).unwrap();
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
    fn a_tiny_terminal_still_draws() {
        for (w, h) in [(1, 1), (12, 4), (30, 6)] {
            frame(w, h, |v| v.step(3));
        }
    }
}
