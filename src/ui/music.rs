//! cliamp's bars as ot draws them: the hills, a bar every other column in
//! the rows nothing else uses, each rising with its band and throwing a
//! cap up off its top that falls back, and a strip of the same levels
//! along one line. The motion is cliamp's own ClassicPeak.
use super::{ask, fg, keys, put, typed, width, wrap, Out, AMBER, BAND, DIM, FAINT, GREEN, RED, TEAL, TEXT};
use crate::keys::Place;
use crate::music::{Hit, Playing};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Instant;

#[derive(Default)]
pub struct Hills {
    bars: Vec<Bar>,
    /// When the last frame came, for how far `tick` moves the bars.
    last: Option<Instant>,
}

#[derive(Clone, Default)]
struct Bar {
    level: f32,
    cap: f32,
    /// The cap's speed up, 0 while it rests on the bar or holds at its top.
    speed: f32,
    /// Seconds left holding at the top.
    hold: f32,
}

const RAMP: [&str; 8] = ["", "▁", "▂", "▃", "▄", "▅", "▆", "▇"];
const CAPS: [&str; 4] = ["⎺", "⎻", "⎼", "⎽"];
/// Deepest the hills go, in rows.
pub const DEEPEST: usize = 12;

impl Hills {
    /// A frame's bands moved into the bars of a screen `cols` wide, `dt`
    /// seconds after the last.
    pub fn step(&mut self, bands: &[f32], cols: usize, dt: f32) {
        let n = (cols + 1) / 2 + 1;
        self.bars.resize(n, Bar::default());
        for (i, b) in self.bars.iter_mut().enumerate() {
            b.step(spread(bands, i, n), dt);
        }
    }

    /// A frame's bands moved in, the time since the last frame measured
    /// here; a gap longer than a glance (the screen away) moves them as
    /// a quarter second would.
    pub fn tick(&mut self, bands: &[f32], cols: usize) {
        let dt = self
            .last
            .map_or(1.0 / 30.0, |t| t.elapsed().as_secs_f32().min(0.25));
        self.last = Some(Instant::now());
        self.step(bands, cols, dt);
    }

    /// Nothing to draw: every bar is down and every cap on it.
    pub fn quiet(&self) -> bool {
        self.bars.iter().all(|b| b.level < 0.01 && b.cap < 0.01)
    }

    /// `depth` rows of the bars, top first, `cols` wide, each with its
    /// color: its height's, so a bar keeps its colors as it moves.
    pub fn rows(&self, cols: usize, depth: usize) -> Vec<(String, Color)> {
        let n = depth;
        (0..n)
            .map(|row| {
                let from_bottom = n - 1 - row;
                let text: String = (0..cols)
                    .map(|col| match (col % 2, self.bars.get(col / 2)) {
                        (0, Some(b)) => b.cell(from_bottom, n),
                        _ => " ",
                    })
                    .collect();
                (text, tone(from_bottom as f32 / n as f32))
            })
            .collect()
    }

    pub fn lines(&self, cols: usize, depth: usize) -> Vec<Line<'static>> {
        self.rows(cols, depth)
            .into_iter()
            .map(|(text, c)| Line::styled(text, fg(c)))
            .collect()
    }

    /// The levels as one line of eighth blocks, `cols` wide.
    pub fn strip(&self, cols: usize) -> Vec<Span<'static>> {
        let levels: Vec<f32> = self.bars.iter().map(|b| b.level).collect();
        (0..cols)
            .map(|i| {
                let v = spread(&levels, i, cols);
                let l = ((v * 8.0 + 0.5) as usize).min(8);
                match l {
                    0 => Span::styled("▁", fg(FAINT)),
                    8 => Span::styled("█", fg(tone(v))),
                    l => Span::styled(RAMP[l], fg(tone(v))),
                }
            })
            .collect()
    }
}

