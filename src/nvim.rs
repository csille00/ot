//! The review, in nvim. ot works out everything the review shows and hands
//! it to the plugin (nvim/ot.lua, compiled in, so the two never drift) as
//! JSON, which only draws it and answers keys. The JSON is written to files
//! while nvim starts, so the two overlap: the plugin waits on the files,
//! never the other way round. Each file holds what it is for, or
//! {"note": …} while ot is still working on it (the plugin shows the note
//! and keeps waiting), or {"error": …}.
//!
//! What the plugin asks of GitLab (a comment, finishing the review) comes
//! back to ot over a socket, as a `Request`, to the same writes as the
//! board's.
//!
//! In the JSON every index and line number counts from 1, as Lua does.
use crate::gitlab::Shared;
use crate::keys::{self, KEYS};
use crate::model::{bang, size, with_commas, Mr};
use crate::music::{Frame, Playing};
use crate::ui::music::Hills;
use ratatui::style::Color;
use crate::review::{self, Line, ListRow, Mark, Review, Talk};
use crate::write::{self, At, Position, Spot, Write, Writer};
use crate::{cache, git, tour, worktree};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write as _};
use std::ops::Range;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const PLUGIN: &str = include_str!("../nvim/ot.lua");

/// One review's files: what the plugin reads, written when ready.
pub struct Open {
    /// Everything the review shows (`shown`).
    pub review: PathBuf,
    /// What has been said on the MR: description, threads, your drafts.
    pub talk: PathBuf,
    /// Where the review checkout is once it is at the MR's head, so nvim
    /// can trade its copies for real files your LSP works on.
    pub tree: PathBuf,
    /// Claude's tour of the change, when the review was opened for one.
    pub tour: PathBuf,
}

/// Where this ot keeps its open reviews' files; gone when it exits.
pub fn scratch() -> PathBuf {
    std::env::temp_dir().join(format!("ot-{}", std::process::id()))
}

/// Whether a review is open, for what is only written while one reads it.
static REVIEWING: AtomicBool = AtomicBool::new(false);

/// What cliamp plays, for the review's plugin (its music line, and the
/// player M opens there): written while a review is open and when it
/// changes, which while something plays is once a second, as the clock
/// moves. Called from the music feed's own thread, so it keeps up while
/// nvim has the terminal.
pub fn music(frame: &Frame) {
    static LAST: Mutex<Option<Playing>> = Mutex::new(None);
    if !REVIEWING.load(Ordering::SeqCst) {
        return;
    }
    foot(&frame.bands);
    let now = frame.playing.as_ref().map(|p| Playing {
        position: p.position.floor(),
        ..p.clone()
    });
    let mut last = LAST.lock().unwrap();
    if *last == now {
        return;
    }
    *last = now.clone();
    let v = match now {
        Some(p) => json!({
            "state": p.state, "buffering": p.buffering,
            "title": p.title, "artist": p.artist,
            "position": p.position, "duration": p.duration,
            "index": p.index, "total": p.total,
            "volume": p.volume, "shuffle": p.shuffle, "next": p.next,
        }),
        None => json!({ "state": "gone", "title": "" }),
    };
    write_json(&music_path(), &v);
}

fn music_path() -> PathBuf {
    scratch().join("music.json")
}

/// cliamp's hills at the foot of nvim's list, stepped with each of its
/// frames like the board's and written for the plugin, at the size it
/// last asked for; a quiet frame is written once, to clear them.
struct Foot {
    cols: usize,
    depth: usize,
    hills: Hills,
    drawn: bool,
}

static FOOT: Mutex<Option<Foot>> = Mutex::new(None);

fn foot_size(cols: usize, depth: usize) {
    let mut foot = FOOT.lock().unwrap();
    let hills = foot.take().map(|f| f.hills).unwrap_or_default();
    *foot = Some(Foot { cols, depth, hills, drawn: false });
}

fn foot(bands: &[f32]) {
    let mut foot = FOOT.lock().unwrap();
    let Some(f) = foot.as_mut().filter(|f| f.cols > 0) else {
        return;
    };
    f.hills.tick(bands, f.cols);
    let quiet = f.hills.quiet();
    if quiet && !f.drawn {
        return;
    }
    f.drawn = !quiet;
    let rows: Vec<Value> = f
        .hills
        .rows(f.cols, f.depth)
        .into_iter()
        .map(|(text, c)| {
            let Color::Rgb(r, g, b) = c else { unreachable!("a tone is rgb") };
            json!({ "text": text, "fg": format!("#{r:02x}{g:02x}{b:02x}") })
        })
        .collect();
    write_json(&hills_path(), &json!({ "rows": rows }));
}

fn hills_path() -> PathBuf {
    scratch().join("hills.json")
}

/// The newest open, which the review worktree is for. It outlives the
/// session: a move still waiting on a fetch when you have left the review
/// and opened another must not then move the worktree from under it.
static LATEST: AtomicU64 = AtomicU64::new(0);

/// The review open now, as nvim's requests need it.
#[derive(Clone)]
struct Session {
    /// Which open this is, to tell a late answer for an older one.
    opened: u64,
    project: String,
    mr: Mr,
    /// Yours: comments go out at once. Someone else's: they are drafts
    /// until you finish the review.
    mine: bool,
    /// The commit your edits here are made on: the head the review shows,
    /// then each one pushed from it.
    at: String,
    /// Its change, once read.
    review: Option<Arc<Review>>,
}

/// Takes nvim's requests on a socket and answers each with what has been
/// said on the MR since, so the plugin only redraws.
#[derive(Clone)]
pub struct Server {
    session: Arc<Mutex<Option<Session>>>,
}

fn sock() -> PathBuf {
    scratch().join("ot.sock")
}

