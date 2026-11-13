//! An MR's title and description, read in a box over the board (l on its row).
use super::{fg, md, wrap, Out, AMBER, DIM, FAINT, TEAL, TEXT};
use crate::model::Mr;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};
use ratatui::Frame;

/// The description as read, or why not; None while GitLab is asked.
pub fn draw(f: &mut Frame, m: &Mr, text: Option<&Result<String, String>>, scroll: &mut usize) {
    let area = f.area();
    let w = (area.width.saturating_sub(8)).min(110);
    let h = (area.height * 4 / 5).max(10).min(area.height);
    let at = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    f.render_widget(Clear, at);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(fg(AMBER))
        .padding(Padding::horizontal(1))
        .title(Line::styled(format!(" !{} ", m.iid), fg(TEAL).add_modifier(Modifier::BOLD)));
    let inner = block.inner(at);
    f.render_widget(block, at);
    let tw = inner.width as usize;
    let mut head: Vec<Line<'static>> = wrap(&m.title, tw).into_iter().map(|l| Line::styled(l, fg(TEXT).add_modifier(Modifier::BOLD))).collect();
    let (merge, tone) = crate::model::merge_status(m);
    let mut o = Out::default();
    o.put(&format!("{}  ·  {} → {}  ·  ", m.author, m.source, m.target), fg(DIM));
    o.put(&merge, fg(super::tone(tone)));
    head.push(o.line());
    head.push(Line::styled("─".repeat(tw), fg(FAINT)));
    let body: Vec<Line<'static>> = match text {
        None => vec![Line::styled("reading its description…", fg(DIM))],
        Some(Err(e)) => vec![Line::styled(e.clone(), fg(DIM))],
        Some(Ok(t)) if t.trim().is_empty() => vec![Line::styled("(no description)", fg(FAINT))],
        Some(Ok(t)) => md::render(t, tw, fg(TEXT)).into_iter().map(|row| Line::from(row.into_iter().map(|(s, st)| ratatui::text::Span::styled(s, st)).collect::<Vec<_>>())).collect(),
    };
    let room = (inner.height as usize).saturating_sub(head.len() + 1);
    *scroll = (*scroll).min(body.len().saturating_sub(room));
    let shown = body.len().min(*scroll + room);
    let mut lines = head;
    lines.extend(body[*scroll..shown].iter().cloned());
    f.render_widget(Paragraph::new(lines), inner);
    let more = if body.len() > room { format!("{}–{} of {}  ", *scroll + 1, shown, body.len()) } else { String::new() };
    let keys = format!("{more}j/k scroll  w web  y link  esc closes");
    f.render_widget(Line::styled(keys, Style::new().fg(FAINT)).right_aligned(), Rect { y: inner.bottom().saturating_sub(1), height: 1, ..inner });
}
