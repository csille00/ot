//! A reply written in your own nvim (nvim/compose.lua): the draft kept per thread in
//! ~/.cache/ot/compose until :w sends it, with the images it names attached.
use crate::claude::{Turn, Who};
use crate::hc::Agent;
use std::path::{Path, PathBuf};

const SCRIPT: &str = include_str!("../nvim/compose.lua");

/// A reply being written to `to`.
pub struct Compose {
    pub to: Agent,
    draft: PathBuf,
    meta: PathBuf,
    sent: PathBuf,
}

/// Where images the Mac sends land, for the draft to attach.
pub fn paste_dir() -> PathBuf {
    cache().join("paste")
}

fn cache() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/ot")
}

/// The draft to `to`, as left last time, and what nvim needs to show it.
pub fn prepare(to: &Agent) -> Result<Compose, String> {
    let dir = cache().join("compose");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name: String = to.address.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    let draft = dir.join(format!("{name}.md"));
    let (meta, sent) = (dir.join(format!("{name}.json")), dir.join(format!("{name}.sent")));
    let _ = std::fs::remove_file(&sent);
    if !draft.exists() {
        std::fs::write(&draft, "").map_err(|e| e.to_string())?;
    }
    let script = crate::nvim::scratch().join("compose.lua");
    std::fs::create_dir_all(crate::nvim::scratch()).map_err(|e| e.to_string())?;
    std::fs::write(&script, SCRIPT).map_err(|e| e.to_string())?;
    let m = serde_json::json!({
        "title": format!("reply to {}", to.name),
        "draft": draft,
        "sent": sent,
        "paste": paste_dir(),
    });
    std::fs::write(&meta, m.to_string()).map_err(|e| e.to_string())?;
    Ok(Compose { to: to.clone(), draft, meta, sent })
}

/// The conversation as markdown, its last `most` things said.
fn markdown(name: &str, turns: &[Turn], most: usize) -> String {
    let mut out = format!("# {name}\n");
    let said: Vec<&Turn> = turns.iter().filter(|t| t.who != Who::Tool).collect();
    for t in said.iter().skip(said.len().saturating_sub(most)) {
        out.push_str(if t.who == Who::You { "\n## you\n\n" } else { "\n## claude\n\n" });
        out.push_str(t.text.trim());
        out.push('\n');
    }
    out
}

/// nvim, read-only and with your config, on the whole conversation as markdown, at
/// its end: to select, search and yank what was said. Over ssh a yank reaches your
/// clipboard through OSC 52 unless your config says otherwise.
pub fn reader(name: &str, turns: &[Turn]) -> Result<Vec<String>, String> {
    let dir = cache().join("read");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).take(60).collect();
    let path = dir.join(format!("{file}.md"));
    std::fs::write(&path, markdown(name, turns, usize::MAX)).map_err(|e| e.to_string())?;
    Ok(vec![
        "-R".into(),
        "-c".into(),
        "lua if vim.env.SSH_TTY and vim.g.clipboard == nil then vim.g.clipboard = 'osc52' end".into(),
        "-c".into(),
        "setlocal wrap linebreak nomodifiable".into(),
        "+".into(),
        path.display().to_string(),
    ])
}

/// nvim's arguments for the draft, with your config.
pub fn nvim_args(c: &Compose) -> Vec<String> {
    let script = crate::nvim::scratch().join("compose.lua");
    vec![
        "--cmd".into(),
        format!("let g:ot_compose = '{}'", c.meta.display()),
        "-c".into(),
        format!("lua dofile('{}')", script.display()),
        c.draft.display().to_string(),
    ]
}

/// The Mac's files a drop on Ghostty names, when that is all `text` is: their
/// paths, as Ghostty writes them (backslashes before spaces and the like).
pub fn dropped(text: &str) -> Option<Vec<String>> {
    let mut paths = vec![];
    let mut cur = String::new();
    let mut chars = text.trim().chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => cur.extend(chars.next()),
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    paths.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        paths.push(cur);
    }
    let mac = |p: &String| ["/Users/", "/var/folders/", "/private/", "/Volumes/"].iter().any(|r| p.starts_with(r)) && !Path::new(p).exists();
    (!paths.is_empty() && paths.iter().all(mac)).then_some(paths)
}

/// Where ssh reaches the Mac: OT_MAC, else the address this ssh session came from,
/// as the same user unless OT_MAC_USER says.
pub fn mac() -> Option<String> {
    if let Some(m) = std::env::var("OT_MAC").ok().filter(|m| !m.is_empty()) {
        return Some(m);
    }
    let from = std::env::var("SSH_CLIENT").ok()?.split_whitespace().next()?.to_string();
    let user = std::env::var("OT_MAC_USER").or_else(|_| std::env::var("USER")).ok()?;
    Some(format!("{user}@{from}"))
}

/// Copies `path` from the Mac into the paste folder, where the open draft
/// attaches it; the copy's path.
pub fn fetch(mac: &str, path: &str) -> Result<PathBuf, String> {
    let base = Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or("dropped");
    let name: String = base.chars().map(|c| if c.is_whitespace() || c == '(' || c == ')' { '-' } else { c }).collect();
    let dir = paste_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut to = dir.join(&name);
    if to.exists() {
        to = dir.join(format!("{}-{name}", jiff::Timestamp::now().as_second()));
    }
    let part = dir.join(format!(".{}", to.file_name().and_then(|n| n.to_str()).unwrap_or("part")));
    let file = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let quoted = format!("'{}'", path.replace('\'', "'\\''"));
    let out = std::process::Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=5", "-o", "StrictHostKeyChecking=accept-new", mac, "cat", "--", &quoted])
        .stdout(file)
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("ssh: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&part);
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if why.is_empty() { format!("ssh to {mac} failed") } else { why });
    }
    std::fs::rename(&part, &to).map_err(|e| e.to_string())?;
    Ok(to)
}