impl Server {
    pub fn new(writer: Arc<dyn Writer>, gitlab: Shared) -> Server {
        let server = Server {
            session: Arc::default(),
        };
        let _ = std::fs::create_dir_all(scratch());
        let _ = std::fs::remove_file(sock());
        if let Ok(listener) = UnixListener::bind(sock()) {
            let held = server.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (server, writer, gitlab) = (held.clone(), writer.clone(), gitlab.clone());
                    std::thread::spawn(move || server.serve(stream, writer.as_ref(), &gitlab));
                }
            });
        }
        server
    }

    fn session(&self) -> std::sync::MutexGuard<'_, Option<Session>> {
        self.session.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A review opens; its change comes later (attach).
    fn begin(&self, opened: u64, project: &str, mr: &Mr, mine: bool) {
        REVIEWING.store(true, Ordering::SeqCst);
        *self.session() = Some(Session {
            opened,
            project: project.to_string(),
            mr: mr.clone(),
            mine,
            at: mr.head.clone(),
            review: None,
        });
    }

    /// A push from it went out as `head`, which further edits are made on.
    fn pushed(&self, opened: u64, head: String) {
        if let Some(s) = self.session().as_mut().filter(|s| s.opened == opened) {
            s.at = head;
        }
    }

    /// Its change is read, unless that review is no longer the open one.
    fn attach(&self, opened: u64, review: Arc<Review>) {
        if let Some(s) = self.session().as_mut().filter(|s| s.opened == opened) {
            s.review = Some(review);
        }
    }

    /// The review is over: its files go, and a request still on its way
    /// finds nothing to do.
    pub fn end(&self, open: Open) {
        REVIEWING.store(false, Ordering::SeqCst);
        *self.session() = None;
        *FOOT.lock().unwrap() = None;
        for f in [open.review, open.talk, open.tree, open.tour] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// One request, one answer: what has been said since, or the error.
    fn serve(&self, stream: UnixStream, writer: &dyn Writer, gitlab: &Shared) {
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            return;
        }
        let session = self.session().clone();
        let answer = (|| {
            let request: Request = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            let how = match request {
                Request::Hills { cols, depth } => {
                    foot_size(cols, depth);
                    return Ok(json!({}));
                }
                Request::Amend => Some(worktree::Commit::Amend),
                Request::Commit { ref message } => Some(worktree::Commit::New(message.clone())),
                _ => None,
            };
            let session = session.ok_or("the review is closed")?;
            if let Request::Upload { ref path } = request {
                return upload(gitlab, &session.project, path);
            }
            if let Some(how) = how {
                let _one = worktree::CHECKOUT.lock().unwrap_or_else(|e| e.into_inner());
                // Read now, not before the wait: a push just ahead moved it.
                let at = self.session().as_ref().filter(|s| s.opened == session.opened).map(|s| s.at.clone()).ok_or("the review is closed")?;
                let tree = cache::review_tree(&session.project);
                let pushed = worktree::push(&tree, &session.mr.source, &at, how)?;
                self.pushed(session.opened, git::rev(&tree, "HEAD").unwrap_or_default());
                return Ok(json!({ "pushed": pushed }));
            }
            let review = session
                .review
                .clone()
                .ok_or("the change is still being read")?;
            write::all(writer, &writes_for(request, &session, &review)?)?;
            // It went through: a reload that fails mustn't read as a failed
            // send, or it gets sent twice.
            let talk = gitlab
                .get()
                .and_then(|g| g.talk(&session.project, session.mr.iid, &session.mr.head))
                .map_err(|e| format!("sent; couldn't reload what was said: {e}"))?;
            Ok::<_, String>(placed(&review, &talk))
        })();
        let reply = answer.unwrap_or_else(|e| json!({ "error": e }));
        let _ = (&stream).write_all(format!("{reply}\n").as_bytes());
    }
}

/// What the plugin asks of ot (ot.lua's `send`), a JSON line each, named
/// by its "op".
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Request {
    /// A comment: under a thread if `reply`; else on a line of `file`, by
    /// its new number (`line`) or a removed one by its old (`old`), or on
    /// the lines `from` to `line`; else on the MR.
    Comment {
        body: String,
        reply: Option<String>,
        file: Option<usize>,
        line: Option<u32>,
        old: Option<u32>,
        from: Option<u32>,
    },
    /// One of your drafts, changed; saved empty, it is deleted.
    Edit {
        id: u64,
        body: String,
    },
    Resolve {
        thread: String,
        resolved: bool,
    },
    /// The review finished: your drafts sent, then the comment if there is
    /// one, then the approval if asked for.
    Finish {
        body: String,
        approve: bool,
    },
    /// How big the hills at the foot of the list are, 0 wide while there
    /// is no room for them.
    Hills {
        cols: usize,
        depth: usize,
    },
    /// Your edits in the review checkout, folded into your MR's last
    /// commit and pushed.
    Amend,
    /// Your edits in the review checkout as a new commit on your MR, pushed.
    Commit {
        message: String,
    },
    /// A picture or video a body shows, to draw (`upload`).
    Upload {
        path: String,
    },
}

/// A picture or video a body shows, by the path GitLab links it at
/// ("/uploads/<secret>/<name>"), fetched into ot's cache the first time for
/// the review's image plugin to draw: `file`, and for a video `still`, its
/// first frame, since a terminal draws pictures, not video. The path is
/// whatever the MR's author wrote, so it must name one file in the cache.
fn upload(gitlab: &Shared, project: &str, path: &str) -> Result<Value, String> {
    let (secret, name) = path
        .strip_prefix("/uploads/")
        .and_then(|p| p.split_once('/'))
        .filter(|(s, n)| {
            !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit()) && !n.contains('/') && *n != "." && *n != ".."
        })
        .ok_or_else(|| format!("not an upload: {path}"))?;
    let file = cache::uploads(project).join(secret).join(name);
    if !file.exists() {
        cache::write(&file, &gitlab.get()?.upload(project, path)?)?;
    }
    // GitLab's own list of what it plays as video.
    let video = ["mp4", "m4v", "mov", "webm", "ogv"]
        .iter()
        .any(|e| file.extension().is_some_and(|x| x.eq_ignore_ascii_case(e)));
    let still = video.then(|| still(&file)).flatten();
    Ok(json!({ "file": file, "still": still }))
}

