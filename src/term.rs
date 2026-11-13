//! Copy, open and notify on the desktop you look at: the Mac's commands at its keyboard,
//! else OSC 52 / OSC 777 escapes, which reach your terminal over ssh and through zellij.
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Whether ot is at the Mac's own keyboard; false over ssh, where its commands miss you.
fn at_the_mac() -> bool {
    cfg!(target_os = "macos") && std::env::var_os("SSH_TTY").is_none() && std::env::var_os("SSH_CONNECTION").is_none()
}

/// Whether a browser here would be one you can see.
fn has_browser() -> bool {
    at_the_mac()
        || (std::env::var_os("SSH_CONNECTION").is_none()
            && (std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()))
}

/// Puts `text` on the clipboard of the machine you are looking at.
pub fn copy(text: &str) {
    if at_the_mac() {
        run("pbcopy", &[], text);
    } else {
        emit(&osc52(text));
    }
}

/// Opens `url` where you can see it, or copies it when nothing can; the note to show.
pub fn open(url: &str) -> Option<String> {
    if at_the_mac() {
        run("open", &[url], "");
        None
    } else if has_browser() {
        run("xdg-open", &[url], "");
        None
    } else {
        copy(url);
        Some(format!("no browser over ssh: copied {url}"))
    }
}

/// A desktop notification titled ot. zellij drops OSC 777, so inside it the
/// escape goes straight to its clients' ttys.
pub fn notify(body: &str) {
    if at_the_mac() {
        // The text goes as an argument, so nothing in it is read as script.
        let script = ["-e", "on run argv", "-e", "display notification (item 1 of argv) with title \"ot\"", "-e", "end run", body];
        run("osascript", &script, "");
        return;
    }
    let seq = osc777("ot", body);
    if std::env::var_os("ZELLIJ").is_some() {
        let ttys = zellij_client_ttys();
        if !ttys.is_empty() {
            for tty in ttys {
                if let Ok(mut f) = OpenOptions::new().write(true).open(&tty) {
                    let _ = f.write_all(seq.as_bytes());
                }
            }
            return;
        }
    }
    emit(&seq);
}

/// Writes `seq` to the terminal ot draws on; called between frames, on the drawing thread.
fn emit(seq: &str) {
    let mut out = std::io::stdout();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
}

/// A command run for its effect, fed `input`, with nobody waiting on it.
fn run(cmd: &str, args: &[&str], input: &str) {
    let (cmd, args, input): (String, Vec<String>, String) =
        (cmd.into(), args.iter().map(|a| a.to_string()).collect(), input.into());
    std::thread::spawn(move || {
        let child = Command::new(&cmd)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut c) = child {
            if let Some(mut i) = c.stdin.take() {
                let _ = i.write_all(input.as_bytes());
            }
            let _ = c.wait();
        }
    });
}

fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

fn osc777(title: &str, body: &str) -> String {
    format!("\x1b]777;notify;{};{}\x07", plain(title), plain(body))
}

/// `s` without what would end or split the escape: controls become spaces, `;` a comma.
fn plain(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            c if c.is_control() => ' ',
            ';' => ',',
            c => c,
        })
        .collect()
}

/// The ttys of your zellij clients (a zellij whose stdin is a tty; the server's isn't).
fn zellij_client_ttys() -> Vec<PathBuf> {
    let Ok(procs) = std::fs::read_dir("/proc") else { return vec![] };
    let mut ttys: Vec<PathBuf> = procs
        .flatten()
        .filter(|p| p.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit()))
        .filter(|p| std::fs::read_to_string(p.path().join("comm")).is_ok_and(|c| c.trim() == "zellij"))
        .filter_map(|p| std::fs::read_link(p.path().join("fd/0")).ok())
        .filter(|t| t.starts_with("/dev/pts/") || t.starts_with("/dev/tty"))
        .collect();
    ttys.sort();
    ttys.dedup();
    ttys
}

pub fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"osc52-test"), "b3NjNTItdGVzdA==");
    }

    #[test]
    fn a_notification_cannot_end_its_own_sequence() {
        assert_eq!(osc777("ot", "!12: review\x07now; please"), "\x1b]777;notify;ot;!12: review now, please\x07");
    }
}
