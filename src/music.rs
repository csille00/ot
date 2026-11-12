//! What cliamp is playing, from its socket (~/.config/cliamp/cliamp.sock):
//! ot asks and cliamp answers, one JSON line each way, on a thread of its
//! own, so a frame that is late costs the screen nothing. A search goes
//! through yt-dlp and a pick to cliamp's command, each on a thread of the
//! caller's. Nothing here draws.
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How often the bands are asked for while something plays.
pub const FPS: u32 = 30;

/// What the player is on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Playing {
    /// playing, paused or stopped.
    pub state: String,
    pub title: String,
    pub artist: String,
    /// Seconds into the track, and its length; 0 when unknown.
    pub position: f64,
    pub duration: f64,
    /// Which track of how many, counting from 0; -1 for none.
    pub index: i64,
    pub total: i64,
    /// dB.
    pub volume: f64,
    pub shuffle: bool,
    /// The titles that play next.
    pub next: Vec<String>,
    /// Says playing, but the clock hasn't moved for a second: waiting on
    /// the stream, as after a seek.
    pub buffering: bool,
}

impl Playing {
    pub fn playing(&self) -> bool {
        self.state == "playing"
    }
}

/// One look at the player: its bands now, 0 to 1 and lowest first, and
/// what it plays; None when cliamp isn't running.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    pub bands: Vec<f32>,
    pub playing: Option<Playing>,
}

pub fn sock() -> PathBuf {
    let config = std::env::var_os("CLIAMP_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(|x| PathBuf::from(x).join("cliamp")))
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config/cliamp")
        });
    config.join("cliamp.sock")
}