/// A video's first frame as a PNG beside it, made once; none without ffmpeg.
fn still(video: &Path) -> Option<PathBuf> {
    let png = PathBuf::from(format!("{}.png", video.display()));
    if png.exists() {
        return Some(png);
    }
    let part = PathBuf::from(format!("{}.part.png", video.display()));
    let made = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(video)
        .args(["-frames:v", "1"])
        .arg(&part)
        .output()
        .is_ok_and(|o| o.status.success());
    (made && std::fs::rename(&part, &png).is_ok()).then_some(png)
}

/// The writes a request makes. A comment on your own MR goes out now; on
/// anyone else's it is a draft until you finish the review.
fn writes_for(request: Request, session: &Session, review: &Review) -> Result<Vec<Write>, String> {
    let mr = &session.mr;
    let iid = mr.iid;
    Ok(match request {
        // serve answers these before the GitLab path; they never get here.
        Request::Hills { .. } | Request::Amend | Request::Commit { .. } | Request::Upload { .. } => {
            unreachable!("not a write to GitLab")
        }
        Request::Comment { body, .. } if body.trim().is_empty() => vec![],
        Request::Comment {
            body,
            reply,
            file,
            line,
            old,
            from,
        } => {
            let at = match (reply, file) {
                (Some(thread), _) => At::Reply(thread),
                (None, Some(f)) => At::Line(position(review, mr, f, line, old, from)?),
                (None, None) => At::Mr,
            };
            let body = body.trim().to_string();
            vec![if session.mine {
                Write::Comment { iid, at, body }
            } else {
                Write::Draft { iid, at, body }
            }]
        }
        Request::Edit { id, body } if body.trim().is_empty() => {
            vec![Write::DeleteDraft { iid, id }]
        }
        Request::Edit { id, body } => vec![Write::EditDraft {
            iid,
            id,
            body: body.trim().to_string(),
        }],
        Request::Resolve { thread, resolved } => vec![Write::Resolve {
            iid,
            thread,
            resolved,
        }],
        Request::Finish { body, approve } => {
            let mut writes = vec![Write::Publish { iid }];
            if !body.trim().is_empty() {
                let body = body.trim().to_string();
                writes.push(Write::Comment {
                    iid,
                    at: At::Mr,
                    body,
                });
            }
            if approve {
                writes.push(Write::Approve {
                    iid,
                    head: mr.head.clone(),
                });
            }
            writes
        }
    })
}

/// A line of the review as GitLab names it: one still there by its new
/// number (and its old, when unchanged), a removed one by its old; or the
/// lines still there from `from` to `new`. `file` counts from 1.
fn position(
    review: &Review,
    mr: &Mr,
    file: usize,
    new: Option<u32>,
    old: Option<u32>,
    from: Option<u32>,
) -> Result<Position, String> {
    let f = file
        .checked_sub(1)
        .and_then(|i| review.files.get(i))
        .ok_or("no such file")?;
    let line = f.line_at(new, old).ok_or("no such line in the change")?;
    let side = |n: u32| (n > 0).then_some(n);
    let spot = |n: u32| -> Result<Spot, String> {
        let (old, l) = f.old_pos(n).ok_or("no such line in the change")?;
        Ok(Spot {
            old,
            new: n,
            added: l.mark == Mark::Add,
        })
    };
    let range = match (from, new) {
        (Some(first), Some(last)) => Some([spot(first)?, spot(last)?]),
        _ => None,
    };
    Ok(Position {
        base: mr.base.clone(),
        start: mr.start.clone(),
        head: mr.head.clone(),
        old_path: f.path.clone(),
        new_path: f.path.clone(),
        old_line: side(line.old),
        new_line: side(line.new),
        range,
    })
}

/// Starts reading the MR's change and what has been said on it, and moving
/// the review worktree to its head, and says where each will land.
/// `opened` numbers the opens, newest highest. With `tour`, Claude is asked
/// for its tour too, which lands when it has read the change.
pub fn start(
    server: &Server,
    project: &str,
    mr: &Mr,
    me: &str,
    gitlab: Shared,
    opened: u64,
    tour: bool,
) -> Open {
    LATEST.store(opened, Ordering::SeqCst);
    let mine = mr.author == me;
    server.begin(opened, project, mr, mine);
    let file = |what: &str| scratch().join(format!("{}-{opened}.{what}.json", mr.iid));
    let open = Open {
        review: file("review"),
        talk: file("talk"),
        tree: file("tree"),
        tour: file("tour"),
    };
    let (review_at, talk_at, tree_at, tour_at) = (open.review.clone(), open.talk.clone(), open.tree.clone(), open.tour.clone());
    let (mr, project, server) = (mr.clone(), project.to_string(), server.clone());
    std::thread::spawn(move || {
        let talk = {
            let (project, iid, head, gitlab) = (project.clone(), mr.iid, mr.head.clone(), gitlab.clone());
            std::thread::spawn(move || gitlab.get().and_then(|g| g.talk(&project, iid, &head)))
        };
        let review = local_commits(&mr, &review_at).and_then(|()| {
            // The worktree moves while the change is read; it needs the head
            // commit, which is only now sure to be here.
            move_worktree(&project, &mr.head, opened, tree_at);
            Ok(Arc::new(review::review(&git::sides(&mr.base, &mr.head)?)))
        });
        match &review {
            Ok(r) => {
                server.attach(opened, r.clone());
                let viewed = cache::viewed(&project);
                write_json(
                    &review_at,
                    &shown(r, &mr, mine, &viewed, &cache::viewed_path(&project), &cache::keys_path()),
                );
            }
            Err(e) => write_json(&review_at, &json!({ "error": e })),
        }
        if let (true, Ok(r)) = (tour, &review) {
            let (r, mr, project) = (r.clone(), mr.clone(), project.clone());
            std::thread::spawn(move || write_json(&tour_at, &toured(&gitlab, &project, &mr, &r, &tour_at)));
        }
        match (review, talk.join().unwrap_or_else(|_| Err("talk".into()))) {
            (Ok(r), Ok(t)) => write_json(&talk_at, &placed(&r, &t)),
            (_, Err(e)) => write_json(&talk_at, &json!({ "error": format!("no comments: {e}") })),
            (Err(_), _) => {}
        }
    });
    open
}