impl Bar {
    fn step(&mut self, lv: f32, dt: f32) {
        let ease = if lv > self.level { 34.0 } else { 10.0 };
        self.level += (lv - self.level) * (1.0 - (-ease * dt).exp());
        if self.speed == 0.0 && self.cap <= self.level + 0.01 && lv > self.cap {
            self.speed = (0.8 + 1.4 * (lv - self.cap)).min(1.7);
            self.cap = lv;
            self.hold = 0.0;
        }
        if self.hold > 0.0 {
            self.hold -= dt;
            return;
        }
        let was_rising = self.speed > 0.0;
        self.cap = (self.cap + self.speed * dt).min(1.0);
        self.speed -= 9.5 * dt;
        if was_rising && self.speed <= 0.0 && self.cap > self.level + 0.01 {
            self.speed = 0.0;
            self.hold = 0.08;
        }
        if self.cap <= self.level {
            self.cap = self.level;
            self.speed = 0.0;
        }
    }

    /// The cell `from_bottom` rows up in hills `n` deep: a full or part
    /// block of the bar, else the cap when it is in this row, else nothing.
    fn cell(&self, from_bottom: usize, n: usize) -> &'static str {
        let f = ((self.level * n as f32 - from_bottom as f32) * 8.0) as i32;
        if f >= 8 {
            return "█";
        }
        if f >= 1 {
            return RAMP[f as usize];
        }
        if self.cap > self.level + (0.5 / (4 * n) as f32).max(0.01) {
            let dy = ((1.0 - self.cap.min(1.0)) * (4 * n - 1) as f32 + 0.5) as usize;
            if dy / 4 == n - 1 - from_bottom {
                return CAPS[dy % 4];
            }
        }
        " "
    }
}

/// Bar `i` of `n` from the bands: the band it falls on and the next,
/// blended with a cosine so the hills roll instead of step.
fn spread(bands: &[f32], i: usize, n: usize) -> f32 {
    if bands.len() < 2 || n < 2 {
        return bands.first().copied().unwrap_or(0.0);
    }
    let x = i as f32 * (bands.len() - 1) as f32 / (n - 1) as f32;
    let j = (x as usize).min(bands.len() - 2);
    let f = x - j as f32;
    let f = (1.0 - (f * std::f32::consts::PI).cos()) / 2.0;
    (bands[j] + (bands[j + 1] - bands[j]) * f).clamp(0.0, 1.0)
}

/// Green low, amber in the middle, red at the top, each pulled toward the
/// band's shade so the hills sit behind the text, not in front of it.
fn tone(height: f32) -> Color {
    let c = if height >= 0.6 {
        RED
    } else if height >= 0.3 {
        AMBER
    } else {
        GREEN
    };
    let (Color::Rgb(r, g, b), Color::Rgb(br, bg, bb)) = (c, BAND) else {
        return c;
    };
    let mix = |x: u8, y: u8| (y as f32 + (x as f32 - y as f32) * 0.7) as u8;
    Color::Rgb(mix(r, br), mix(g, bg), mix(b, bb))
}

/// The music screen's own state: a search, in whichever stage it is, and
/// what the last pick came to.
#[derive(Debug, Default)]
pub struct View {
    pub search: Search,
    /// Why a pick was refused, until the next key.
    pub said: Option<String>,
}

#[derive(Debug, Default)]
pub enum Search {
    #[default]
    Off,
    /// What you have typed so far.
    Typing(String),
    /// yt-dlp is out looking.
    Searching(String),
    /// What it found, and which one the cursor is on.
    Hits { hits: Vec<Hit>, at: usize },
}

impl View {
    pub fn step(&mut self, by: isize) {
        if let Search::Hits { hits, at } = &mut self.search {
            *at = at
                .saturating_add_signed(by)
                .min(hits.len().saturating_sub(1));
        }
    }

    /// The hit the cursor is on.
    pub fn picked(&self) -> Option<&Hit> {
        match &self.search {
            Search::Hits { hits, at } => hits.get(*at),
            _ => None,
        }
    }
}

