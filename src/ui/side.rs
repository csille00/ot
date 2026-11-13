//! The threads beside a conversation, to switch between them as a file tree does.
use super::{fg, width, Out, AMBER, BAND, DIM, FAINT, GREEN, TEAL, TEXT};
use crate::hc::{Agent, Status};
use crate::model::ago;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::Frame;

/// How wide the list is, its border included.
pub const WIDTH: u16 = 44;

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
    // Each thread is its title on up to two lines, its time on the first.
    let room = w.saturating_sub(3 + 5);
    let lines: Vec<Vec<String>> = s.threads.iter().map(|a| two_lines(&a.name, room)).collect();
    let mut top = 0;
    while top < at && lines[top..=at].iter().map(Vec::len).sum::<usize>() > rows {
        top += 1;
    }
    let mut y = area.y + 2;
    for (i, a) in s.threads.iter().enumerate().skip(top) {
        if y + lines[i].len() as u16 > area.bottom() {
            break;
        }
        let on = focused && s.cursor == Some(a.address.as_str());
        let reading = s.reading == Some(a.address.as_str());
        let bg = if on { Style::new().bg(BAND) } else { Style::new() };
        let (glyph, color) = match a.status {
            Status::Working => ("●", GREEN),
            Status::Blocked => ("◐", AMBER),
            Status::Stopped => ("○", DIM),
        };
        let name = if reading { bg.fg(TEAL).add_modifier(Modifier::BOLD) } else { bg.fg(TEXT) };
        for (k, part) in lines[i].iter().enumerate() {
            let mut o = Out::default();
            let edge = if on { "▌" } else if reading && k == 0 { "▸" } else { " " };
            o.put(edge, bg.fg(if on { AMBER } else { TEAL }));
            o.put(if k == 0 { glyph } else { " " }, bg.fg(color));
            o.put(" ", bg);
            o.put(part, if k == 0 { name } else { name.patch(fg(DIM)) });
            if k == 0 {
                let when = ago(now - a.at);
                pad(&mut o, w.saturating_sub(width(&when) + 1), bg);
                o.put(&when, bg.fg(FAINT));
            }
            pad(&mut o, w, bg);
            f.render_widget(o.line(), Rect::new(area.x, y, w as u16, 1));
            y += 1;
        }
    }
}

/// `s` wrapped to `w` on at most two lines, the second cut short if it must be.
fn two_lines(s: &str, w: usize) -> Vec<String> {
    let mut parts = super::wrap(s, w);
    if parts.len() > 2 {
        let rest = parts[1..].join(" ");
        parts = vec![parts[0].clone(), cut(&rest, w)];
    }
    parts
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