/// Claude's tour of the change for the plugin, or why there is none. While
/// Claude works, `at` holds how far it has got, to show the wait.
fn toured(gitlab: &Shared, project: &str, mr: &Mr, r: &Review, at: &Path) -> Value {
    let reading = |tour: &tour::Tour| {
        let mut v = tour_json(r, tour);
        v["reading"] = json!(true);
        write_json(at, &v);
    };
    reading(&tour::Tour::default());
    let run = cache::tour_dir(project).join(format!("{}-{}", mr.base, mr.head));
    let tour = gitlab
        .get()
        .and_then(|g| g.description(project, mr.iid))
        .and_then(|description| tour::read(mr, &description, r, &run, &mut |tour| reading(tour)));
    match tour {
        Ok(tour) => tour_json(r, &tour),
        Err(e) => json!({ "error": e }),
    }
}

/// A tour as the plugin draws it: each step and flag with its file, its
/// line and the code it is read over, a removed line with no number.
fn tour_json(r: &Review, tour: &tour::Tour) -> Value {
    let shown = |s: &tour::Step| {
        let f = &r.files[s.file];
        let code: Vec<Value> = f.lines[tour::excerpt(f, s)]
            .iter()
            .filter(|l| !(l.mark == Mark::Del && l.moved))
            .map(|l| match (l.mark, l.moved) {
                (Mark::Del, _) => json!({ "mark": "-", "text": l.text, "words": words(l) }),
                (Mark::Add, false) => json!({ "mark": "+", "n": l.new, "text": l.text, "words": words(l) }),
                _ => json!({ "mark": " ", "n": l.new, "text": l.text }),
            })
            .collect();
        json!({ "file": s.file + 1, "path": f.path, "line": s.line, "title": s.title, "says": s.says, "ask": s.ask, "code": code, "beyond": tour::beyond(f, s) })
    };
    let flags: Vec<Value> = tour
        .flags
        .iter()
        .map(|f| {
            let mut v = shown(&f.at);
            v["instead"] = json!(f.instead);
            v
        })
        .collect();
    json!({
        "summary": tour.summary,
        "cases": tour.cases.iter().map(|c| json!({ "when": c.when, "before": c.before, "now": c.now })).collect::<Vec<_>>(),
        "steps": tour.steps.iter().map(shown).collect::<Vec<_>>(),
        "flags": flags,
    })
}

/// Both of the MR's commits, here: fetched from origin when they aren't,
/// saying so in the review's file while it waits.
fn local_commits(mr: &Mr, review_at: &Path) -> Result<(), String> {
    if git::have(&[&mr.base, &mr.head]) {
        return Ok(());
    }
    let note = format!("fetching {} from origin…", bang(mr.iid));
    write_json(review_at, &json!({ "note": note }));
    git::fetch_mr(mr.iid)
}

/// The review worktree onto `head`, in the background, unless a newer
/// review has been opened since. Where it is, or why it isn't, lands in
/// `tree_at`.
fn move_worktree(project: &str, head: &str, opened: u64, tree_at: PathBuf) {
    let (head, tree) = (head.to_string(), cache::review_tree(project));
    std::thread::spawn(move || {
        let _one = worktree::CHECKOUT.lock().unwrap_or_else(|e| e.into_inner());
        if LATEST.load(Ordering::SeqCst) != opened {
            return;
        }
        // Said only when there is a wait, so it never flashes up.
        if !tree.exists() {
            let note = "making ot's review checkout, the first time only…";
            write_json(&tree_at, &json!({ "note": note }));
        } else if !worktree::is_at(&tree, &head) {
            let note = "moving the review checkout here; LSP once it lands…";
            write_json(&tree_at, &json!({ "note": note }));
        }
        let landed = match worktree::review(Path::new("."), &tree, &head) {
            Ok(()) => json!({ "root": tree }),
            Err(e) => json!({ "error": e }),
        };
        write_json(&tree_at, &landed);
    });
}

/// For the plugin, which reads a file only once it is whole. Nothing to do
/// about one that can't be written: the review then says it is reading.
fn write_json(path: &Path, v: &Value) {
    let _ = cache::write(path, v.to_string().as_bytes());
}

/// nvim with your config and this ot's own plugin, opening the review.
pub fn command(open: &Open) -> Command {
    let file = scratch().join("ot.lua");
    let _ = cache::write(&file, PLUGIN.as_bytes());
    let mut c = Command::new("nvim");
    c.args(LOAD);
    c.env("OT_PLUGIN", &file)
        .env("OT_REVIEW", &open.review)
        .env("OT_TALK", &open.talk)
        .env("OT_TREE", &open.tree)
        .env("OT_TOUR", &open.tour)
        .env("OT_MUSIC", music_path())
        .env("OT_HILLS", hills_path())
        .env("OT_SOCK", sock());
    c
}