/// The music screen: what plays, the hills across the middle, where the
/// track is and what comes next; a search's results in the hills' place
/// while you pick one. `aim` is where a seek is taking the track, shown
/// as the clock from the press until cliamp gets there; `loading` is the
/// pick cliamp is fetching, said until it plays.
pub fn draw(
    f: &mut Frame,
    p: Option<&Playing>,
    hills: &Hills,
    aim: Option<f64>,
    loading: Option<&str>,
    v: &View,
) {
    let area = f.area();
    let (w, h) = (area.width as usize, area.height as usize);
    let right = w.saturating_sub(1);
    let mut y = 0;
    if w < 30 || h < 12 {
        put(
            f,
            area,
            &mut y,
            Line::styled("  too small for the music", fg(DIM)),
        );
        return;
    }
    let mut top = vec![header(p, loading, right), Line::default()];
    match p {
        None => top.push(Line::styled("  cliamp is not running", fg(DIM))),
        Some(p) if p.title.is_empty() => top.push(Line::styled("  Nothing is playing.", fg(DIM))),
        Some(p) => {
            for l in wrap(&p.title, right.saturating_sub(4)).into_iter().take(2) {
                top.push(Line::styled(format!("  {l}"), fg(TEXT).bold()));
            }
            if !p.artist.is_empty() {
                top.push(Line::styled(format!("  {}", p.artist), fg(DIM)));
            }
        }
    }
    if let Some(t) = loading {
        top.push(Line::default());
        for l in wrap(&format!("loading {t}…"), right.saturating_sub(4))
            .into_iter()
            .take(2)
        {
            top.push(Line::styled(format!("  {l}"), fg(AMBER)));
        }
    }
    if let Some(said) = &v.said {
        top.push(Line::default());
        top.push(Line::styled(format!("  {said}"), fg(AMBER)));
    }
    top.push(Line::default());
    for l in top {
        put(f, area, &mut y, l);
    }
    // The hills from here down to the rows the clock and the keys keep,
    // or the search where it is.
    let depth = h.saturating_sub(4).saturating_sub(y);
    match &v.search {
        Search::Hits { hits, at } => {
            for l in results(hits, *at, depth, right) {
                put(f, area, &mut y, l);
            }
        }
        Search::Searching(q) => {
            let l = Line::styled(format!("  searching YouTube for {q}…"), fg(DIM));
            put(f, area, &mut y, l);
        }
        _ => {
            if !hills.quiet() {
                for l in hills.lines(w, depth) {
                    put(f, area, &mut y, l);
                }
            }
        }
    }
    y = h - 4;
    if let Some(p) = p.filter(|p| !p.title.is_empty()) {
        put(f, area, &mut y, Line::default());
        put(f, area, &mut y, progress(p, aim, right));
        let next = p.next.first().map(|n| format!("  next  {n}"));
        if let Some(n) = next.filter(|n| width(n) <= right) {
            put(f, area, &mut y, Line::styled(n, fg(DIM)));
        }
    }
    y = h - 1;
    let foot = match &v.search {
        // What you search for is asked in a box over the screen, with its
        // own keys; the footer says nothing that could be taken for them.
        Search::Typing(_) => Line::default(),
        Search::Hits { .. } => super::footer(&[Place::Hits], right),
        _ => super::footer(&[Place::Music], right),
    };
    put(f, area, &mut y, foot);
    if let Search::Typing(text) = &v.search {
        ask(f, |w| {
            let mut lines = vec![Line::styled("search YouTube for", fg(DIM))];
            lines.extend(typed(text, w));
            lines.push(Line::default());
            lines.push(keys(&[("enter", " searches  "), ("esc", " drops")]));
            lines
        });
    }
}

