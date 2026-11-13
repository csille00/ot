//! Drawing: the palette, and the line-building both screens share.
pub mod board;
pub mod finder;
pub mod md;
pub mod music;
pub mod session;
pub mod side;

use crate::model::Tone;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

// Gruvbox dark, on the terminal's own background.
pub const TEXT: Color = Color::Rgb(0xeb, 0xdb, 0xb2);
pub const DIM: Color = Color::Rgb(0xa8, 0x99, 0x84);
pub const FAINT: Color = Color::Rgb(0x66, 0x5c, 0x54);
pub const AMBER: Color = Color::Rgb(0xfa, 0xbd, 0x2f);
pub const ORANGE: Color = Color::Rgb(0xfe, 0x80, 0x19);
pub const RED: Color = Color::Rgb(0xfb, 0x49, 0x34);
pub const GREEN: Color = Color::Rgb(0xb8, 0xbb, 0x26);
pub const TEAL: Color = Color::Rgb(0x83, 0xa5, 0x98);
pub const PINK: Color = Color::Rgb(0xd3, 0x86, 0x9b);
pub const BAND: Color = Color::Rgb(0x32, 0x30, 0x2f);

pub fn tone(t: Tone) -> Color {
    match t {
        Tone::Dim => DIM,
        Tone::Faint => FAINT,
        Tone::Amber => AMBER,
        Tone::Orange => ORANGE,
        Tone::Red => RED,
        Tone::Green => GREEN,
        Tone::Pink => PINK,
        Tone::Teal => TEAL,
    }
}

pub fn fg(c: Color) -> Style {
    Style::new().fg(c)
}

pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Text in lines no wider than `w`, never cut: it breaks after a space or
/// one of - / _ . and between the words of a camelCase name where it can,
/// and inside a word only when it must.
pub fn wrap(text: &str, w: usize) -> Vec<String> {
    let w = w.max(1);
    let mut lines = vec![];
    let mut rest = text.trim();
    while width(rest) > w {
        let mut fit = 0;
        // A space is the best place to break; inside a name, after - / _ .
        // or between camelCase words, only when the line has no space.
        let (mut space, mut name) = (None, None);
        let mut last = ' ';
        for (i, c) in rest.char_indices() {
            let end = i + c.len_utf8();
            if width(&rest[..end]) > w {
                break;
            }
            if c.is_uppercase() && (last.is_lowercase() || last.is_ascii_digit()) && i > 0 {
                name = Some(i);
            }
            fit = end;
            match c {
                ' ' => space = Some(end),
                '-' | '/' | '_' | '.' => name = Some(end),
                _ => {}
            }
            last = c;
        }
        let at = space
            .or(name)
            .unwrap_or(fit.max(rest.chars().next().map_or(1, char::len_utf8)));
        lines.push(rest[..at].trim_end().to_string());
        rest = rest[at..].trim_start();
    }
    lines.push(rest.to_string());
    lines
}

/// Facts in lines no wider than `w`, "  ·  " between two on a line; a fact
/// only breaks when it is wider than a line by itself.
pub fn pack(facts: &[Span<'static>], w: usize) -> Vec<Line<'static>> {
    const SEP: &str = "  ·  ";
    let mut lines: Vec<Out> = vec![];
    for f in facts {
        let fw = width(&f.content);
        match lines.last_mut() {
            Some(l) if l.w + width(SEP) + fw <= w => {
                l.put(SEP, fg(DIM));
                l.put(&f.content, f.style);
            }
            _ => {
                for piece in wrap(&f.content, w) {
                    let mut l = Out::default();
                    l.put(&piece, f.style);
                    lines.push(l);
                }
            }
        }
    }
    lines.into_iter().map(Out::line).collect()
}

/// One line onto the frame at row `y`, which moves on; nothing past its
/// bottom.
pub fn put(f: &mut Frame, area: Rect, y: &mut usize, l: Line<'static>) {
    if *y < area.height as usize {
        f.render_widget(l, Rect::new(area.x, area.y + *y as u16, area.width, 1));
    }
    *y += 1;
}

/// A question over everything on the screen, in a box near the top where
/// you can't miss it, until a key meant for it answers. Every input ot
/// takes is asked here, never in a footer. The box is one width and stays
/// where it is whatever it holds, so nothing moves while you type; `lines`
/// is told how wide a line may be. No lines draws nothing.
pub fn ask(f: &mut Frame, lines: impl FnOnce(usize) -> Vec<Line<'static>>) {
    let area = f.area();
    // Four columns of the screen show on each side, past a reading width.
    let w = area.width.saturating_sub(8).min(80);
    // The border and a space inside it on each side.
    let lines = lines(w.saturating_sub(4) as usize);
    if lines.is_empty() {
        return;
    }
    let h = (lines.len() as u16 + 2).min(area.height);
    let x = area.x + (area.width - w) / 2;
    // A box that grows (what you type wraps) grows down, its top still.
    let y = area.y + (area.height / 4).min(area.height - h);
    let at = Rect::new(x, y, w, h);
    f.render_widget(Clear, at);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(fg(AMBER))
        .padding(Padding::horizontal(1));
    f.render_widget(Paragraph::new(lines).block(block), at);
}

/// What you are typing, in lines no wider than `w`, the cursor after it.
pub fn typed(text: &str, w: usize) -> Vec<Line<'static>> {
    let mut parts = wrap(text, w.saturating_sub(1));
    // wrap trims, but a space just typed should move the cursor.
    let last = parts.len() - 1;
    if !parts[last].is_empty() {
        parts[last].push_str(&text[text.trim_end().len()..]);
    }
    parts
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            let mut spans = vec![Span::styled(p, fg(TEXT))];
            if i == last {
                spans.push(Span::styled("▏", fg(TEAL)));
            }
            Line::from(spans)
        })
        .collect()
}

