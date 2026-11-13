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
    let (merge, tone) = crate::model::merge_status(m);
    out.push(Line::from(vec![
        ratatui::text::Span::styled(" merge: ", fg(DIM)),
        ratatui::text::Span::styled(merge, fg(super::tone(tone)).add_modifier(Modifier::BOLD)),
    ]));
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

/// The per-file counts, then the diff drawn by delta when it is here (syntax colours,
/// changed words lit, line numbers), else coloured as git does; lines cut to `w`.
pub fn diff(patch: &str, w: usize) -> Vec<Line<'static>> {
    let (stat, patch) = patch.split_once("\n\n").unwrap_or(("", patch));
    let mut out: Vec<Line<'static>> = stat.lines().filter_map(|l| file_line(l, w)).collect();
    if !out.is_empty() {
        out.push(Line::default());
    }
    if let Some(lines) = delta(patch, w) {
        out.extend(lines);
        return out;
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

/// `patch` as delta draws it `w` wide, read back from its colour codes; kept for the
/// last patch and width asked, since the panel draws every frame.
fn delta(patch: &str, w: usize) -> Option<Vec<Line<'static>>> {
    use std::hash::{Hash, Hasher};
    use std::sync::Mutex;
    static LAST: Mutex<Option<(u64, Option<Vec<Line<'static>>>)>> = Mutex::new(None);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (patch, w).hash(&mut h);
    let key = h.finish();
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, lines)) = last.as_ref().filter(|(k, _)| *k == key) {
        let _ = k;
        return lines.clone();
    }
    let lines = run_delta(patch, w);
    *last = Some((key, lines.clone()));
    lines
}

fn run_delta(patch: &str, w: usize) -> Option<Vec<Line<'static>>> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("delta")
        .args(["--no-gitconfig", "--paging=never", "--true-color=always", "--line-numbers", "--syntax-theme=gruvbox-dark"])
        .arg(format!("--width={w}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut input = child.stdin.take()?;
    let patch = patch.to_string();
    // Fed from its own thread, so a big diff can't fill both pipes.
    let feed = std::thread::spawn(move || input.write_all(patch.as_bytes()));
    let out = child.wait_with_output().ok()?;
    let _ = feed.join();
    let text = String::from_utf8_lossy(&out.stdout);
    out.status.success().then(|| text.trim_start_matches('\n').lines().map(|l| ansi_line(l, w)).collect())
}

/// One line of terminal colour codes as styled spans, cut to `w`; an erase to the
/// line's end carries its background on to there.
pub fn ansi_line(text: &str, w: usize) -> Line<'static> {
    use ratatui::style::Color;
    let mut o = Out::default();
    o.put(" ", Style::new());
    let mut style = Style::new();
    let mut run = String::new();
    let mut chars = text.chars().peekable();
    let mut fill = None;
    let room = w + 1;
    let flush = |o: &mut Out, run: &mut String, style: Style| {
        if run.is_empty() {
            return;
        }
        let left = room.saturating_sub(o.w);
        let piece = if super::width(run) > left { cut(run, left) } else { std::mem::take(run) };
        run.clear();
        if !piece.is_empty() && left > 0 {
            o.put(&piece, style);
        }
    };
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            if c == '\t' {
                run.push_str("    ");
            } else {
                run.push(c);
            }
            continue;
        }
        if chars.peek() != Some(&'[') {
            continue;
        }
        chars.next();
        let mut code = String::new();
        let mut end = ' ';
        for d in chars.by_ref() {
            if d.is_ascii_alphabetic() {
                end = d;
                break;
            }
            code.push(d);
        }
        flush(&mut o, &mut run, style);
        match end {
            'K' => fill = style.bg,
            'm' => {
                let nums: Vec<u16> = code.split(';').map(|n| n.parse().unwrap_or(0)).collect();
                let mut i = 0;
                while i < nums.len() {
                    let colour = |at: usize| match nums.get(at + 1) {
                        Some(5) => nums.get(at + 2).map(|n| (Color::Indexed(*n as u8), 3)),
                        Some(2) => match (nums.get(at + 2), nums.get(at + 3), nums.get(at + 4)) {
                            (Some(r), Some(g), Some(b)) => Some((Color::Rgb(*r as u8, *g as u8, *b as u8), 5)),
                            _ => None,
                        },
                        _ => None,
                    };
                    match nums[i] {
                        0 => style = Style::new(),
                        1 => style = style.add_modifier(Modifier::BOLD),
                        2 => style = style.add_modifier(Modifier::DIM),
                        3 => style = style.add_modifier(Modifier::ITALIC),
                        4 => style = style.add_modifier(Modifier::UNDERLINED),
                        22 => style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
                        23 => style = style.remove_modifier(Modifier::ITALIC),
                        24 => style = style.remove_modifier(Modifier::UNDERLINED),
                        n @ 30..=37 => style = style.fg(Color::Indexed((n - 30) as u8)),
                        n @ 90..=97 => style = style.fg(Color::Indexed((n - 90 + 8) as u8)),
                        n @ 40..=47 => style = style.bg(Color::Indexed((n - 40) as u8)),
                        n @ 100..=107 => style = style.bg(Color::Indexed((n - 100 + 8) as u8)),
                        39 => style.fg = None,
                        49 => style.bg = None,
                        38 | 48 => {
                            if let Some((c, used)) = colour(i) {
                                style = if nums[i] == 38 { style.fg(c) } else { style.bg(c) };
                                i += used - 1;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
            }
            _ => {}
        }
    }
    flush(&mut o, &mut run, style);
    if let Some(bg) = fill {
        let pad = room.saturating_sub(o.w);
        o.put(&" ".repeat(pad), Style::new().bg(bg));
    }
    o.line()
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


#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn colour_codes_become_styles_and_an_erase_fills_the_line() {
        let l = ansi_line("\x1b[38;2;251;73;52mgone\x1b[0m \x1b[48;5;22;1mnew\x1b[0K\x1b[0m", 12);
        let text: String = l.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(text, " gone new    ");
        assert_eq!(l.spans[1].style.fg, Some(Color::Rgb(251, 73, 52)));
        assert_eq!(l.spans[3].style.bg, Some(Color::Indexed(22)));
        assert!(l.spans[3].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(l.spans.last().unwrap().style.bg, Some(Color::Indexed(22)));
    }

    #[test]
    fn a_line_wider_than_the_panel_is_cut() {
        let l = ansi_line("\x1b[32mabcdefghijklmnop\x1b[0m", 8);
        let text: String = l.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(text, " abcdefg…");
    }
}
