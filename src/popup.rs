//! A program (nvim, for a reply) run in a box over ot: ot draws its screen and
//! hands it every key, so ot stays in view around it, in zellij or not.
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Clear};
use ratatui::Frame;
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::sync::{Arc, Mutex};

pub struct Popup {
    pub title: String,
    screen: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    size: (u16, u16),
}

/// What the box draws: its title and the program's screen.
#[derive(Clone)]
pub struct Shows {
    title: String,
    screen: Arc<Mutex<vt100::Parser>>,
}

/// Where the box goes on a screen of `area`: centred, a little over half of it.
pub fn place(area: Rect) -> Rect {
    let w = (area.width * 3 / 5).max(70).min(area.width.saturating_sub(2));
    let h = (area.height * 9 / 20).max(14).min(area.height.saturating_sub(2));
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

fn inner(area: Rect) -> (u16, u16) {
    (area.height.saturating_sub(2).max(1), area.width.saturating_sub(2).max(1))
}

fn nz(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n.max(1)).unwrap_or(NonZeroU16::MIN)
}

impl Popup {
    /// `cmd` in a box over a screen of `area`; `changed` is called when its screen
    /// does, and with true once it has exited.
    pub fn open(mut cmd: CommandBuilder, title: String, area: Rect, changed: impl Fn(bool) + Send + 'static) -> Result<Popup, String> {
        let (rows, cols) = inner(place(area));
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).map_err(|e| e.to_string())?;
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let screen = Arc::new(Mutex::new(vt100::Parser::new(nz(rows), nz(cols), 0)));
        let fed = screen.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut p) = fed.lock() {
                            p.process(&buf[..n]);
                        }
                        changed(false);
                    }
                }
            }
            changed(true);
        });
        Ok(Popup { title, screen, writer, master: pair.master, child, size: (rows, cols) })
    }

    pub fn shows(&self) -> Shows {
        Shows { title: self.title.clone(), screen: self.screen.clone() }
    }

    /// Sizes the program to the box on a screen of `area`.
    pub fn fit(&mut self, area: Rect) {
        let size = inner(place(area));
        if size == self.size {
            return;
        }
        self.size = size;
        let _ = self.master.resize(PtySize { rows: size.0, cols: size.1, pixel_width: 0, pixel_height: 0 });
        if let Ok(mut p) = self.screen.lock() {
            p.screen_mut().set_size(nz(size.0), nz(size.1));
        }
    }

    pub fn key(&mut self, k: &KeyEvent) {
        let app_cursor = self.screen.lock().map(|p| p.screen().application_cursor()).unwrap_or(false);
        let bytes = encode(k, app_cursor);
        if !bytes.is_empty() {
            let _ = self.writer.write_all(&bytes);
            let _ = self.writer.flush();
        }
    }

    pub fn paste(&mut self, text: &str) {
        let bracketed = self.screen.lock().map(|p| p.screen().bracketed_paste()).unwrap_or(false);
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        let out = if bracketed { format!("\x1b[200~{text}\x1b[201~") } else { text };
        let _ = self.writer.write_all(out.as_bytes());
        let _ = self.writer.flush();
    }

    /// Waits for the program, which has exited.
    pub fn close(mut self) {
        let _ = self.child.wait();
    }
}

/// The bytes a terminal sends for `k`.
pub fn encode(k: &KeyEvent, app_cursor: bool) -> Vec<u8> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let arrow = |c: char| if app_cursor { format!("\x1bO{c}") } else { format!("\x1b[{c}") };
    let mut out: Vec<u8> = match k.code {
        KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
            c @ 'a'..='z' => vec![c as u8 & 0x1f],
            '[' => vec![0x1b],
            '\\' => vec![0x1c],
            ']' => vec![0x1d],
            '^' | '6' => vec![0x1e],
            '_' | '-' => vec![0x1f],
            ' ' | '@' | '2' => vec![0],
            c => c.to_string().into_bytes(),
        },
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => arrow('A').into_bytes(),
        KeyCode::Down => arrow('B').into_bytes(),
        KeyCode::Right => arrow('C').into_bytes(),
        KeyCode::Left => arrow('D').into_bytes(),
        KeyCode::Home => arrow('H').into_bytes(),
        KeyCode::End => arrow('F').into_bytes(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::F(n @ 1..=4) => format!("\x1bO{}", (b'P' + n - 1) as char).into_bytes(),
        KeyCode::F(n) => match n {
            5 => b"\x1b[15~".to_vec(),
            6..=10 => format!("\x1b[{}~", n + 11).into_bytes(),
            11 | 12 => format!("\x1b[{}~", n + 12).into_bytes(),
            _ => vec![],
        },
        _ => vec![],
    };
    if alt && !out.is_empty() {
        out.insert(0, 0x1b);
    }
    out
}

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// The box and the program's screen in it, its cursor where the program has it.
pub fn draw(f: &mut Frame, s: &Shows) {
    let at = place(f.area());
    f.render_widget(Clear, at);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(crate::ui::fg(crate::ui::AMBER))
        .title(Line::styled(format!(" {} ", s.title), crate::ui::fg(crate::ui::AMBER)).centered());
    let area = block.inner(at);
    f.render_widget(block, at);
    let Ok(p) = s.screen.lock() else { return };
    let screen = p.screen();
    let buf = f.buffer_mut();
    for row in 0..area.height {
        for col in 0..area.width {
            let Some(cell) = screen.cell(row, col) else { continue };
            if cell.is_wide_continuation() {
                continue;
            }
            let mut style = Style::new().fg(color(cell.fgcolor())).bg(color(cell.bgcolor()));
            for (on, m) in [(cell.bold(), Modifier::BOLD), (cell.dim(), Modifier::DIM), (cell.italic(), Modifier::ITALIC), (cell.underline(), Modifier::UNDERLINED), (cell.inverse(), Modifier::REVERSED)] {
                if on {
                    style = style.add_modifier(m);
                }
            }
            let text = if cell.has_contents() { cell.contents() } else { " " };
            if let Some(c) = buf.cell_mut((area.x + col, area.y + row)) {
                c.set_symbol(text).set_style(style);
            }
        }
    }
    if !screen.hide_cursor() {
        let (r, c) = screen.cursor_position();
        if r < area.height && c < area.width {
            f.set_cursor_position((area.x + c, area.y + r));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_sent_as_a_terminal_sends_them() {
        let k = |code, m| KeyEvent::new(code, m);
        assert_eq!(encode(&k(KeyCode::Char('w'), KeyModifiers::CONTROL), false), [0x17]);
        assert_eq!(encode(&k(KeyCode::Char('x'), KeyModifiers::ALT), false), b"\x1bx");
        assert_eq!(encode(&k(KeyCode::Up, KeyModifiers::NONE), true), b"\x1bOA");
        assert_eq!(encode(&k(KeyCode::Up, KeyModifiers::NONE), false), b"\x1b[A");
        assert_eq!(encode(&k(KeyCode::F(5), KeyModifiers::NONE), false), b"\x1b[15~");
    }
}