/// What :w sent, as text and the images it names; None when nvim was left without it.
pub fn written(c: &Compose) -> Option<(String, Vec<PathBuf>)> {
    if !c.sent.exists() {
        return None;
    }
    let text = std::fs::read_to_string(&c.draft).ok()?;
    let (text, images) = images(&text);
    Some((text, images))
}

/// The draft is sent: nothing of it is kept.
pub fn done(c: &Compose) {
    for p in [&c.draft, &c.sent] {
        let _ = std::fs::remove_file(p);
    }
}

/// `text` without its `![name](path)` images that are files here, and those files.
pub fn images(text: &str) -> (String, Vec<PathBuf>) {
    let mut out = String::new();
    let mut found = vec![];
    let mut rest = text;
    while let Some(i) = rest.find("![") {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        let picked = after.find("](").and_then(|j| after[j + 2..].find(')').map(|k| (j + 2, j + 2 + k)));
        match picked {
            Some((from, to)) if is_image(Path::new(&after[from..to])) => {
                found.push(PathBuf::from(&after[from..to]));
                rest = &after[to + 1..];
            }
            _ => {
                out.push_str("![");
                rest = &after[2..];
            }
        }
    }
    out.push_str(rest);
    (out.trim().to_string(), found)
}

fn is_image(p: &Path) -> bool {
    p.is_file() && mime(p).is_some()
}

/// An image's type, by its name.
pub fn mime(p: &Path) -> Option<&'static str> {
    match p.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drop_of_the_macs_files_is_read_as_their_paths() {
        assert_eq!(
            dropped("/Users/connor/Desktop/Screenshot\\ 2026-11-13\\ at\\ 9.41.png /Users/connor/notes.pdf "),
            Some(vec!["/Users/connor/Desktop/Screenshot 2026-11-13 at 9.41.png".into(), "/Users/connor/notes.pdf".into()])
        );
        assert_eq!(dropped("look at /Users/connor/x.png"), None, "text that is more than paths pastes as text");
        assert_eq!(dropped("/home/connor/ot/Cargo.toml"), None);
    }

    #[test]
    fn images_that_are_files_here_come_out_of_the_text() {
        let dir = std::env::temp_dir().join(format!("ot-compose-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shot = dir.join("shot.png");
        std::fs::write(&shot, b"png").unwrap();
        let text = format!("look at this\n![shot.png]({})\nand ![gone](/nowhere/x.png) too", shot.display());
        let (text, found) = images(&text);
        assert_eq!(found, [shot]);
        assert_eq!(text, "look at this\n\nand ![gone](/nowhere/x.png) too");
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod nvim_tests {
    use super::*;
    use std::process::Command;

    fn agent() -> Agent {
        Agent { address: format!("test-{}", std::process::id()), name: "billing".into(), status: crate::hc::Status::Stopped, said: String::new(), at: 0, pid: None, branch: String::new() }
    }

    /// nvim, headless and without your config, on the draft, running `then` once it opens;
    /// it gives up after 15s rather than hang.
    fn run(c: &Compose, then: &[&str]) {
        let mut cmd = Command::new("nvim");
        cmd.arg("--headless").arg("-u").arg("NONE").args(nvim_args(c));
        cmd.arg("-c").arg("lua vim.defer_fn(function() vim.cmd('cquit 3') end, 15000)");
        for t in then {
            cmd.arg("-c").arg(t);
        }
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn w_sends_what_is_written_and_q_keeps_it_for_next_time() {
        if Command::new("nvim").arg("--version").output().is_err() {
            return;
        }
        let turns = vec![Turn { who: Who::Claude, text: "Done.".into(), at: 0, times: 1, agent: None }];
        let c = prepare(&agent()).unwrap();
        run(&c, &["call setline(1, 'kept for later')", "q"]);
        assert!(written(&c).is_none(), ":q sends nothing");
        let c = prepare(&agent()).unwrap();
        assert_eq!(std::fs::read_to_string(&c.draft).unwrap().trim(), "kept for later", "the draft comes back");
        run(&c, &["call setline(1, 'ship it')", "w"]);
        assert_eq!(written(&c).map(|w| w.0), Some("ship it".to_string()));
        done(&c);
        assert!(!c.draft.exists());
    }

    #[test]
    fn an_image_sent_to_the_paste_folder_goes_in_the_draft() {
        if Command::new("nvim").arg("--version").output().is_err() {
            return;
        }
        let mut a = agent();
        a.address.push_str("-img");
        let c = prepare(&a).unwrap();
        let name = format!("paste-test-{}.png", std::process::id());
        let shot = paste_dir().join(&name);
        // As the Mac's script sends it: under a dot name, then renamed.
        let part = paste_dir().join(format!(".{name}"));
        let drop = format!("lua vim.defer_fn(function() vim.fn.writefile({{'png'}}, '{}'); vim.uv.fs_rename('{}', '{}') end, 200)", part.display(), part.display(), shot.display());
        run(&c, &[&drop, "call wait(7000, {-> getline(1) =~ 'paste-test'})", "w"]);
        let (_, images) = written(&c).unwrap();
        assert_eq!(images, std::slice::from_ref(&shot));
        done(&c);
        let _ = std::fs::remove_file(shot);
    }
}
