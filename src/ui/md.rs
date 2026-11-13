//! Markdown as Claude writes it, drawn for a terminal: emphasis, code, lists, quotes,
//! headings and tables, wrapped to a width with list items hanging under themselves.
use super::{width, wrap, AMBER, BAND, DIM, FAINT, TEAL};
use ratatui::style::{Modifier, Style};

/// One drawn line: its pieces, each with a style, starting at the text's column.
pub type Row = Vec<(String, Style)>;

/// `text` as rows no wider than `w`, on `base` (TEXT, or with a background).
pub fn render(text: &str, w: usize, base: Style) -> Vec<Row> {
    let w = w.max(8);
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<Row> = vec![];
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        if let Some(lang) = trimmed.strip_prefix("```") {
            let mut code = vec![];
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                code.push(lines[i]);
                i += 1;
            }
            i += 1;
            out.extend(code_rows(&code, lang.trim(), w, base));
            continue;
        }
        if trimmed.starts_with('|') {
            let start = i;
            while i < lines.len() && lines[i].trim_start().starts_with('|') {
                i += 1;
            }
            out.extend(table_rows(&lines[start..i], w, base));
            continue;
        }
        i += 1;
        if trimmed.is_empty() {
            if out.last().is_some_and(|r| !r.is_empty()) {
                out.push(vec![]);
            }
            continue;
        }
        if is_rule(trimmed) {
            out.push(vec![("─".repeat(w.min(40)), base.fg(FAINT))]);
            continue;
        }
        if let Some((level, title)) = heading(trimmed) {
            let style = base.fg(if level <= 2 { AMBER } else { TEAL }).add_modifier(Modifier::BOLD);
            out.extend(flow(inline(title, style), w, 0, &[]));
            continue;
        }
        if let Some(quoted) = trimmed.strip_prefix('>') {
            let bar = [("▎ ".to_string(), base.fg(FAINT))];
            out.extend(flow(inline(quoted.trim_start(), base.fg(DIM).add_modifier(Modifier::ITALIC)), w, 2, &bar).into_iter().map(|mut r| {
                if !r.first().is_some_and(|(s, _)| s == "▎ ") {
                    r.insert(0, bar[0].clone());
                }
                r
            }));
            continue;
        }
        let indent = width(&line[..line.len() - trimmed.len()]).min(12);
        if let Some((marker, rest)) = bullet(trimmed) {
            let lead = format!("{}{marker} ", " ".repeat(indent));
            let hang = width(&lead);
            let first = [(lead, base.fg(if marker == "•" { TEAL } else { DIM }))];
            out.extend(flow(inline(rest, base), w, hang, &first));
            continue;
        }
        out.extend(flow(inline(trimmed, base), w, 0, &[]));
    }
    while out.last().is_some_and(|r| r.is_empty()) {
        out.pop();
    }
    out
}

fn is_rule(s: &str) -> bool {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    s.len() >= 3 && (s.chars().all(|c| c == '-') || s.chars().all(|c| c == '*') || s.chars().all(|c| c == '_'))
}

fn heading(s: &str) -> Option<(usize, &str)> {
    let level = s.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&level).then(|| s[level..].strip_prefix(' ').map(|t| (level, t.trim())))?
}

/// A list item's marker as drawn ("•" for -, * and +; "3." kept) and its text.
fn bullet(s: &str) -> Option<(String, &str)> {
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(m) {
            return Some(("•".into(), rest));
        }
    }
    let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
    let rest = &s[digits..];
    if (1..=3).contains(&digits) && (rest.starts_with(". ") || rest.starts_with(") ")) {
        return Some((format!("{}.", &s[..digits]), &rest[2..]));
    }
    None
}

/// A fenced block: a bar down its side and the code dimmed, wrapped where it must.
fn code_rows(code: &[&str], lang: &str, w: usize, base: Style) -> Vec<Row> {
    let bar = ("│ ".to_string(), base.fg(FAINT));
    let mut out = vec![];
    if !lang.is_empty() {
        out.push(vec![("╭ ".to_string(), base.fg(FAINT)), (lang.to_string(), base.fg(FAINT).add_modifier(Modifier::ITALIC))]);
    }
    for line in code {
        let line = line.replace('\t', "    ");
        if line.trim().is_empty() {
            out.push(vec![bar.clone()]);
            continue;
        }
        let lead = line.len() - line.trim_start().len();
        for part in wrap(&line, w.saturating_sub(2 + lead).max(8)) {
            out.push(vec![bar.clone(), (" ".repeat(lead), base), (part, base.fg(DIM))]);
        }
    }
    out
}