/// The hits as rows, the one at `at` banded and kept in the `depth` rows
/// there are: its length, its title wrapped under itself, its channel.
fn results(hits: &[Hit], at: usize, depth: usize, right: usize) -> Vec<Line<'static>> {
    const TEXT_COL: usize = 14;
    let mut out = vec![];
    let mut cur = 0..0;
    for (i, h) in hits.iter().enumerate() {
        let on = i == at;
        let start = out.len();
        let channel = format!("  {}", h.channel);
        let room = right.saturating_sub(TEXT_COL + width(&channel)).max(8);
        for (j, part) in wrap(&h.title, room).into_iter().enumerate() {
            let mut o = Out::default();
            o.put(if on { "  ▌ " } else { "    " }, fg(TEAL));
            if j == 0 {
                o.put(&format!("{:>8}", h.length), fg(DIM));
            }
            o.pad_to(TEXT_COL);
            o.put(&part, fg(TEXT));
            if j == 0 {
                o.put(&channel, fg(DIM));
            }
            let l = o.line();
            out.push(if on {
                l.style(Style::new().bg(BAND))
            } else {
                l
            });
        }
        if on {
            cur = start..out.len();
        }
    }
    // The first screen as it is; past it, the cursor's lines end the view.
    let scroll = cur.end.saturating_sub(depth).min(cur.start);
    out.into_iter().skip(scroll).take(depth).collect()
}

fn header(p: Option<&Playing>, loading: Option<&str>, right: usize) -> Line<'static> {
    let mut o = Out::default();
    o.put("  ♪ music", fg(TEAL).bold());
    let mut facts: Vec<(String, Color)> = vec![];
    // Loud when you'd wonder why there is no sound.
    match p {
        _ if loading.is_some() => facts.push(("loading".into(), AMBER)),
        Some(p) if p.buffering => facts.push(("buffering".into(), AMBER)),
        Some(p) if p.playing() => facts.push(("playing".into(), GREEN)),
        Some(p) if p.state == "paused" => facts.push(("paused".into(), AMBER)),
        Some(p) if !p.title.is_empty() => facts.push((p.state.clone(), DIM)),
        _ => facts.push(("nothing playing".into(), DIM)),
    }
    if let Some(p) = p.filter(|p| !p.title.is_empty()) {
        if p.total > 1 && p.index >= 0 {
            facts.push((format!("{} of {}", p.index + 1, p.total), DIM));
        }
        facts.push((format!("{:+.0} dB", p.volume), DIM));
        if p.shuffle {
            facts.push(("shuffle".into(), DIM));
        }
    }
    let wide: usize = facts.iter().map(|(t, _)| width(t) + 5).sum::<usize>() - 5;
    o.pad_to(right.saturating_sub(wide).max(o.w + 2));
    for (i, (t, c)) in facts.iter().enumerate() {
        if i > 0 {
            o.put("  ·  ", fg(FAINT));
        }
        o.put(t, fg(*c));
    }
    o.line()
}

/// Where the track is: the clock, a bar across the rest, its length. A
/// seek on its way shows as the clock it is heading for, with an arrow;
/// a paused track's bar goes dim.
fn progress(p: &Playing, aim: Option<f64>, right: usize) -> Line<'static> {
    let at = aim.unwrap_or(p.position).clamp(0.0, p.duration.max(0.0));
    let done = match aim {
        Some(_) => format!("→ {}", clock(at)),
        None => clock(at),
    };
    let total = clock(p.duration);
    let bar = right.saturating_sub(8 + width(&done) + width(&total));
    let filled = if p.duration > 0.0 {
        ((at / p.duration) * bar as f64).round() as usize
    } else {
        0
    };
    let mut o = Out::default();
    o.put("  ", Style::new());
    o.put(&done, fg(if aim.is_some() { AMBER } else { DIM }));
    o.put("  ", Style::new());
    o.put(&"━".repeat(filled.min(bar)), fg(if p.playing() { AMBER } else { DIM }));
    o.put(&"─".repeat(bar.saturating_sub(filled)), fg(FAINT));
    o.put("  ", Style::new());
    o.put(&total, fg(DIM));
    o.line()
}

