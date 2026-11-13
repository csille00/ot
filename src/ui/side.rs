//! The threads beside a conversation, to switch between them as a file tree does.
use super::{fg, width, Out, AMBER, BAND, DIM, FAINT, GREEN, TEAL, TEXT};
use crate::hc::{Agent, Status};
use crate::model::ago;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::Frame;

/// How wide the list is, its border included.
pub const WIDTH: u16 = 34;

/// Narrower than this, the conversation keeps the whole screen.
pub const FROM: u16 = 100;

/// The list: its threads, the one you are reading, and the cursor while it has the keys.
pub struct Side<'a> {
    pub threads: &'a [Agent],
    pub reading: Option<&'a str>,
    pub cursor: Option<&'a str>,
}

pub fn draw(f: &mut Frame, area: Rect, s: &Side, now: i64) {
    let w = area.width.saturating_sub(1) as usize;
    for y in area.y..area.bottom() {
        f.render_widget(Line::styled("│", fg(FAINT)), Rect::new(area.right() - 1, y, 1, 1));
    }
    let focused = s.cursor.is_some();
    let mut o = Out::default();
    o.put(" Threads", fg(if focused { AMBER } else { DIM }).add_modifier(Modifier::BOLD));
    let count = s.threads.len().to_string();
    o.pad_to(w.saturating_sub(width(&count) + 1));
    o.put(&count, fg(FAINT));
    f.render_widget(o.line(), Rect::new(area.x, area.y, w as u16, 1));
    f.render_widget(Line::styled("─".repeat(w), fg(FAINT)), Rect::new(area.x, area.y + 1, w as u16, 1));
    let rows = area.height.saturating_sub(2) as usize;
    let mark = s.cursor.or(s.reading);
    let at = mark.and_then(|m| s.threads.iter().position(|a| a.address == m)).unwrap_or(0);
    let top = at.saturating_sub(rows.saturating_sub(1));
    for (i, a) in s.threads.iter().enumerate().skip(top).take(rows) {
        let on = focused && s.cursor == Some(a.address.as_str());
        let reading = s.reading == Some(a.address.as_str());
        let bg = if on { Style::new().bg(BAND) } else { Style::new() };
        let (glyph, color) = match a.status {
            Status::Working => ("●", GREEN),
            Status::Blocked => ("◐", AMBER),
            Status::Stopped => ("○", DIM),
        };
        let when = ago(now - a.at);
        let mut o = Out::default();
        o.put(if on { "▌" } else if reading { "▸" } else { " " }, bg.fg(if on { AMBER } else { TEAL }));
        o.put(glyph, bg.fg(color));
        o.put(" ", bg);
        let room = w.saturating_sub(o.w + width(&when) + 2);
        let name = if reading { bg.fg(TEAL).add_modifier(Modifier::BOLD) } else { bg.fg(TEXT) };
        o.put(&cut(&a.name, room), name);
        pad(&mut o, w.saturating_sub(width(&when) + 1), bg);
        o.put(&when, bg.fg(FAINT));
        pad(&mut o, w, bg);
        f.render_widget(o.line(), Rect::new(area.x, area.y + 2 + (i - top) as u16, w as u16, 1));
    }
}

fn pad(o: &mut Out, col: usize, style: Style) {
    if col > o.w {
        o.put(&" ".repeat(col - o.w), style);
    }
}

fn cut(s: &str, w: usize) -> String {
    if width(s) <= w {
        return s.to_string();
    }
    let mut out = String::new();
    for c in s.chars() {
        if width(&out) + width(c.encode_utf8(&mut [0; 4])) + 1 > w {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