/// A table with its columns lined up, or each row on its own when it can't fit.
fn table_rows(lines: &[&str], w: usize, base: Style) -> Vec<Row> {
    let cells = |l: &str| -> Vec<String> {
        let l = l.trim().trim_start_matches('|').trim_end_matches('|');
        l.split('|').map(|c| c.trim().to_string()).collect()
    };
    let is_sep = |row: &[String]| row.iter().all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')));
    let rows: Vec<Vec<String>> = lines.iter().map(|l| cells(l)).filter(|r| !is_sep(r)).collect();
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    let drawn: Vec<Vec<Row>> = rows.iter().map(|r| r.iter().map(|c| inline(c, base)).collect()).collect();
    let cell_w = |c: &Row| c.iter().map(|(s, _)| width(s)).sum::<usize>();
    let mut widths = vec![0; n];
    for r in &drawn {
        for (j, c) in r.iter().enumerate() {
            widths[j] = widths[j].max(cell_w(c));
        }
    }
    let total = widths.iter().sum::<usize>() + 3 * n.saturating_sub(1);
    let sep = (" │ ".to_string(), base.fg(FAINT));
    if total > w || n == 0 {
        return records(&rows, w, base);
    }
    let mut out = vec![];
    for (i, r) in drawn.iter().enumerate() {
        let mut row: Row = vec![];
        for j in 0..n {
            if j > 0 {
                row.push(sep.clone());
            }
            let c = r.get(j).cloned().unwrap_or_default();
            let pad = widths[j] - cell_w(&c);
            for (s, st) in c {
                row.push((s, if i == 0 { st.add_modifier(Modifier::BOLD) } else { st }));
            }
            row.push((" ".repeat(pad), base));
        }
        out.push(row);
        if i == 0 && drawn.len() > 1 {
            let rule: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
            out.push(vec![(rule.join("─┼─"), base.fg(FAINT))]);
        }
    }
    out
}

/// A table too wide to line up, each row a record: "• id: 1", its other cells under it.
fn records(rows: &[Vec<String>], w: usize, base: Style) -> Vec<Row> {
    let Some((head, body)) = rows.split_first() else { return vec![] };
    if body.is_empty() {
        return flow(inline(&head.join("  ·  "), base), w, 0, &[]);
    }
    let mut out = vec![];
    for r in body {
        for (j, cell) in r.iter().enumerate() {
            let lead = if j == 0 { "• " } else { "  " };
            let name = head.get(j).filter(|h| !h.is_empty()).map(|h| format!("{h}: ")).unwrap_or_default();
            let first = [(lead.to_string(), base.fg(TEAL)), (name, base.fg(DIM))];
            out.extend(flow(inline(cell, base), w, 4, &first));
        }
    }
    out
}

/// Inline markdown in `s`: `code`, **bold**, *italic* and [links](url), on `base`.
pub fn inline(s: &str, base: Style) -> Row {
    let cs: Vec<char> = s.chars().collect();
    let (mut out, mut buf): (Row, String) = (vec![], String::new());
    let (mut bold, mut italic) = (false, false);
    let style = |bold: bool, italic: bool| {
        let mut st = base;
        if bold {
            st = st.add_modifier(Modifier::BOLD);
        }
        if italic {
            st = st.add_modifier(Modifier::ITALIC);
        }
        st
    };
    let ahead = |from: usize, pat: &[char]| (from..cs.len()).find(|&k| cs[k..].starts_with(pat));
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == '`' {
            if let Some(end) = ahead(i + 1, &['`']) {
                flush(&mut out, &mut buf, style(bold, italic));
                let code: String = cs[i + 1..end].iter().collect();
                out.push((code, style(bold, italic).fg(AMBER)));
                i = end + 1;
                continue;
            }
        }
        if c == '*' && cs.get(i + 1) == Some(&'*') && (bold || ahead(i + 2, &['*', '*']).is_some()) {
            flush(&mut out, &mut buf, style(bold, italic));
            bold = !bold;
            i += 2;
            continue;
        }
        let opens = cs.get(i + 1).is_some_and(|n| !n.is_whitespace()) && ahead(i + 1, &['*']).is_some();
        if c == '*' && (italic || opens) {
            flush(&mut out, &mut buf, style(bold, italic));
            italic = !italic;
            i += 1;
            continue;
        }
        let image = c == '!' && cs.get(i + 1) == Some(&'[');
        if c == '[' || image {
            let open = if image { i + 1 } else { i };
            if let Some(mid) = ahead(open + 1, &[']', '(']) {
                if let Some(end) = ahead(mid + 2, &[')']) {
                    flush(&mut out, &mut buf, style(bold, italic));
                    let text: String = cs[open + 1..mid].iter().collect();
                    if image {
                        out.push((format!("▣ {text}"), style(bold, italic).fg(TEAL)));
                    } else {
                        out.push((text, style(bold, italic).fg(TEAL).add_modifier(Modifier::UNDERLINED)));
                    }
                    i = end + 1;
                    continue;
                }
            }
        }
        buf.push(c);
        i += 1;
    }
    flush(&mut out, &mut buf, style(bold, italic));
    out
}

fn flush(out: &mut Row, buf: &mut String, style: Style) {
    if !buf.is_empty() {
        out.push((std::mem::take(buf), style));
    }
}

