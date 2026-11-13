//! The threads beside a conversation, to switch between them as a file tree does.
use super::{fg, width, Out, AMBER, BAND, DIM, FAINT, GREEN, TEAL, TEXT};
use crate::hc::{Agent, Status};
use crate::model::ago;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::Frame;
use std::collections::HashMap;

/// How wide the list is, its border included.
pub const WIDTH: u16 = 34;

/// Narrower than this, the conversation keeps the whole screen.
pub const FROM: u16 = 100;

/// The list: its threads, the one you are reading, and the cursor while it has the keys.
pub struct Side<'a> {
    pub threads: &'a [Agent],
    /// By thread: its MR ("!78877"), and the last thing said in it, by you or not.
    pub mrs: &'a HashMap<String, String>,
    pub said: &'a HashMap<String, (bool, String)>,
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
    // Each thread is its title, how it stands and its MR, and the last thing said,
    // then a blank line.
    let room = w.saturating_sub(3 + 5);
    let lines: Vec<Vec<(String, Style)>> = s.threads.iter().map(|a| entry(a, s, room)).collect();
    let tall = |from: usize, to: usize| lines[from..to].iter().map(|l| l.len() + 1).sum::<usize>();
    // The threads at work (and those waiting on you) are at the bottom and always
    // in view; the done ones above them scroll, as far as the thread marked needs.
    let busy = s.threads.iter().position(|a| a.status != Status::Stopped).unwrap_or(s.threads.len());
    let shown: Vec<usize> = if tall(busy, lines.len()) <= rows + 1 {
        let room = rows + 1 - tall(busy, lines.len());
        let mut top = 0;
        while at < busy && top < at && tall(top, at + 1) > room {
            top += 1;
        }
        let mut done = vec![];
        let mut used = 0;
        for i in top..busy {
            used += lines[i].len() + 1;
            if used > room {
                break;
            }
            done.push(i);
        }
        done.into_iter().chain(busy..lines.len()).collect()
    } else {
        // More at work than fits: the whole list scrolls, keeping its end in view.
        let mut top = 0;
        while top < lines.len() && tall(top, lines.len()) > rows + 1 {
            top += 1;
        }
        (top.min(at)..lines.len()).collect()
    };
    let mut y = area.y + 2;
    for i in shown {
        let a = &s.threads[i];
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
        for (k, (part, style)) in lines[i].iter().enumerate() {
            let mut o = Out::default();
            let edge = if on { "▌" } else if reading { "▎" } else { " " };
            o.put(edge, bg.fg(if on { AMBER } else { TEAL }));
            o.put(if k == 0 { glyph } else { " " }, bg.fg(color));
            o.put(" ", bg);
            let style = if k == 0 && reading { style.fg(TEAL) } else { *style };
            o.put(part, bg.patch(style));
            if k == 0 {
                let when = ago(now - a.at);
                pad(&mut o, w.saturating_sub(width(&when) + 1), bg);
                o.put(&when, bg.fg(FAINT));
            }
            pad(&mut o, w, bg);
            f.render_widget(o.line(), Rect::new(area.x, y, w as u16, 1));
            y += 1;
        }
        y += 1;
    }
}

/// A thread's lines in the list: its title on up to two, how it stands, then up to two
/// of what was last said.
fn entry(a: &Agent, s: &Side, w: usize) -> Vec<(String, Style)> {
    let title = fg(TEXT).add_modifier(Modifier::BOLD);
    let mut out: Vec<(String, Style)> = two_lines(&a.name, w).into_iter().map(|l| (l, title)).collect();
    let word = match a.status {
        Status::Working => "working",
        Status::Blocked => "waiting on you",
        Status::Stopped => "done",
    };
    let mut stands = word.to_string();
    if let Some(mr) = s.mrs.get(&a.address) {
        stands.push_str(&format!(" · {mr}"));
    }
    out.push((cut(&stands, w), fg(DIM)));
    if let Some((you, text)) = s.said.get(&a.address) {
        let said = format!("{}{text}", if *you { "you: " } else { "" });
        out.extend(two_lines(&said, w + 2).into_iter().map(|l| (l, fg(FAINT))));
    }
    out
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