/// Hands frames to `send` for as long as ot runs: FPS a second while
/// something plays, four a second otherwise, and one empty frame every
/// two seconds while cliamp is away. A connection fails whenever cliamp
/// is slow to answer, as while it loads a pick, so a lost one is made
/// again at once.
pub fn feed(mut send: impl FnMut(Frame) + Send + 'static) {
    std::thread::spawn(move || loop {
        match Conn::open() {
            Ok(mut c) => c.serve(&mut send),
            Err(_) => {
                send(Frame::default());
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    });
}

struct Conn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Conn {
    fn open() -> std::io::Result<Conn> {
        let s = UnixStream::connect(sock())?;
        s.set_read_timeout(Some(Duration::from_secs(2)))?;
        s.set_write_timeout(Some(Duration::from_secs(2)))?;
        Ok(Conn {
            reader: BufReader::new(s.try_clone()?),
            writer: s,
        })
    }

    /// One request, its one reply; Err when the connection is no good.
    fn ask(&mut self, mut msg: Value) -> Result<Value, ()> {
        msg["version"] = json!(2);
        msg["id"] = json!("ot");
        let line = format!("{msg}\n");
        self.writer.write_all(line.as_bytes()).map_err(|_| ())?;
        let mut reply = String::new();
        match self.reader.read_line(&mut reply) {
            Ok(n) if n > 0 => serde_json::from_str(&reply).map_err(|_| ()),
            _ => Err(()),
        }
    }

    /// Frames until the connection fails.
    fn serve(&mut self, send: &mut impl FnMut(Frame)) {
        let mut playing = Playing::default();
        let mut asked = Instant::now() - Duration::from_secs(1);
        let mut sent = Frame::default();
        // What plays next is asked for again only when the queue or the
        // place in it changes.
        let mut queue = (-1, -1);
        let mut moved = (f64::NAN, Instant::now());
        loop {
            if asked.elapsed() >= Duration::from_millis(250) {
                let Ok(snap) = self.ask(json!({ "method": "state.get" })) else {
                    return;
                };
                let snap = &snap["snapshot"];
                let next = std::mem::take(&mut playing.next);
                playing = playing_of(snap);
                if playing.position != moved.0 || !playing.playing() {
                    moved = (playing.position, Instant::now());
                }
                playing.buffering = playing.playing() && moved.1.elapsed() > Duration::from_secs(1);
                let now = (
                    snap["playlist_revision"].as_i64().unwrap_or(0),
                    playing.index,
                );
                playing.next = if now == queue {
                    next
                } else {
                    queue = now;
                    self.next(playing.index)
                };
                asked = Instant::now();
            }
            let on = playing.playing();
            let bands = if on {
                let Ok(r) = self.ask(json!({ "method": "spectrum.get" })) else {
                    return;
                };
                r["result"]["bands"]
                    .as_array()
                    .map(|b| b.iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect())
                    .unwrap_or_default()
            } else {
                // cliamp keeps answering with the last bands it had.
                vec![]
            };
            // A stopped player answers the same every time; the screen
            // hears of it once.
            let frame = Frame {
                bands,
                playing: Some(playing.clone()),
            };
            if frame != sent {
                send(frame.clone());
                sent = frame;
            }
            std::thread::sleep(if on {
                Duration::from_millis(1000 / FPS as u64)
            } else {
                Duration::from_millis(250)
            });
        }
    }

    /// The titles after track `index`. An operation is a job, answered once
    /// it is done.
    fn next(&mut self, index: i64) -> Vec<String> {
        let submit = json!({
            "method": "operation.submit", "operation": "queue.list",
            "params": { "offset": index + 1, "limit": 3 },
        });
        let Ok(mut r) = self.ask(submit) else {
            return vec![];
        };
        for _ in 0..20 {
            let state = r["job"]["state"].as_str().unwrap_or("");
            if state != "queued" && state != "running" {
                break;
            }
            let id = r["job"]["id"].clone();
            std::thread::sleep(Duration::from_millis(20));
            match self.ask(json!({ "method": "job.get", "job_id": id })) {
                Ok(again) => r = again,
                Err(()) => return vec![],
            }
        }
        let result = if r["job"]["result"].is_null() {
            &r["result"]
        } else {
            &r["job"]["result"]
        };
        result["tracks"]
            .as_array()
            .map(|t| {
                t.iter()
                    .filter_map(|x| x["title"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The player as a state snapshot says it stands.
fn playing_of(snap: &Value) -> Playing {
    let track = if snap["track"].is_null() {
        &snap["logical_track"]
    } else {
        &snap["track"]
    };
    Playing {
        state: snap["state"].as_str().unwrap_or("stopped").to_string(),
        title: track["title"].as_str().unwrap_or("").to_string(),
        artist: track["artist"].as_str().unwrap_or("").to_string(),
        position: snap["position"].as_f64().unwrap_or(0.0),
        duration: snap["duration"].as_f64().unwrap_or(0.0),
        index: snap["index"].as_i64().unwrap_or(-1),
        total: snap["total"].as_i64().unwrap_or(0),
        volume: snap["volume"].as_f64().unwrap_or(0.0),
        shuffle: snap["shuffle"] == true,
        next: vec![],
        buffering: false,
    }
}

/// One result of a search.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hit {
    pub url: String,
    /// "1:04:42", or "live".
    pub length: String,
    pub channel: String,
    pub title: String,
}

/// YouTube's top matches for `query`, through yt-dlp; a few seconds.
pub fn search(query: &str) -> Result<Vec<Hit>, String> {
    let out = Command::new("yt-dlp")
        .args([
            "--flat-playlist",
            "--no-warnings",
            "--print",
            "%(ie_key)s\t%(id)s\t%(duration_string)s\t%(channel)s\t%(title)s",
            &format!("ytsearch25:{query}"),
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("yt-dlp: {e}"))?;
    if !out.status.success() {
        return Err(refused(&String::from_utf8_lossy(&out.stderr)));
    }
    let found = hits(&String::from_utf8_lossy(&out.stdout));
    if found.is_empty() {
        return Err(format!("nothing on YouTube for {query}"));
    }
    Ok(found)
}

/// The videos in yt-dlp's rows; channels that come up as matches are
/// left out. The link is YouTube Music's, since cliamp's own YouTube
/// client no longer loads videos.
fn hits(rows: &str) -> Vec<Hit> {
    rows.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f.len() >= 5 && f[0] == "Youtube").then(|| Hit {
                url: format!("https://music.youtube.com/watch?v={}", f[1]),
                length: if f[2] == "NA" { "live" } else { f[2] }.to_string(),
                channel: f[3].to_string(),
                title: f[4].to_string(),
            })
        })
        .collect()
}

/// Plays `url` now, or after what plays, in cliamp, started headless
/// when it isn't running. Err says why it was refused, in one line.
pub fn play(url: &str, now: bool) -> Result<(), String> {
    up()?;
    let params = json!({ "path": url, "play": now }).to_string();
    let out = Command::new("cliamp")
        .args(["remote", "call", "url.load", "--params", &params, "--wait"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cliamp: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(refused(&String::from_utf8_lossy(&out.stderr)))
    }
}

/// cliamp running, started headless if it wasn't, and answering.
fn up() -> Result<(), String> {
    if UnixStream::connect(sock()).is_ok() {
        return Ok(());
    }
    Command::new("cliamp")
        .arg("--daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cliamp: {e}"))?;
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        if UnixStream::connect(sock()).is_ok() {
            return Ok(());
        }
    }
    Err("cliamp didn't start".into())
}

/// Why yt-dlp or cliamp said no, in one line.
fn refused(stderr: &str) -> String {
    if stderr.contains("not a bot") {
        return "YouTube thinks this machine is a bot: put --cookies-from-browser <browser> in ~/.config/yt-dlp/config".into();
    }
    match stderr.rsplit("ERROR: ").next() {
        Some(e) if e.len() < stderr.len() => e.trim().lines().next().unwrap_or("").to_string(),
        _ => stderr
            .trim()
            .lines()
            .last()
            .unwrap_or("refused")
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_keeps_the_videos_and_says_live_for_a_stream() {
        let rows = "Youtube\tabc\t1:04:42\tLofi Girl\tcozy autumn music\nYoutubeTab\tUC1\tNA\tLofi Girl\tLofi Girl\nYoutube\tdef\tNA\tLofi Girl\tlofi hip hop radio\n";
        let h = hits(rows);
        assert_eq!(h.len(), 2);
        assert_eq!(
            (
                h[0].url.as_str(),
                h[0].length.as_str(),
                h[1].length.as_str(),
                h[1].title.as_str()
            ),
            (
                "https://music.youtube.com/watch?v=abc",
                "1:04:42",
                "live",
                "lofi hip hop radio"
            )
        );
    }

    #[test]
    fn a_refusal_is_one_line() {
        assert_eq!(
            refused("WARNING: x\nERROR: [youtube] abc: Video unavailable\n"),
            "[youtube] abc: Video unavailable"
        );
        assert!(refused("ERROR: Sign in to confirm you're not a bot").starts_with("YouTube thinks"));
        assert_eq!(refused("something odd\n"), "something odd");
    }

    #[test]
    fn a_snapshot_is_read_whether_stopped_or_playing() {
        let stopped = json!({ "revision": 1, "state": "stopped", "total": 15, "shuffle": true,
            "logical_track": { "title": "Lofi", "path": "https://radio.cliamp.stream/lofi/stream" } });
        let p = playing_of(&stopped);
        assert_eq!(
            (
                p.state.as_str(),
                p.title.as_str(),
                p.index,
                p.total,
                p.shuffle
            ),
            ("stopped", "Lofi", -1, 15, true)
        );
        assert!(!p.playing());
        let playing = json!({ "revision": 2, "state": "playing", "track": { "title": "A set", "artist": "Someone" },
            "position": 91.5, "duration": 3600.0, "index": 2, "total": 15, "volume": -4.0 });
        let p = playing_of(&playing);
        assert_eq!(
            (
                p.title.as_str(),
                p.artist.as_str(),
                p.position,
                p.duration,
                p.index,
                p.volume
            ),
            ("A set", "Someone", 91.5, 3600.0, 2, -4.0)
        );
        assert!(p.playing());
    }
}