/// Styled pieces wrapped to `w`: `first` leads the first row, and the rows after
/// it start `hang` columns in, under the text.
pub fn flow(pieces: Row, w: usize, hang: usize, first: &[(String, Style)]) -> Vec<Row> {
    let mut words: Vec<(String, Style, bool)> = vec![];
    for (text, style) in pieces {
        let mut word = String::new();
        for ch in text.chars() {
            if ch == ' ' {
                if !word.is_empty() {
                    words.push((std::mem::take(&mut word), style, false));
                }
                words.push((" ".into(), style, true));
            } else {
                word.push(ch);
            }
        }
        if !word.is_empty() {
            words.push((word, style, false));
        }
    }
    let mut rows: Vec<Row> = vec![first.to_vec()];
    let mut at = first.iter().map(|(s, _)| width(s)).sum::<usize>();
    let start = |rows: &mut Vec<Row>, at: &mut usize| {
        while rows.last().is_some_and(|r| r.last().is_some_and(|(s, _)| s == " ")) {
            rows.last_mut().unwrap().pop();
        }
        rows.push(if hang > 0 { vec![(" ".repeat(hang), Style::new())] } else { vec![] });
        *at = hang;
    };
    for (word, style, space) in words {
        let ww = width(&word);
        if space {
            if at > hang && at < w {
                rows.last_mut().unwrap().push((word, style));
                at += 1;
            }
            continue;
        }
        if at + ww > w && at > hang {
            start(&mut rows, &mut at);
        }
        if ww > w.saturating_sub(at) {
            let parts = wrap(&word, w.saturating_sub(hang).max(1));
            let last = parts.len() - 1;
            for (k, p) in parts.into_iter().enumerate() {
                let pw = width(&p);
                rows.last_mut().unwrap().push((p, style));
                at += pw;
                if k < last {
                    start(&mut rows, &mut at);
                }
            }
            continue;
        }
        rows.last_mut().unwrap().push((word, style));
        at += ww;
    }
    while rows.last().is_some_and(|r| r.last().is_some_and(|(s, _)| s == " ")) {
        rows.last_mut().unwrap().pop();
    }
    rows
}

/// The rows as plain text, for tests.
#[cfg(test)]
fn plain(rows: &[Row]) -> Vec<String> {
    rows.iter().map(|r| r.iter().map(|(s, _)| s.as_str()).collect::<String>().trim_end().to_string()).collect()
}

/// A background band behind `rows`, padded out to `w`, for a message of yours.
pub fn banded(rows: Vec<Row>, w: usize) -> Vec<Row> {
    rows.into_iter()
        .map(|mut r| {
            let used: usize = r.iter().map(|(s, _)| width(s)).sum();
            for (_, st) in r.iter_mut() {
                *st = st.bg(BAND);
            }
            r.push((" ".repeat(w.saturating_sub(used)), Style::new().bg(BAND)));
            r
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::TEXT;

    fn text(md: &str, w: usize) -> Vec<String> {
        plain(&render(md, w, Style::new().fg(TEXT)))
    }

    #[test]
    fn emphasis_and_code_lose_their_marks() {
        assert_eq!(text("a **bold** and `code` and *it* [link](https://x.y)", 80), ["a bold and code and it link"]);
        assert_eq!(text("snake_case and 2 * 3 stay", 80), ["snake_case and 2 * 3 stay"]);
    }

    #[test]
    fn an_inline_image_reads_as_its_caption() {
        assert_eq!(text("see ![Part A](/tmp/a.mp4) here", 80), ["see ▣ Part A here"]);
    }

    #[test]
    fn list_items_hang_under_their_text() {
        assert_eq!(text("- one two three four five", 12), ["• one two", "  three four", "  five"]);
        assert_eq!(text("12. first", 20), ["12. first"]);
    }

    #[test]
    fn tables_line_up_and_drop_their_separator_row() {
        assert_eq!(
            text("| id | type |\n|---|---|\n| 1 | gmail |", 40),
            ["id │ type", "───┼──────", "1  │ gmail"]
        );
    }

    #[test]
    fn a_table_too_wide_reads_row_by_row() {
        assert_eq!(text("| aaaaaaaa | bbbbbbbb |\n|---|---|\n| c | d |", 18), ["• aaaaaaaa: c", "  bbbbbbbb: d"]);
    }

    #[test]
    fn code_blocks_keep_their_lines_under_a_bar() {
        assert_eq!(text("```sql\nSELECT 1\n  FROM t\n```", 40), ["╭ sql", "│ SELECT 1", "│   FROM t"]);
    }

    #[test]
    fn headings_and_quotes_drop_their_marks() {
        assert_eq!(text("## Plan\n> said before", 40), ["Plan", "▎ said before"]);
    }

    #[test]
    fn nothing_is_lost_when_a_word_is_wider_than_the_line() {
        assert_eq!(text("x https://example.com/a/very/long/path", 12).concat().replace(' ', ""), "xhttps://example.com/a/very/long/path");
    }
}
