//! The thread finder over whatever is up, laid out as snacks.nvim's picker: the
//! query and its matches on the left, the thread under the cursor on the right.
use super::{fg, md, width, Out, AMBER, BAND, DIM, FAINT, GREEN, PINK, TEAL, TEXT};
use crate::finder::{Finder, Thread};
use crate::hc::Status;
use crate::model::ago;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use std::collections::HashMap;

/// Narrower than this, the preview goes and the list has the box.
const PREVIEW_FROM: u16 = 100;

pub fn draw(f: &mut Frame, finder: &mut Finder, status: &HashMap<String, Status>, now: i64) {
    let area = f.area();
    let w = if area.width >= 130 { area.width * 4 / 5 } else { area.width.saturating_sub(4) };
    let h = if area.height >= 30 { area.height * 4 / 5 } else { area.height.saturating_sub(2) };
    let at = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    f.render_widget(Clear, at);
    let (list, preview) = if w >= PREVIEW_FROM {
        let left = w / 2;
        (Rect { width: left, ..at }, Some(Rect { x: at.x + left, width: w - left, ..at }))
    } else {
        (at, None)
    };
    draw_list(f, list, finder, status, now);
    if let Some(p) = preview {
        draw_preview(f, p, finder, now);
    }
}

fn frame(title: &str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(fg(FAINT))
        .title(Line::styled(format!(" {title} "), fg(AMBER)).centered())
}

fn draw_list(f: &mut Frame, area: Rect, finder: &Finder, status: &HashMap<String, Status>, now: i64) {
    let block = frame("Threads");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 3 {
        return;
    }
    let w = inner.width as usize;
    let count = format!("{}/{}", finder.rows.len(), finder.threads.len());
    let mut o = Out::default();
    o.put(" ❯ ", fg(TEAL));
    let room = w.saturating_sub(width(&count) + 5);
    o.put(&tail(&finder.query, room), fg(TEXT));
    o.put("▏", fg(TEAL));
    o.pad_to(w.saturating_sub(width(&count) + 1));
    o.put(&count, fg(FAINT));
    f.render_widget(o.line(), Rect { height: 1, ..inner });
    f.render_widget(Line::styled("─".repeat(w), fg(FAINT)), Rect { y: inner.y + 1, height: 1, ..inner });
    let body = Rect { y: inner.y + 2, height: inner.height - 2, ..inner };
    if let Some(e) = &finder.error {
        f.render_widget(Paragraph::new(Line::styled(format!(" {e}"), fg(DIM))), body);
        return;
    }
    let rows = body.height as usize;
    let top = finder.at.saturating_sub(rows.saturating_sub(1));
    let terms: Vec<&str> = finder.query.split_whitespace().collect();
    for (k, row) in finder.rows.iter().enumerate().skip(top).take(rows) {
        let t = &finder.threads[row.thread];
        let on = k == finder.at;
        let bg = if on { Style::new().bg(BAND) } else { Style::new() };
        let (glyph, color) = glyph(t, status.get(&t.id).copied());
        let when = ago(now - t.updated);
        let mut o = Out::default();
        o.put(if on { "▌" } else { " " }, bg.fg(AMBER));
        o.put(glyph, bg.fg(color));
        o.put(" ", bg);
        let right = w.saturating_sub(width(&when) + 2);
        let title_room = if row.hit.is_some() { right.saturating_sub(3) * 3 / 5 } else { right.saturating_sub(3) };
        let base = if t.archived || t.settled { bg.fg(DIM) } else { bg.fg(TEXT) };
        put_marked(&mut o, &t.title, &row.marks, title_room, base, bg.fg(AMBER).add_modifier(Modifier::BOLD));
        if let Some(h) = &row.hit {
            o.put("  ", bg);
            let room = right.saturating_sub(o.w + 1);
            put_terms(&mut o, &cut(&h.snippet, room), &terms, bg.fg(FAINT), bg.fg(AMBER));
        }
        pad(&mut o, right + 1, bg);
        o.put(&when, bg.fg(FAINT));
        pad(&mut o, w, bg);
        f.render_widget(o.line(), Rect { y: body.y + (k - top) as u16, height: 1, ..body });
    }
}

fn glyph(t: &Thread, s: Option<Status>) -> (&'static str, ratatui::style::Color) {
    match s {
        Some(Status::Working) => ("●", GREEN),
        Some(Status::Blocked) => ("◐", AMBER),
        _ if t.archived => ("▪", FAINT),
        _ if t.settled => ("✓", FAINT),
        _ => ("○", DIM),
    }
}