/// Seconds as m:ss, or h:mm:ss past an hour.
fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (h, m, s) = (s / 3600, s % 3600 / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_rise_to_their_bands_and_caps_fly_off_them() {
        let mut h = Hills::default();
        let bands = [0.9, 0.5, 0.1];
        for _ in 0..30 {
            h.step(&bands, 10, 1.0 / 30.0);
        }
        assert_eq!(h.bars.len(), 6);
        assert!(h.bars[0].level > 0.85 && h.bars[5].level < 0.15);
        assert!(!h.quiet());
        for _ in 0..90 {
            h.step(&[0.0; 3], 10, 1.0 / 30.0);
        }
        assert!(h.quiet(), "everything settles once the music stops");
    }

    #[test]
    fn the_clock_reads_like_a_players() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(91.7), "1:31");
        assert_eq!(clock(3600.0), "1:00:00");
    }

    #[test]
    fn the_music_screen_at_60x14() {
        let p = Playing {
            buffering: false,
            state: "playing".into(),
            title: "A long set that goes on and on, the kind that fills an afternoon".into(),
            artist: "Someone".into(),
            position: 91.0,
            duration: 3600.0,
            index: 1,
            total: 15,
            volume: -4.0,
            shuffle: true,
            next: vec!["The one after".into()],
        };
        let mut h = Hills::default();
        for _ in 0..12 {
            h.step(&[1.0, 0.6, 0.2, 0.0], 60, 1.0 / 30.0);
        }
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 14)).unwrap();
        let v = View::default();
        t.draw(|f| draw(f, Some(&p), &h, Some(p.position + 10.0), None, &v)).unwrap();
        insta::assert_snapshot!(crate::ui::text(&t));
        t.draw(|f| draw(f, None, &Hills::default(), None, None, &v))
            .unwrap();
        assert!(crate::ui::text(&t).contains("cliamp is not running"));
        t.draw(|f| draw(f, Some(&p), &h, None, Some("The one after"), &v))
            .unwrap();
        let text = crate::ui::text(&t);
        assert!(text.contains("loading The one after…"), "{text}");
        assert!(
            text.lines().next().unwrap().contains("loading  ·"),
            "{text}"
        );
    }

    #[test]
    fn a_searchs_results_take_the_hills_place() {
        let hit = |n: usize| Hit {
            url: format!("https://music.youtube.com/watch?v={n}"),
            length: if n == 2 {
                "live".into()
            } else {
                format!("{n}:04:42")
            },
            channel: "Lofi Girl".into(),
            title: format!("set {n}: beats to relax and study to, the long one"),
        };
        let mut v = View {
            search: Search::Hits {
                hits: (1..=9).map(hit).collect(),
                at: 0,
            },
            said: None,
        };
        v.step(7);
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 14)).unwrap();
        t.draw(|f| draw(f, None, &Hills::default(), None, None, &v))
            .unwrap();
        insta::assert_snapshot!(crate::ui::text(&t));
        assert_eq!(v.picked().unwrap().title, hit(8).title);
        v.search = Search::Typing("lofi".into());
        t.draw(|f| draw(f, None, &Hills::default(), None, None, &v))
            .unwrap();
        insta::assert_snapshot!("a_search_is_typed_in_a_box", crate::ui::text(&t));
    }

    #[test]
    fn a_frame_of_hills() {
        let mut h = Hills::default();
        for _ in 0..12 {
            h.step(&[1.0, 0.6, 0.2, 0.0], 24, 1.0 / 30.0);
        }
        let rows: Vec<String> = h.lines(24, 5).iter().map(|l| l.to_string()).collect();
        insta::assert_snapshot!(rows.join("\n"));
        let strip: String = h.strip(12).iter().map(|s| s.content.to_string()).collect();
        assert_eq!(strip.chars().count(), 12);
    }
}