/// The plugin by its path, not the runtimepath, which lazy.nvim resets
/// while your config loads; and a name to open, since a dashboard shows
/// only when nvim starts with none.
const LOAD: [&str; 3] = [
    "-c",
    "lua package.loaded.ot = dofile(vim.env.OT_PLUGIN); require('ot').open()",
    "ot://review",
];

/// Everything the review shows, for the plugin: the MR, every file with the
/// diff drawn on it, the symbols, the loose rows, which of those you have
/// viewed, the review's keys, and the key log they are written to.
pub fn shown(
    r: &Review,
    mr: &Mr,
    mine: bool,
    viewed: &HashSet<String>,
    viewed_path: &Path,
    keys_path: &Path,
) -> Value {
    let files: Vec<Value> = (0..r.files.len()).map(|f| file_json(r, f)).collect();
    let symbols: Vec<Value> = (0..r.symbols.len()).map(|i| symbol_json(r, i)).collect();
    let loose: Vec<Value> = r
        .loose
        .iter()
        .enumerate()
        .map(|(i, l)| {
            json!({
                "file": l.file + 1,
                "counts": counts(l.adds, l.dels),
                "adds": plus(l.adds),
                "dels": minus(l.dels),
                "hunks": jumps(r, ListRow::Loose(i)),
                "mark": r.viewed_mark(ListRow::Loose(i)),
            })
        })
        .collect();
    // Of every row's mark, those you have viewed.
    let marked: Vec<&Value> = files
        .iter()
        .chain(&symbols)
        .chain(&loose)
        .map(|row| &row["mark"])
        .filter(|m| m.as_str().is_some_and(|m| viewed.contains(m)))
        .collect();
    let keys: Vec<Value> = KEYS
        .iter()
        .filter(|k| k.on.iter().any(|p| keys::REVIEW.contains(p)))
        .map(|k| {
            json!({
                "key": k.key,
                "word": k.word,
                "help": k.help,
                "on": k.on.iter().map(|p| format!("{p:?}")).collect::<Vec<_>>(),
                // ot.lua dispatches on this name (see keys::Act).
                "act": format!("{:?}", k.act),
            })
        })
        .collect();
    let size = format!(
        "{} in {} {}",
        size(mr.adds, mr.dels),
        with_commas(mr.files),
        if mr.files == 1 { "file" } else { "files" }
    );
    json!({
        "iid": mr.iid,
        "name": bang(mr.iid),
        "title": mr.title,
        "url": mr.url,
        "author": mr.author,
        "mine": mine,
        "pipeline": mr.pipeline,
        "failed_jobs": mr.failed_jobs,
        "size": size,
        "files": files,
        "symbols": symbols,
        "loose": loose,
        "roots": r.roots.iter().map(|i| i + 1).collect::<Vec<_>>(),
        "viewed": marked,
        "viewed_path": viewed_path,
        "keys_path": keys_path,
        "keys": keys,
    })
}

fn symbol_json(r: &Review, i: usize) -> Value {
    let s = &r.symbols[i];
    json!({
        "name": s.name,
        "kind": s.kind,
        "file": s.file + 1,
        "counts": counts(s.adds, s.dels),
        "adds": plus(s.adds),
        "dels": minus(s.dels),
        "lines": ends(&s.new),
        "at": s.at,
        "old": ends(&s.old),
        // The file of its other place, if it moved between files.
        "moved": s.moved.map(|f| r.files[f].path.as_str()),
        // What a symbol opens to: what it uses, then its members.
        "kids": s.uses.iter().chain(&s.members).map(|k| k + 1).collect::<Vec<_>>(),
        "mentions": mentions(r, s),
        "hunks": jumps(r, ListRow::Symbol(i)),
        "mark": r.viewed_mark(ListRow::Symbol(i)),
    })
}

/// Where a symbol's own lines name the changed symbols it uses, for the
/// code to underline: [line, first byte, end byte] each. A name right after
/// a dot is something else's property, unless the dot ends a spread, and
/// one before a colon or ?: is a key or a typed property, as seam reads it.
fn mentions(r: &Review, s: &review::Symbol) -> Vec<[usize; 3]> {
    // seam names a type that shares a const's name "type X"; code says X.
    let names: HashSet<&str> = s
        .uses
        .iter()
        .map(|&u| r.symbols[u].name.trim_start_matches("type "))
        .collect();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$';
    let mut out = vec![];
    for l in r.files[s.file]
        .lines
        .iter()
        .filter(|l| l.mark != Mark::Del && s.owns(l))
    {
        let b = l.text.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if !word(b[i]) {
                i += 1;
                continue;
            }
            let start = i;
            while i < b.len() && word(b[i]) {
                i += 1;
            }
            let property = start > 0 && b[start - 1] == b'.' && !l.text[..start].ends_with("...");
            let after = l.text[i..].trim_start();
            let key = after.starts_with(':') || after.starts_with("?:");
            if names.contains(&l.text[start..i]) && !property && !key {
                out.push([l.new as usize, start, i]);
            }
        }
    }
    out
}

