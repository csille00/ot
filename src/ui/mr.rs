//! An MR at a glance, and its diff, for the finder's preview and the panel beside a thread.
use super::{fg, wrap, Out, AMBER, DIM, FAINT, GREEN, RED, TEAL, TEXT};
use crate::model::{ago, size, Mr};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

/// The MR's title, branches and how it stands, in lines no wider than `w`.
pub fn info(m: &Mr, w: usize, now: i64) -> Vec<Line<'static>> {
    let mut out = vec![];
    for (i, part) in wrap(&format!("!{} {}", m.iid, m.title), w.saturating_sub(1)).into_iter().enumerate() {
        let mut o = Out::default();
        o.put(" ", Style::new());
        if i == 0 {
            let n = format!("!{}", m.iid);
            o.put(&n, fg(TEAL).add_modifier(Modifier::BOLD));
            o.put(&part[n.len()..], fg(TEXT).add_modifier(Modifier::BOLD));
        } else {
            o.put(&part, fg(TEXT).add_modifier(Modifier::BOLD));
        }
        out.push(o.line());
    }
    out.push(Line::styled(format!(" {} → {}", m.source, m.target), fg(DIM)));
    let mut o = Out::default();
    o.put(" ", Style::new());
    let fact = |o: &mut Out, text: &str, style: Style| {
        if o.w > 1 {
            o.put("  ·  ", fg(FAINT));
        }
        o.put(text, style);
    };
    fact(&mut o, if m.draft { "draft" } else { "ready" }, fg(if m.draft { DIM } else { TEXT }));
    let (pipe, color) = match m.pipeline.as_str() {
        "success" => ("pipeline passed", GREEN),
        "failed" => ("pipeline failed", RED),
        "" => ("no pipeline", FAINT),
        "running" | "pending" | "created" | "preparing" | "waiting_for_resource" | "scheduled" => ("pipeline running", AMBER),
        other => (other, DIM),
    };
    fact(&mut o, pipe, fg(color));
    if m.approved {
        fact(&mut o, "approved", fg(GREEN));
    }
    if m.conflicts {
        fact(&mut o, "conflicts", fg(RED));
    }
    if m.open_threads > 0 {
        fact(&mut o, &format!("{} open threads", m.open_threads), fg(AMBER));
    }
    out.push(o.line());
    let mut o = Out::default();
    o.put(" ", Style::new());
    o.put(&size(m.adds, m.dels), fg(DIM));
    o.put(&format!("  ·  {} files  ·  {}", m.files, ago(now - m.updated)), fg(DIM));
    out.push(o.line());
    if !m.failed_jobs.is_empty() {
        out.push(Line::styled(format!(" failed: {}", m.failed_jobs.join(", ")), fg(RED)));
    }
    if !m.reviewers.is_empty() {
        let who: Vec<String> = m.reviewers.iter().map(|r| if m.approved_by.contains(r) { format!("{r} ✓") } else { r.clone() }).collect();
        out.push(Line::styled(format!(" reviewers: {}", who.join(", ")), fg(DIM)));
    }
    out
}

/// A unified diff, coloured as git does, its lines cut to `w`.
pub fn diff(patch: &str, w: usize) -> Vec<Line<'static>> {
    let (stat, patch) = patch.split_once("\n\n").unwrap_or(("", patch));
    let mut out: Vec<Line<'static>> = stat.lines().filter_map(|l| file_line(l, w)).collect();
    if !out.is_empty() {
        out.push(Line::default());
    }
    out.extend(patch
        .lines()
        .map(|l| {
            let style = if l.starts_with("diff --git") {
                return Line::styled(format!(" {}", cut(l.trim_start_matches("diff --git a/").split(" b/").next().unwrap_or(l), w)), fg(AMBER).add_modifier(Modifier::BOLD));
            } else if l.starts_with("+++") || l.starts_with("---") || l.starts_with("index ") || l.starts_with("new file") || l.starts_with("deleted file") {
                return Line::default().style(fg(FAINT));
            } else if l.starts_with("@@") {
                fg(TEAL)
            } else if l.starts_with('+') {
                fg(GREEN)
            } else if l.starts_with('-') {
                fg(RED)
            } else if l.contains('|') && (l.ends_with('+') || l.ends_with('-') || l.contains("Bin ")) {
                fg(DIM)
            } else {
                fg(TEXT)
            };
            Line::styled(format!(" {}", cut(&l.replace('\t', "    "), w)), style)
        })
        .filter(|l| !(l.spans.is_empty() && l.style.fg == Some(FAINT))));
    out
}

/// A `--numstat` line as "+12 −3  src/main.rs", the path's start dropped if it must be.
fn file_line(l: &str, w: usize) -> Option<Line<'static>> {
    let mut it = l.splitn(3, '\t');
    let (adds, dels, path) = (it.next()?, it.next()?, it.next()?);
    let (adds, dels) = if adds == "-" { ("bin".to_string(), String::new()) } else { (format!("+{adds}"), format!("\u{2212}{dels}")) };
    let mut o = Out::default();
    o.put(" ", Style::new());
    o.put(&format!("{adds:>5}"), fg(GREEN));
    o.put(&format!(" {dels:<5} "), fg(RED));
    let room = w.saturating_sub(o.w);
    let path = if super::width(path) > room {
        let tail: String = path.chars().rev().take(room.saturating_sub(1)).collect::<Vec<_>>().into_iter().rev().collect();
        format!("…{tail}")
    } else {
        path.to_string()
    };
    o.put(&path, fg(TEXT));
    Some(o.line())
}

fn cut(s: &str, w: usize) -> String {
    if super::width(s) <= w {
        return s.to_string();
    }
    let mut out = String::new();
    for c in s.chars() {
        if super::width(&out) + super::width(c.encode_utf8(&mut [0; 4])) + 1 > w {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

/// The thread's MR down the right of the screen: how it stands, then its diff from `scroll`.
pub fn panel(f: &mut ratatui::Frame, area: ratatui::layout::Rect, m: &Mr, patch: Option<&Result<String, String>>, scroll: &mut usize, now: i64) {
    use ratatui::layout::Rect;
    for y in area.y..area.bottom() {
        f.render_widget(Line::styled("│", fg(FAINT)), Rect::new(area.x, y, 1, 1));
    }
    let inner = Rect { x: area.x + 1, width: area.width.saturating_sub(1), ..area };
    let w = inner.width.saturating_sub(2) as usize;
    let mut head = info(m, w, now);
    head.push(Line::styled(format!(" {}", "─".repeat(w)), fg(FAINT)));
    let body = match patch {
        Some(Ok(p)) => diff(p, w),
        Some(Err(e)) => vec![Line::styled(format!(" {e}"), fg(DIM))],
        None => vec![],
    };
    let room = (inner.height as usize).saturating_sub(head.len() + 1);
    *scroll = (*scroll).min(body.len().saturating_sub(room));
    let shown = body.len().min(*scroll + room);
    let mut lines = head;
    lines.extend(body[*scroll..shown].iter().cloned());
    f.render_widget(ratatui::widgets::Paragraph::new(lines), inner);
    if !body.is_empty() {
        let at = format!(" {}–{} of {}  ctrl-f/ctrl-b  o review  m hide ", *scroll + 1, shown, body.len());
        let y = inner.bottom().saturating_sub(1);
        f.render_widget(Line::styled(at, fg(FAINT)).right_aligned(), Rect { y, height: 1, ..inner });
    }
}