/// The keys that answer a question, each with what it does.
pub fn keys(ks: &[(&str, &str)]) -> Line<'static> {
    Line::from(
        ks.iter()
            .flat_map(|(k, does)| {
                [
                    Span::styled(k.to_string(), fg(TEXT)),
                    Span::styled(does.to_string(), fg(DIM)),
                ]
            })
            .collect::<Vec<_>>(),
    )
}

/// What a test terminal shows, a line each, trailing spaces off.
#[cfg(test)]
pub fn text(t: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
    let buf = t.backend().buffer();
    let a = buf.area;
    (0..a.height)
        .map(|y| {
            (0..a.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The keys that work in `at`, with their word each, as many as fit.
pub fn footer(at: &[crate::keys::Place], right: usize) -> Line<'static> {
    let mut o = Out::default();
    o.put("  ", Style::new());
    for k in crate::keys::footer(at) {
        if o.w + width(k.key) + 1 + width(k.word) + 2 > right {
            break;
        }
        o.put(k.key, fg(TEXT));
        o.put(&format!(" {}  ", k.word), fg(DIM));
    }
    o.line()
}

/// A line built left to right, knowing how wide it is so far.
#[derive(Default)]
pub struct Out {
    spans: Vec<Span<'static>>,
    pub w: usize,
}

impl Out {
    pub fn put(&mut self, s: &str, style: Style) {
        self.w += width(s);
        self.spans.push(Span::styled(s.to_string(), style));
    }

    /// Spaces up to column `col`; nothing if already there.
    pub fn pad_to(&mut self, col: usize) {
        if col > self.w {
            self.put(&" ".repeat(col - self.w), Style::new());
        }
    }

    pub fn line(self) -> Line<'static> {
        Line::from(self.spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_breaks_after_dashes_and_never_drops_text() {
        assert_eq!(
            wrap("placeholder-links-calendar-entry", 14),
            ["placeholder-", "links-", "calendar-entry"]
        );
        assert_eq!(
            wrap("Catch references that belong somewhere else", 20),
            ["Catch references", "that belong", "somewhere else"]
        );
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("short", 40), ["short"]);
        assert_eq!(
            wrap("draftGuidelineFixChanges", 20),
            ["draftGuidelineFix", "Changes"]
        );
        assert_eq!(
            wrap("GUIDELINE_REVIEW_CHECKS", 18),
            ["GUIDELINE_REVIEW_", "CHECKS"]
        );
        assert_eq!(wrap("Move GitLab client", 10), ["Move", "GitLab", "client"]);
    }

    #[test]
    fn a_box_is_one_size_whatever_it_holds() {
        let corners = |lines: Vec<Line<'static>>| {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
            t.draw(|f| ask(f, |_| lines)).unwrap();
            let buf = t.backend().buffer().clone();
            let find = |c: &str| buf.content.iter().position(|cell| cell.symbol() == c);
            (find("╭"), find("╮"))
        };
        assert_eq!(
            corners(vec![Line::raw("cc on !102:"), Line::raw("k")]),
            corners(vec![Line::raw("cc on !102:"), Line::raw("maggie@acme.com alex@acme.com sam@acme.com")]),
        );
    }

    #[test]
    fn pack_keeps_each_fact_whole() {
        let facts: Vec<Span> = [
            "rebase it",
            "split from main 6 days ago",
            "stale-check failed",
        ]
        .map(Span::raw)
        .to_vec();
        let lines: Vec<String> = pack(&facts, 50).iter().map(|l| l.to_string()).collect();
        assert_eq!(
            lines,
            [
                "rebase it  ·  split from main 6 days ago",
                "stale-check failed"
            ]
        );
    }
}