/// "+9 −2", leaving out a side with nothing.
fn counts(adds: u32, dels: u32) -> String {
    [plus(adds), minus(dels)]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// "+12", or "" for none: a column of its own in the list.
fn plus(n: u32) -> String {
    if n > 0 {
        format!("+{}", with_commas(n as u64))
    } else {
        String::new()
    }
}

fn minus(n: u32) -> String {
    if n > 0 {
        format!("\u{2212}{}", with_commas(n as u64))
    } else {
        String::new()
    }
}

/// [first, last] of a range of line numbers, or null when empty.
fn ends(r: &Range<u32>) -> Value {
    if r.is_empty() {
        Value::Null
    } else {
        json!([r.start, r.end - 1])
    }
}

/// The line of the shown text a run of changes is at: its first line still
/// there, else the one its removed lines are drawn above, else the last.
fn anchor_line(lines: &[Line], run: &Range<usize>) -> u32 {
    let shown = |l: &&Line| l.mark != Mark::Del;
    lines[run.start..]
        .iter()
        .find(shown)
        .or_else(|| lines.iter().rev().find(shown))
        .map_or(1, |l| l.new)
}

/// The hunks ] and [ walk on a row, in order, as the plugin centers them:
/// the line each is at, its last line there, and how many removed lines
/// are drawn above it.
fn jumps(r: &Review, row: ListRow) -> Vec<Value> {
    struct Jump {
        at: u32,
        last: u32,
        removed: usize,
    }
    let file = &r.files[r.file_of(row)];
    let mut out: Vec<Jump> = vec![];
    for h in r.hunks(row) {
        let run = &file.lines[h.clone()];
        let hunk = if file.gone() {
            Jump {
                at: run[0].old,
                last: run[run.len() - 1].old,
                removed: 0,
            }
        } else {
            let at = anchor_line(&file.lines, &h);
            let last = run
                .iter()
                .rev()
                .find(|l| l.mark != Mark::Del)
                .map_or(at, |l| l.new);
            let removed = run.iter().filter(|l| l.mark == Mark::Del).count();
            Jump { at, last, removed }
        };
        // Two runs drawn at one line are one place to jump to.
        match out.last_mut() {
            Some(prev) if prev.at == hunk.at => {
                prev.last = prev.last.max(hunk.last);
                prev.removed += hunk.removed;
            }
            _ => out.push(hunk),
        }
    }
    out.into_iter()
        .map(|j| json!({ "at": j.at, "last": j.last, "removed": j.removed }))
        .collect()
}

fn words(l: &Line) -> Value {
    l.words
        .as_ref()
        .map_or(Value::Null, |w| json!([w.start, w.end]))
}

/// A file as nvim shows it: the text of its new side (its old side, when it
/// was removed), the lines added, and the removed lines in blocks, each
/// drawn above the line it sits on, or below the last when it ends the file.
fn file_json(r: &Review, f: usize) -> Value {
    let file = &r.files[f];
    let gone = file.gone();
    let text: Vec<&str> = file
        .lines
        .iter()
        .filter(|l| (l.mark == Mark::Del) == gone)
        .map(|l| l.text.as_str())
        .collect();
    let added: Vec<Value> = file
        .lines
        .iter()
        .filter(|l| l.mark == Mark::Add && !l.moved)
        .map(|l| json!({ "line": l.new, "words": words(l) }))
        .collect();
    let mut removed: Vec<Value> = vec![];
    if !gone {
        // One that only moved is at its symbol's other place, not drawn here.
        let drawn = |l: &Line| l.mark == Mark::Del && !l.moved;
        let mut i = 0;
        while i < file.lines.len() {
            if !drawn(&file.lines[i]) {
                i += 1;
                continue;
            }
            let start = i;
            while i < file.lines.len() && drawn(&file.lines[i]) {
                i += 1;
            }
            let lines: Vec<Value> = file.lines[start..i]
                .iter()
                .map(|l| json!({ "old": l.old, "text": l.text, "words": words(l) }))
                .collect();
            removed.push(json!({
                "at": anchor_line(&file.lines, &(start..i)),
                "below": i == file.lines.len(),
                "lines": lines,
            }));
        }
    }
    json!({
        "path": file.path,
        "status": file.status.to_string(),
        "aside": file.aside(),
        "kind": file.kind,
        "counts": counts(file.adds, file.dels),
        "adds": plus(file.adds),
        "dels": minus(file.dels),
        "readable": file.readable,
        "gone": gone,
        "file": f + 1,
        "text": text,
        "added": added,
        "removed": removed,
        // Of a removed file's lines, those that only moved, which aren't red.
        "moved": file.lines.iter().filter(|l| gone && l.moved).map(|l| l.old).collect::<Vec<_>>(),
        "hunks": jumps(r, ListRow::File(f)),
        "mark": r.viewed_mark(ListRow::File(f)),
    })
}

/// What has been said, for the plugin: each thread and draft on its file
/// and line, or on the MR when it has no path; how many open threads each
/// symbol has (those on lines it owns, as its hunks are); the description.
pub fn placed(r: &Review, talk: &Talk) -> Value {
    let file = |path: &str| r.files.iter().position(|f| f.path == path);
    let mut open_threads = vec![0u32; r.symbols.len()];
    let mut threads: Vec<Value> = vec![];
    for t in &talk.threads {
        let f = file(&t.path);
        let open = t.resolvable && !t.resolved;
        if let Some(line) = f
            .and_then(|f| r.files[f].line_at(t.new_line, t.old_line))
            .filter(|_| open)
        {
            for (i, s) in r.symbols.iter().enumerate() {
                if Some(s.file) == f && s.owns(line) {
                    open_threads[i] += 1;
                }
            }
        }
        let notes: Vec<Value> = t
            .notes
            .iter()
            .map(|n| json!({ "author": n.author, "body": n.body }))
            .collect();
        threads.push(json!({
            "id": t.id,
            "file": f.map(|f| f + 1),
            "line": t.new_line,
            "old": t.old_line,
            "resolved": t.resolved,
            "resolvable": t.resolvable,
            "notes": notes,
        }));
    }
    let drafts: Vec<Value> = talk
        .drafts
        .iter()
        .map(|d| {
            json!({
                "id": d.id,
                "body": d.body,
                "file": file(&d.path).map(|f| f + 1),
                "line": d.new_line,
                "old": d.old_line,
                "reply_to": d.reply_to,
            })
        })
        .collect();
    json!({
        "description": talk.description,
        "threads": threads,
        "drafts": drafts,
        "open_threads": open_threads,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixture as board;
    use crate::review::{fixture, Note, Thread};

    fn data() -> (Value, Value) {
        let r = review::review(&fixture::sides());
        let mut mr = board::mr(301, "alex", "alex/sum-reduce", "main");
        mr.title = "Sum with reduce and say how many items there were".into();
        let viewed =
            HashSet::from([r.viewed_mark(ListRow::Loose(fixture::loose(&r, "src/sum.ts")))]);
        let shown = shown(&r, &mr, false, &viewed, Path::new("/nowhere/viewed"), Path::new("/nowhere/keys.tsv"));
        let thread = Thread {
            id: "d1".into(),
            path: "src/sum.ts".into(),
            new_line: Some(8),
            resolvable: true,
            notes: vec![Note {
                author: "kim".into(),
                body: "Could count go in the same template string as total?".into(),
            }],
            ..Default::default()
        };
        let threads = placed(
            &r,
            &Talk {
                description: "Sums with reduce.\n\n![shot](/uploads/ab12/shot.png){width=900 height=500}\n\n![demo](/uploads/cd34/demo.mp4)".into(),
                threads: vec![thread],
                drafts: vec![],
            },
        );
        (shown, threads)
    }

    #[test]
    fn removed_lines_hang_above_the_line_they_sat_on() {
        let (d, _) = data();
        let f = &d["files"][1];
        assert_eq!(f["path"], "src/sum.ts");
        assert_eq!(f["text"][0], "import { a, b } from \"./a\";");
        let first = &f["removed"][0];
        assert_eq!(
            (first["at"].as_u64(), first["below"].as_bool()),
            (Some(1), Some(false))
        );
        assert_eq!(first["lines"][0]["text"], "import { a } from \"./a\";");
        assert_eq!(f["added"][0], json!({ "line": 1, "words": [10, 13] }));
    }

    #[test]
    fn a_symbols_jumps_are_its_own_hunks() {
        let (d, _) = data();
        let report = d["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == "report")
            .unwrap();
        // Its new line at 8, and the old one drawn above 11; its first
        // line is the same at both places.
        assert_eq!(
            report["hunks"],
            json!([{ "at": 8, "last": 8, "removed": 0 }, { "at": 11, "last": 11, "removed": 1 }])
        );
        assert_eq!(report["lines"], json!([7, 9]));
    }

    #[test]
    fn a_thread_counts_on_the_symbol_that_owns_its_line() {
        let (d, t) = data();
        let at = |name: &str| {
            d["symbols"]
                .as_array()
                .unwrap()
                .iter()
                .position(|s| s["name"] == name)
                .unwrap()
        };
        assert_eq!(t["open_threads"][at("report")], 1);
        assert_eq!(t["open_threads"][at("total")], 0);
    }

    #[test]
    fn a_symbol_underlines_the_changed_symbols_it_names() {
        let (d, _) = data();
        let report = d["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == "report")
            .unwrap();
        // `total ${total(items)} of ${count(items)}`: the word in the
        // template's text counts too, as it does for seam.
        assert_eq!(
            report["mentions"],
            json!([[8, 10, 15], [8, 18, 23], [8, 37, 42]])
        );
        let line = review::fixture::NEW.lines().nth(7).unwrap();
        assert_eq!(
            [&line[10..15], &line[18..23], &line[37..42]],
            ["total", "total", "count"]
        );
    }

    /// The writes a request, as the plugin sends it, makes.
    fn writes(req: &Value, s: &Session, r: &Review) -> Result<Vec<Write>, String> {
        writes_for(
            serde_json::from_value(req.clone()).map_err(|e| e.to_string())?,
            s,
            r,
        )
    }

    fn session(mine: bool) -> (Session, Review) {
        let mut mr = board::mr(301, if mine { "sam" } else { "alex" }, "x", "main");
        (mr.base, mr.start, mr.head) = ("b".into(), "s".into(), "h".into());
        (
            Session {
                opened: 1,
                project: "team/app".into(),
                at: mr.head.clone(),
                mr,
                mine,
                review: None,
            },
            review::review(&fixture::sides()),
        )
    }

    #[test]
    fn a_comment_is_a_draft_on_their_mr_and_goes_out_on_yours() {
        let (theirs, r) = session(false);
        let req = json!({ "op": "comment", "file": 2, "line": 8, "body": " why reduce? " });
        let ws = writes(&req, &theirs, &r).unwrap();
        let Write::Draft {
            at: At::Line(p),
            body,
            ..
        } = &ws[0]
        else {
            panic!("{ws:?}")
        };
        assert_eq!(
            (
                body.as_str(),
                p.new_line,
                p.old_line,
                p.new_path.as_str(),
                p.head.as_str()
            ),
            ("why reduce?", Some(8), None, "src/sum.ts", "h")
        );
        let (mine, r) = session(true);
        assert!(matches!(
            writes(&req, &mine, &r).unwrap()[0],
            Write::Comment { .. }
        ));
    }

    #[test]
    fn a_line_is_named_by_the_sides_it_is_on() {
        let (s, r) = session(false);
        let at = |req: Value| match writes(&req, &s, &r).unwrap().remove(0) {
            Write::Draft {
                at: At::Line(p), ..
            } => (p.old_line, p.new_line),
            w => panic!("{w:?}"),
        };
        assert_eq!(
            at(json!({ "op": "comment", "file": 2, "old": 10, "body": "gone?" })),
            (Some(10), None),
            "a removed line by its old number"
        );
        assert_eq!(
            at(json!({ "op": "comment", "file": 2, "line": 2, "body": "here" })),
            (Some(2), Some(2)),
            "an unchanged one by both"
        );
    }

    #[test]
    fn a_comment_on_several_lines_spans_them() {
        let (s, r) = session(false);
        // Line 6 is unchanged (old 8); 7 and 8 were added where old 9 and
        // 10 were removed, so GitLab's diff counts them at old 11.
        let req = json!({ "op": "comment", "file": 2, "from": 6, "line": 8, "body": "these" });
        match writes(&req, &s, &r).unwrap().remove(0) {
            Write::Draft {
                at: At::Line(p), ..
            } => assert_eq!(
                (p.new_line, p.range),
                (
                    Some(8),
                    Some([
                        Spot {
                            old: 8,
                            new: 6,
                            added: false
                        },
                        Spot {
                            old: 11,
                            new: 8,
                            added: true
                        },
                    ])
                )
            ),
            w => panic!("{w:?}"),
        }
    }

    #[test]
    fn finishing_sends_the_drafts_then_the_comment_then_the_approval() {
        let (s, r) = session(false);
        let ws = writes(
            &json!({ "op": "finish", "body": "Looks good", "approve": true }),
            &s,
            &r,
        )
        .unwrap();
        assert_eq!(
            ws,
            [
                Write::Publish { iid: 301 },
                Write::Comment {
                    iid: 301,
                    at: At::Mr,
                    body: "Looks good".into()
                },
                Write::Approve {
                    iid: 301,
                    head: "h".into()
                }
            ]
        );
        assert_eq!(
            writes(
                &json!({ "op": "finish", "body": "", "approve": false }),
                &s,
                &r
            )
            .unwrap(),
            [Write::Publish { iid: 301 }]
        );
    }

    #[test]
    fn a_draft_saved_empty_is_deleted() {
        let (s, r) = session(false);
        assert_eq!(
            writes(&json!({ "op": "edit", "id": 9, "body": " " }), &s, &r).unwrap(),
            [Write::DeleteDraft { iid: 301, id: 9 }]
        );
    }

    #[test]
    fn a_request_ot_does_not_know_is_an_error() {
        let (s, r) = session(false);
        assert!(writes(&json!({ "op": "delete", "id": 9 }), &s, &r).is_err());
    }

    #[test]
    fn only_this_reviews_marks_go_to_the_plugin() {
        let (d, _) = data();
        assert_eq!(d["viewed"].as_array().unwrap().len(), 1);
        let sum = d["loose"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["file"] == 2)
            .unwrap();
        assert_eq!(d["viewed"][0], sum["mark"]);
    }

    /// The plugin against what this module hands it, in a headless nvim
    /// without your config (nvim/test.lua says what it checks).
    #[test]
    fn the_plugin_draws_the_review() {
        if Command::new("nvim").arg("--version").output().is_err() {
            eprintln!("no nvim here; the plugin went untested");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ot-plugin-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (d, t) = data();
        let (data_at, threads_at, tree_at) = (
            dir.join("d.json"),
            dir.join("t.json"),
            dir.join("tree.json"),
        );
        let mut d = d;
        d["viewed_path"] = json!(dir.join("viewed"));
        write_json(&data_at, &d);
        write_json(&threads_at, &t);
        // A checkout at the MR's head, as worktree.rs leaves one.
        let root = dir.join("root");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/sum.ts"), review::fixture::NEW).unwrap();
        write_json(&tree_at, &json!({ "root": root }));
        let music_at = dir.join("music.json");
        write_json(
            &music_at,
            &json!({
                "state": "paused", "buffering": false, "title": "A song", "artist": "Someone",
                "position": 100.0, "duration": 200.0, "index": 0, "total": 3,
                "volume": 0.0, "shuffle": false, "next": ["B side"],
            }),
        );
        // Written by test.lua once it is reading something, as Claude's lands.
        let tour_at = dir.join("tour.json");
        let hills_at = dir.join("hills.json");
        write_json(&hills_at, &json!({ "rows": [{ "text": " ▂ ▇ ", "fg": "#c4a62b" }] }));
        // ot's side of the socket, standing in: it keeps each request and
        // answers with what was said, a comment's draft added; the foot's
        // size it only hears, and an upload it says it fetched to /got.
        let sock = dir.join("ot.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let heard: Arc<Mutex<Vec<Value>>> = Arc::default();
        let (keep, said) = (heard.clone(), t.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                if req["op"] == "hills" {
                    let _ = (&stream).write_all(b"{}\n");
                    continue;
                }
                if req["op"] == "upload" {
                    let file = req["path"].as_str().unwrap().replace("/uploads/", "/got/");
                    let still = file.ends_with(".mp4").then(|| format!("{file}.png"));
                    let _ = (&stream).write_all(format!("{}\n", json!({ "file": file, "still": still })).as_bytes());
                    continue;
                }
                let mut answer = said.clone();
                if req["op"] == "comment" {
                    answer["drafts"] = json!([{ "id": 1, "body": req["body"], "file": req["file"], "line": req["line"] }]);
                }
                keep.lock().unwrap().push(req);
                let _ = (&stream).write_all(format!("{answer}\n").as_bytes());
            }
        });
        let root = env!("CARGO_MANIFEST_DIR");
        let out = Command::new("nvim")
            .args([
                "--headless",
                "--clean",
                "-c",
                &format!("lua vim.schedule(function() dofile('{root}/nvim/test.lua') end)"),
            ])
            .args(LOAD)
            .env("OT_PLUGIN", format!("{root}/nvim/ot.lua"))
            .env("OT_REVIEW", &data_at)
            .env("OT_TALK", &threads_at)
            .env("OT_TREE", &tree_at)
            .env("OT_TOUR", &tour_at)
            .env("OT_MUSIC", &music_at)
            .env("OT_HILLS", &hills_at)
            .env("OT_SOCK", &sock)
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{said}");
        assert_eq!(
            *heard.lock().unwrap(),
            [
                json!({ "op": "comment", "file": 2, "line": 8, "body": "why reduce?" }),
                json!({ "op": "comment", "file": 2, "line": 8, "from": 7, "body": "these two" }),
                json!({ "op": "finish", "body": "lgtm", "approve": true })
            ]
        );
    }
}