fn draw_preview(f: &mut Frame, area: Rect, finder: &mut Finder, now: i64) {
    let Some(t) = finder.picked().cloned() else {
        f.render_widget(frame("Preview"), area);
        return;
    };
    let hit = finder.rows.get(finder.at).and_then(|r| r.hit.clone());
    let terms: Vec<String> = finder.query.split_whitespace().map(String::from).collect();
    let terms: Vec<&str> = terms.iter().map(String::as_str).collect();
    let block = frame(&cut(&t.title, area.width.saturating_sub(6) as usize));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width.saturating_sub(2) as usize;
    let mut facts = vec![];
    if !t.project.is_empty() {
        facts.push(t.project.clone());
    }
    if !t.branch.is_empty() {
        facts.push(format!("on {}", t.branch));
    }
    facts.push(if t.archived { "archived".into() } else if t.settled { "settled".into() } else { "open".into() });
    facts.push(ago(now - t.updated));
    let head = vec![
        Line::styled(format!(" {}", facts.join("  ·  ")), fg(DIM)),
        Line::styled(format!(" {}", "─".repeat(w)), fg(FAINT)),
    ];
    let room = (inner.height as usize).saturating_sub(head.len());
    let body = match hit {
        // The message that matched, from a little above where it first does.
        Some(h) => {
            let mut body = said_lines(h.you, &h.text, w, &terms);
            let first = body.iter().position(|l| l.spans.iter().any(|s| s.style.fg == Some(AMBER))).unwrap_or(0);
            body.drain(1..first.saturating_sub(2).max(1));
            body.truncate(room);
            body
        }
        // The newest at the bottom, as a conversation reads.
        None => {
            let mut body: Vec<Line<'static>> = vec![];
            for s in finder.preview() {
                if !body.is_empty() {
                    body.push(Line::default());
                }
                body.extend(said_lines(s.you, &s.text, w, &[]));
            }
            if body.is_empty() {
                body.push(Line::styled(" nothing said yet", fg(FAINT)));
            }
            let skip = body.len().saturating_sub(room);
            body.split_off(skip)
        }
    };
    f.render_widget(Paragraph::new(head.into_iter().chain(body).collect::<Vec<_>>()), inner);
}

/// A message under who said it, as markdown, each term in it lit.
fn said_lines(you: bool, text: &str, w: usize, terms: &[&str]) -> Vec<Line<'static>> {
    let (label, color) = if you { ("you", TEAL) } else { ("claude", PINK) };
    let mut lines = vec![Line::styled(format!(" {label}"), fg(color).add_modifier(Modifier::BOLD))];
    let lit = fg(AMBER).add_modifier(Modifier::BOLD);
    for row in md::render(text, w.saturating_sub(2), fg(TEXT)) {
        let mut o = Out::default();
        o.put("   ", Style::new());
        for (piece, style) in row {
            put_terms(&mut o, &piece, terms, style, lit);
        }
        lines.push(o.line());
    }
    lines
}

fn pad(o: &mut Out, col: usize, style: Style) {
    if col > o.w {
        o.put(&" ".repeat(col - o.w), style);
    }
}

/// `text` cut to `w` columns, the matched characters in `hit`.
fn put_marked(o: &mut Out, text: &str, marks: &[usize], w: usize, base: Style, hit: Style) {
    let mut used = 0;
    let n = text.chars().count();
    for (i, c) in text.chars().enumerate() {
        let cw = width(c.encode_utf8(&mut [0; 4]));
        if used + cw > w.saturating_sub(if i + 1 < n { 1 } else { 0 }) && i + 1 < n {
            o.put("…", base);
            return;
        }
        used += cw;
        o.put(c.encode_utf8(&mut [0; 4]), if marks.contains(&i) { hit } else { base });
    }
}

/// `text` with each place a term appears, any case, in `hit`.
fn put_terms(o: &mut Out, text: &str, terms: &[&str], base: Style, hit: Style) {
    let lower: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let chars: Vec<char> = text.chars().collect();
    if lower.len() != chars.len() || terms.is_empty() {
        o.put(text, base);
        return;
    }
    let mut on = vec![false; chars.len()];
    for t in terms {
        let t: Vec<char> = t.to_lowercase().chars().collect();
        if t.is_empty() || t.len() > lower.len() {
            continue;
        }
        for i in 0..=lower.len() - t.len() {
            if lower[i..i + t.len()] == t[..] {
                on[i..i + t.len()].iter_mut().for_each(|b| *b = true);
            }
        }
    }
    let mut run = String::new();
    let mut lit = false;
    for (c, on) in chars.iter().zip(on) {
        if on != lit && !run.is_empty() {
            o.put(&std::mem::take(&mut run), if lit { hit } else { base });
        }
        lit = on;
        run.push(*c);
    }
    if !run.is_empty() {
        o.put(&run, if lit { hit } else { base });
    }
}

/// `s` in `w` columns, an ellipsis where it was cut.
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

/// The end of `s` that fits in `w` columns, for a query longer than its line.
fn tail(s: &str, w: usize) -> String {
    let mut out: Vec<char> = vec![];
    let mut used = 0;
    for c in s.chars().rev() {
        let cw = width(c.encode_utf8(&mut [0; 4]));
        if used + cw > w {
            break;
        }
        used += cw;
        out.push(c);
    }
    out.into_iter().rev().collect()
}

