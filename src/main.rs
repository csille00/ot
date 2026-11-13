//! ot: your GitLab merge requests as a board you can trust to be current,
//! and a review of one, by the symbols it changed, in nvim.
//!
//! The UI thread only draws and answers keys. GitLab and git run on other
//! threads, and everything comes back to the UI as a message, so no key
//! ever waits on either.
mod ask;
mod brief;
mod cache;
mod claude;
mod compose;
mod config;
mod finder;
mod git;
mod gitlab;
mod hc;
mod judge;
mod keys;
mod land;
mod linear;
mod model;
mod music;
mod nvim;
mod popup;
mod rebase;
mod review;
mod slack;
mod stats;
mod t3;
mod term;
mod tour;
mod ui;
mod worktree;
mod write;

use config::Config;
use gitlab::Shared;
use keys::Act;
use model::{bang, Board, Snapshot};
use ratatui::crossterm::event::{self, DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use ui::board::{self as boardui, Doing, Left, Mode, Pick, Sets, Typing};
use ui::music as musicui;
use ui::session as sessionui;
use write::Writer;

/// How often the board asks GitLab while it is open.
const EVERY: Duration = Duration::from_secs(30);

/// How often instead for SOON_FOR after a write of ot's own, while GitLab
/// is still taking it in (a merge, a pipeline starting) on the row you are
/// watching. A refresh is about four requests, so this is some 50 a minute
/// of the 2,000 GitLab allows.
const SOON: Duration = Duration::from_secs(5);
const SOON_FOR: Duration = Duration::from_secs(60);

/// What a row says while Claude reads it for its landing.
const READING: &str = "Claude reading";

enum Msg {
    Term(Event),
    Fetched(Result<Snapshot, String>),
    /// Claude's sessions on this repo, whenever one of them changes.
    Claude(Vec<claude::Session>),
    /// hc's Claude Code agents, whenever one of them changes.
    Hc(Vec<hc::Agent>),
    /// What cliamp plays and its bands, FPS times a second while it plays.
    Music(music::Frame),
    /// What a search for `query` found.
    Found {
        query: String,
        hits: Result<Vec<music::Hit>, String>,
    },
    /// A pick at `url` played, or why not.
    Played {
        url: String,
        result: Result<(), String>,
    },
    /// A plan's writes to an MR are done, or failed.
    Wrote {
        iid: u64,
        doing: &'static str,
        result: Result<(), String>,
    },
    /// What a rebase being prepared is at: a word for the row, the whole
    /// of it for the panel.
    Step {
        iid: u64,
        word: &'static str,
        detail: String,
    },
    /// A rebase is prepared, to ask about, or could not be.
    Rebased {
        iid: u64,
        result: Result<rebase::Rebased, String>,
    },
    /// A rebase is pushed, with the branches it had to leave alone; or
    /// GitLab took none of it. `past` is its bottom layer and the merged MR
    /// whose commits that left out, if any (Rebased::past).
    Pushed {
        iid: u64,
        past: Option<(u64, u64)>,
        result: Result<Vec<String>, String>,
    },
    /// What Claude said of the landing's MRs it was reading, `read`, or why
    /// it couldn't say.
    Judged {
        read: Vec<u64>,
        result: Result<Vec<land::Verdict>, String>,
    },
    /// An MR's description, read for l, or why it couldn't be.
    Blurb {
        iid: u64,
        result: Result<String, String>,
    },
    /// An MR's description, read for c, or why it couldn't be.
    Described {
        iid: u64,
        result: Result<String, String>,
    },
    /// How something asked of one of hc's agents went, for the note.
    Asked(String),
    /// The reply's nvim, in its box over ot, drew; or, true, has exited.
    Popup(bool),
    /// This machine, read again.
    Stats(stats::Stats),
    /// How a tour read ahead for `iid` at `head` is coming along.
    Toured { iid: u64, head: String, state: boardui::Toured },
    /// Files dropped on the reply were copied from the Mac (how many), or why not.
    Dropped { text: String, got: Result<usize, String> },
    /// A T3 thread started with `text`, its id, or why it wasn't.
    Started {
        text: String,
        result: Result<String, String>,
    },
    /// An MR's diff at `head`, for the panel beside its thread, or why not.
    Patch {
        iid: u64,
        head: String,
        result: Result<String, String>,
    },
    /// A T3 thread's messages, read to show it, or why they couldn't be.
    Thread {
        id: String,
        turns: Result<Vec<claude::Turn>, String>,
    },
    /// Haiku's line on where an agent that stopped stands, or why not.
    Brief {
        worker: model::Worker,
        result: Result<String, String>,
    },
    /// Everyone in your Slack, by email, or why not.
    Slack(Result<Vec<String>, String>),
    /// Everyone in the GitLab project, by username, or why not.
    Members(Result<Vec<String>, String>),
    /// Your tickets, as Linear gave them after GitLab's answer, or why not.
    Tickets(Result<Vec<model::Ticket>, String>),
    /// The review checkout, looked at once nothing else was at it: how
    /// `went` went, a push or putting aside for an MR, if one ran; the
    /// change of yours it holds now, and the MR of yours it is on.
    Checkout {
        went: Option<(u64, Result<String, String>)>,
        change: Option<worktree::Change>,
        owner: Option<(u64, String)>,
    },
}

/// Something done in the review checkout, given where it is, with a line
/// on how it went.
type Work = Box<dyn FnOnce(&Path) -> Result<String, String> + Send>;

/// What the terminal shows: the board, one of Claude's sessions or the
/// agent of one named `agent`, or the music.
enum Shown {
    Board,
    Session {
        id: String,
        agent: Option<String>,
        view: sessionui::View,
    },
    Music(musicui::View),
    /// A T3 thread with no session of Claude's here to read, read through T3.
    Thread {
        agent: hc::Agent,
        turns: Vec<claude::Turn>,
        view: sessionui::View,
    },
}

/// What a key asks of the loop, beyond changing the app.
enum Next {
    Stay,
    Quit,
    /// The terminal to nvim, for a review, until it quits.
    Review(nvim::Open),
    /// The terminal to one of hc's agents, until you detach.
    Attach(hc::Agent),
    /// The terminal to a shell in this folder, until it exits.
    Shell(PathBuf),
    /// The terminal to nvim with these arguments, until it quits.
    Nvim(Vec<String>),
    /// A reply to the agent, written in nvim.
    Compose(hc::Agent),
}

/// A pick cliamp is loading, until it answers for that url.
struct Load {
    url: String,
    title: String,
}

struct App {
    cfg: Config,
    gitlab: Shared,
    writer: Arc<dyn Writer>,
    server: nvim::Server,
    tx: Sender<Msg>,
    /// Each () asks the fetcher thread for a fresh snapshot.
    fetches: Sender<()>,
    /// When the board next asks GitLab on its own.
    next_fetch: Instant,
    /// A fetch is out.
    fetching: bool,
    /// A write landed while a fetch was already out: that fetch's answer
    /// may be from before it, so one more is asked for.
    fetch_again: bool,
    /// When ot last wrote to GitLab, after which it asks more often (SOON).
    wrote: Option<Instant>,
    snap: Snapshot,
    sessions: Vec<claude::Session>,
    agents: Vec<hc::Agent>,
    playing: Option<music::Playing>,
    hills: ui::music::Hills,
    /// How wide the last draw was, which the bars are laid out for.
    width: usize,
    /// Images in conversations, once the terminal has said how it draws them.
    images: Option<sessionui::Images>,
    /// Seconds of seeking asked for and not sent yet, and when to send
    /// them: one seek, a moment after the last press, since each restarts
    /// the stream.
    seek: f64,
    seek_due: Option<Instant>,
    /// Where a seek is taking the track, from the press: the track it was
    /// on, the clock it should land on and when it was asked, shown until
    /// cliamp's clock gets there or a few seconds pass without it.
    sought: Option<(i64, f64, Instant)>,
    /// A pick on its way to cliamp, said on the music screen from the
    /// press until cliamp answers.
    loading: Option<Load>,
    board: Board,
    view: boardui::View,
    shown: Shown,
    error: Option<String>,
    note: Option<String>,
    /// What is being done to an MR, said on its row until GitLab's next
    /// answer shows how it came out.
    doing: HashMap<u64, Doing>,
    /// Rebases ready to push, each waiting on its row for enter or b to
    /// ask about it.
    prepared: HashMap<u64, rebase::Rebased>,
    /// Those done, waiting for that answer.
    settling: HashSet<u64>,
    /// The stacks you asked to land (land.rs), kept between runs.
    landings: Vec<land::Landing>,
    /// Who each MR was stacked on (model::remember), kept between runs.
    was_on: HashMap<u64, u64>,
    /// The MRs you hid from the board (h), kept across runs.
    hidden: HashSet<u64>,
    /// Tours read ahead (l then T), and ones already kept, by MR.
    tours: HashMap<u64, boardui::Toured>,
    /// This machine as last read, for the board's htop strip.
    stats: Option<stats::Stats>,
    /// What Claude has been asked to read this run, by the heads of the
    /// landing's MRs: once per version of a landing, so one it couldn't
    /// read waits for each deploy until a push or ot's next start.
    asked: HashSet<String>,
    /// The rebases ot has started on its own this run (due), by MR and its
    /// head then: once each, so one that fails or that you drop waits for a
    /// push to it, or b, not for the next refresh.
    tried: HashSet<(u64, String)>,
    /// The merges ot has sent on its own this run (land::Step::Merge), by
    /// MR and its head then: once each, so one GitLab refuses isn't sent
    /// again on every refresh.
    merges_sent: HashSet<(u64, String)>,
    /// Reviews opened so far, which keeps each open's files apart.
    reviews_opened: u64,
    /// Everyone in your Slack, by email, once it has answered.
    slack: Vec<String>,
    /// Everyone in the GitLab project, by username, once it has answered.
    members: Vec<String>,
    /// Your tickets, as Linear last said, kept between runs.
    tickets: Vec<model::Ticket>,
    /// Haiku's line on each agent on the board that stopped.
    briefs: brief::Briefs,
    /// What each row waited on you for when ot last looked (tell).
    told: BTreeMap<String, String>,
    /// The terminal has focus, as far as it has said.
    focused: bool,
    /// The thread finder, over whatever is up, while it is open.
    finder: Option<finder::Finder>,
    /// The thread list beside a conversation.
    side: Side,
    /// Each T3 thread's turns as last read, shown at once when you switch back to it.
    read: HashMap<String, Vec<claude::Turn>>,
    /// The MR panel beside a thread.
    panel: Panel,
    /// The last thing said in each T3 thread, for the thread list.
    said: HashMap<String, (bool, String)>,
    /// Each MR's diff as read, by its number, with the head it was read at.
    patches: HashMap<u64, (String, Result<String, String>)>,
    /// A reply being written in nvim, in a box over ot that has every key.
    popup: Option<(popup::Popup, compose::Compose)>,
}

/// The MR panel beside a thread: hidden by m, and where its diff is.
#[derive(Default)]
struct Panel {
    hidden: bool,
    view: ui::mr::View,
}

/// The thread list beside a conversation: hidden by b, and the thread its cursor
/// is on while it has the keys.
#[derive(Default)]
struct Side {
    hidden: bool,
    cursor: Option<String>,
}

impl App {
    /// The board on what was cached, and the threads that keep it current:
    /// the fetcher, asked by refresh(), and the server nvim's review talks to.
    fn new(
        cfg: Config,
        snap: Snapshot,
        gitlab: Shared,
        writer: Arc<dyn Writer>,
    ) -> (App, Receiver<Msg>) {
        let (tx, rx) = mpsc::channel();
        let fetches = fetcher(cfg.clone(), gitlab.clone(), tx.clone());
        let landings = cache::landings(&cfg.project);
        let was_on = model::remember(&cache::stacked(&cfg.project), &snap);
        let tickets = cache::tickets(&cfg.project);
        let hidden = cache::hidden(&cfg.project);
        let server = nvim::Server::new(writer.clone(), gitlab.clone());
        let mut app = App {
            cfg,
            gitlab,
            writer,
            server,
            tx,
            fetches,
            next_fetch: Instant::now(),
            fetching: false,
            fetch_again: false,
            wrote: None,
            snap,
            sessions: vec![],
            agents: vec![],
            playing: None,
            hills: ui::music::Hills::default(),
            width: 80,
            images: None,
            seek: 0.0,
            seek_due: None,
            sought: None,
            loading: None,
            board: Board::default(),
            view: boardui::View::default(),
            shown: Shown::Board,
            error: None,
            note: None,
            doing: HashMap::new(),
            prepared: HashMap::new(),
            settling: HashSet::new(),
            landings,
            was_on,
            hidden,
            tours: HashMap::new(),
            stats: None,
            asked: HashSet::new(),
            tried: HashSet::new(),
            merges_sent: HashSet::new(),
            reviews_opened: 0,
            slack: vec![],
            members: vec![],
            tickets,
            briefs: brief::Briefs::default(),
            told: BTreeMap::new(),
            focused: true,
            finder: None,
            side: Side::default(),
            read: HashMap::new(),
            panel: Panel::default(),
            said: HashMap::new(),
            patches: HashMap::new(),
            popup: None,
        };
        app.rebuild();
        app.view.top(&app.board);
        (app, rx)
    }

    fn rebuild(&mut self) {
        // A T3 thread in the shared checkout is on the branch of the MR it made.
        let agents: Vec<hc::Agent> = self
            .agents
            .iter()
            .cloned()
            .map(|mut a| {
                if a.branch.is_empty() {
                    if let Some(m) = t3::mr(&a.address, &self.cfg.project).and_then(|iid| self.snap.mr(iid)) {
                        a.branch = m.source.clone();
                    }
                }
                a
            })
            .collect();
        self.board = model::board_hiding(
            &self.snap,
            &self.cfg.rules,
            &self.was_on,
            &self.sessions,
            &agents,
            &self.tickets,
            jiff::Timestamp::now().as_second(),
            &self.hidden,
        );
        self.view.settle(&self.board);
    }

    /// Haiku reads each agent on the board that has stopped, off the UI
    /// thread, for the panel's line on where its work stands. The loop
    /// asks before each frame, never rebuild(), so tests start no Claude.
    fn brief(&mut self) {
        let run = cache::brief_dir(&self.cfg.project);
        for (worker, log) in self.briefs.due(&self.board, &self.sessions) {
            let (tx, run) = (self.tx.clone(), run.clone());
            std::thread::spawn(move || {
                let result = brief::read(&worker.name, &log, &run);
                let _ = tx.send(Msg::Brief { worker, result });
            });
        }
    }

    /// Without Haiku's line, the panel says what the agent said, as before.
    fn on_brief(&mut self, w: model::Worker, result: Result<String, String>) {
        let said = result.unwrap_or_else(|e| {
            self.note = Some(format!("Haiku couldn't read {}: {e}", w.name));
            w.doing.clone()
        });
        self.briefs.heard(&w, said);
    }

    /// A notification for each row that has come to wait on you since the
    /// last look, unless you are looking at ot. The loop asks before each
    /// frame, never rebuild(), so tests notify nobody.
    fn tell(&mut self) {
        let landing = land::said(&self.landings, &self.snap, &self.cfg.rules);
        let now = boardui::waits(&self.snap, &self.board, &self.doing, &landing);
        let news: Vec<String> = now
            .iter()
            .filter(|(row, what)| self.told.get(*row) != Some(*what))
            .map(|(row, what)| format!("{row}: {what}"))
            .collect();
        self.told = now;
        if !news.is_empty() && !self.focused {
            term::notify(&news.join("\n"));
        }
    }

    /// The screen the next key goes to, as the key log names it, and the
    /// places whose keys work there (keys.rs); None while a question or an
    /// input has the keys.
    fn places(&self) -> Option<(&'static str, Vec<keys::Place>)> {
        if self.finder.is_some() {
            return None;
        }
        match &self.shown {
            Shown::Music(view) => match view.search {
                musicui::Search::Typing(_) => None,
                musicui::Search::Hits { .. } => Some(("hits", vec![keys::Place::Hits])),
                _ => Some(("music", vec![keys::Place::Music])),
            },
            Shown::Session { .. } | Shown::Thread { .. } => {
                if !matches!(self.view.mode, Mode::Board) {
                    return None;
                }
                if self.side.cursor.is_some() {
                    return Some(("threads", vec![keys::Place::Side]));
                }
                if self.panel.view.focused && self.panel_shows() {
                    return Some(("mr", vec![keys::Place::MrFocus]));
                }
                let mut at = vec![keys::Place::Transcript];
                if self.reply_to().is_some() {
                    at.push(keys::Place::Reply);
                }
                if self.thread_mr().is_some() {
                    at.push(keys::Place::ThreadMr);
                }
                Some(("session", at))
            }
            Shown::Board => matches!(self.view.mode, Mode::Board).then(|| ("board", self.view.places(&self.snap, &self.board))),
        }
    }

    /// How long until the board asks GitLab again on its own.
    fn every(&self) -> Duration {
        if self.wrote.is_some_and(|at| at.elapsed() < SOON_FOR) {
            SOON
        } else {
            EVERY
        }
    }

    /// A write to `iid` landed: its row says so until GitLab's next answer,
    /// asked for now and often for a while.
    fn written(&mut self, iid: u64) {
        self.wrote = Some(Instant::now());
        self.settling.insert(iid);
        self.fetch_again |= self.fetching;
        self.refresh();
    }

    /// Asks GitLab, unless a fetch is already out, and puts the next ask off.
    fn refresh(&mut self) {
        self.next_fetch = Instant::now() + self.every();
        if !self.fetching {
            self.fetching = self.fetches.send(()).is_ok();
        }
    }

    fn on_fetched(&mut self, answer: Result<Snapshot, String>) {
        self.fetching = false;
        self.next_fetch = Instant::now() + self.every();
        match answer {
            Ok(snap) => {
                self.snap = snap;
                self.error = None;
                self.tours_kept();
                let was_on = model::remember(&self.was_on, &self.snap);
                if was_on != self.was_on {
                    self.was_on = was_on;
                    if let Err(e) = cache::save_stacked(&self.cfg.project, &self.was_on) {
                        self.note = Some(e);
                    }
                }
                let again = std::mem::take(&mut self.fetch_again);
                if again {
                    self.refresh();
                } else {
                    for iid in self.settling.drain() {
                        self.doing.remove(&iid);
                    }
                }
                self.rebuild();
                // Only on an answer from after every write landed.
                if !again {
                    self.land();
                }
            }
            Err(e) => self.error = Some(format!("{e}, trying again in {}s", self.every().as_secs())),
        }
    }

    /// Seeks `by` seconds, with whatever else is asked within 400ms.
    fn seek(&mut self, by: f64) {
        self.seek += by;
        self.seek_due = Some(Instant::now() + Duration::from_millis(400));
        if let Some(p) = &self.playing {
            let from = self.sought.map(|(_, at, _)| at).unwrap_or(p.position);
            self.sought = Some((p.index, (from + by).max(0.0), Instant::now()));
        }
    }

    /// The seek asked for, once its moment has come.
    fn send_seek(&mut self) {
        if self.seek_due.is_some_and(|due| Instant::now() >= due) {
            let by = std::mem::take(&mut self.seek);
            self.seek_due = None;
            if by != 0.0 {
                off_thread("cliamp", &["seek", &format!("{by}")], String::new());
            }
        }
    }

    /// A search came back: its hits to pick from, unless you have moved
    /// on, or why there are none.
    fn on_found(&mut self, query: &str, hits: Result<Vec<music::Hit>, String>) {
        let Shown::Music(view) = &mut self.shown else {
            return;
        };
        if !matches!(&view.search, musicui::Search::Searching(q) if q == query) {
            return;
        }
        match hits {
            Ok(hits) => view.search = musicui::Search::Hits { hits, at: 0 },
            Err(e) => {
                view.search = musicui::Search::Off;
                view.said = Some(e);
            }
        }
    }

    /// cliamp answered a pick: the wait for it is over, and the music
    /// screen says why when it was refused.
    fn on_played(&mut self, url: &str, result: Result<(), String>) {
        if self.loading.as_ref().is_some_and(|l| l.url == url) {
            self.loading = None;
        }
        if let (Shown::Music(view), Err(e)) = (&mut self.shown, result) {
            view.said = Some(e);
        }
    }

    /// A frame of music: the bars move on by the time since the last.
    fn on_music(&mut self, frame: music::Frame) {
        self.playing = frame.playing;
        // The seek has landed, the track moved on without it, or cliamp
        // had its say (a seek past the end lands nowhere near).
        if let (Some((index, at, asked)), Some(p)) = (self.sought, &self.playing) {
            if p.index != index
                || (p.position - at).abs() < 2.0
                || asked.elapsed() > Duration::from_secs(5)
            {
                self.sought = None;
            }
        }
        self.hills.tick(&frame.bands, self.width);
    }

    /// Claude's sessions changed. One being read that went off the board
    /// (quiet too long) takes the screen back to the board.
    fn on_sessions(&mut self, sessions: Vec<claude::Session>) {
        self.sessions = sessions;
        if let Shown::Session { id, .. } = &self.shown {
            if !self.sessions.iter().any(|s| s.id == *id) {
                self.shown = Shown::Board;
            }
        }
        self.rebuild();
    }

    /// Linear answered. Without an answer the board keeps the tickets it
    /// had, so a hiccup doesn't empty a project.
    fn on_tickets(&mut self, answer: Result<Vec<model::Ticket>, String>) {
        match answer {
            Ok(tickets) if tickets != self.tickets => {
                self.tickets = tickets;
                if let Err(e) = cache::save_tickets(&self.cfg.project, &self.tickets) {
                    self.note = Some(e);
                }
                self.rebuild();
            }
            Ok(_) => {}
            Err(e) => self.note = Some(e),
        }
    }

    fn on_wrote(&mut self, iid: u64, doing: &'static str, result: Result<(), String>) {
        match result {
            Ok(()) => self.written(iid),
            Err(e) => {
                self.doing.remove(&iid);
                self.note = Some(format!("{doing} {} failed: {e}", bang(iid)));
            }
        }
    }

    /// On GitLab's fresh answer, ot readies each rebase that is due, once,
    /// unless its MR already has one under way or waiting on you, and each
    /// landing ends once it is done or can't go on. Its merges wait for you
    /// (ask), but for one Claude said can go out with the one below, which
    /// ot sends itself, once.
    fn land(&mut self) {
        let unread: Vec<Vec<model::Mr>> = self.landings.iter().filter_map(|l| l.to_read(&self.snap)).collect();
        for layers in unread {
            let heads: Vec<&str> = layers.iter().map(|m| m.head.as_str()).collect();
            if self.asked.insert(heads.join(" ")) {
                self.judge(layers);
            }
        }
        for iid in self.due() {
            let head = self.snap.mr(iid).map(|m| m.head.clone()).unwrap_or_default();
            if !self.doing.contains_key(&iid) && self.tried.insert((iid, head)) {
                self.rebase(iid);
            }
        }
        let steps: Vec<land::Step> = self
            .landings
            .iter()
            .map(|l| l.next(&self.snap, &self.cfg.rules))
            .collect();
        let mut ended = vec![];
        for (i, step) in steps.into_iter().enumerate() {
            match step {
                land::Step::Done => ended.push((i, None)),
                land::Step::Stop(why) => ended.push((i, Some(why))),
                land::Step::Merge(iid) => {
                    let plan = self.snap.mr(iid).and_then(|m| Some((m.head.clone(), model::plan(Act::Merge, m)?)));
                    if let Some((head, plan)) = plan {
                        if !self.doing.contains_key(&iid) && self.merges_sent.insert((iid, head)) {
                            carry_out(self, iid, plan);
                        }
                    }
                }
                _ => {}
            }
        }
        for (i, why) in ended.into_iter().rev() {
            let l = self.unland(i);
            self.note = Some(match why {
                None => format!("landed {}", l.names()),
                Some(why) => format!("stopped landing {}: {why}", l.names()),
            });
        }
    }

    /// Claude reads a landing's MRs, off the UI thread, for which can go
    /// out in the same deploy as the one below.
    fn judge(&mut self, layers: Vec<model::Mr>) {
        let Some(top) = layers.last().map(|m| m.iid) else {
            return;
        };
        // Each it reads says so, unless something else is under way there.
        let mut read = vec![];
        for pair in layers.windows(2).filter(|p| p[1].reach.is_none()) {
            let (b, m) = (pair[0].iid, pair[1].iid);
            read.push(m);
            self.doing.entry(m).or_insert(Doing {
                word: READING.into(),
                detail: format!("Claude is reading whether {} can go out in the same deploy as {}", bang(m), bang(b)),
                on_you: false,
            });
        }
        let (gitlab, tx) = (self.gitlab.clone(), self.tx.clone());
        let (project, main) = (self.cfg.project.clone(), self.snap.default_branch.clone());
        let run = cache::land_dir(&project).join(top.to_string());
        std::thread::spawn(move || {
            let result = gitlab
                .get()
                .and_then(|g| judge::together(Path::new("."), &g, &project, &main, &layers, &run));
            let _ = tx.send(Msg::Judged { read, result });
        });
    }

    /// Kept with its landing. Without it, each waits for the one below
    /// to be live, as a landing does anyway.
    fn on_judged(&mut self, read: Vec<u64>, result: Result<Vec<land::Verdict>, String>) {
        for i in &read {
            if self.doing.get(i).is_some_and(|d| d.word == READING) {
                self.doing.remove(i);
            }
        }
        let names: Vec<String> = read.iter().map(|&i| bang(i)).collect();
        match (result, read.first().and_then(|&i| self.landing_of(i))) {
            (Ok(verdicts), Some(i)) => {
                self.landings[i].judged(&self.snap, verdicts);
                self.keep_landings();
            }
            (Ok(_), None) => {}
            (Err(e), _) => {
                self.note = Some(format!("Claude couldn't read {}, so each waits for the one below to be live: {e}", names.join(", ")))
            }
        }
    }

    /// The landing `iid` is in, by its place.
    fn landing_of(&self, iid: u64) -> Option<usize> {
        self.landings.iter().position(|l| l.iids.contains(&iid))
    }

    /// The landing whose next step is to rebase `iid`, which can't go on
    /// past a rebase of it that failed or that you dropped.
    fn rebasing(&self, iid: u64) -> Option<usize> {
        let step = land::Step::Rebase(iid);
        self.landings.iter().position(|l| l.next(&self.snap, &self.cfg.rules) == step)
    }

    /// The landing at `i`, no longer landed.
    fn unland(&mut self, i: usize) -> land::Landing {
        let l = self.landings.remove(i);
        self.keep_landings();
        l
    }

    /// The rebases ot does without being asked: what a landing's next step
    /// is to rebase, then each MR of yours still with the commits of a
    /// merged one it sat on (Mr::carries), however that merged.
    fn due(&self) -> Vec<u64> {
        let mut all = vec![];
        for l in &self.landings {
            if let land::Step::Rebase(iid) = l.next(&self.snap, &self.cfg.rules) {
                all.push(iid);
            }
        }
        for m in self.snap.mrs.iter().filter(|m| m.reach.is_none() && m.carries.is_some()) {
            if !all.contains(&m.iid) {
                all.push(m.iid);
            }
        }
        all
    }

    /// A rebase is in place whose bottom layer left out the commits of the
    /// merged MR it sat on (Rebased::past).
    fn rebased(&mut self, past: Option<(u64, u64)>) {
        if let Some((iid, merged)) = past {
            for l in &mut self.landings {
                l.rebased(iid, merged);
            }
            self.keep_landings();
        }
    }

    fn keep_landings(&mut self) {
        if let Err(e) = cache::save_landings(&self.cfg.project, &self.landings) {
            self.note = Some(e);
        }
    }

    /// b: the MR's stack rebased here, off the UI thread, its row saying
    /// what that is at; what it will do comes back to ask about. Past the
    /// MR it sat on, if that merged, however it did (model::layers).
    fn rebase(&mut self, iid: u64) {
        let (layers, below) = model::layers(&self.snap, &self.was_on, iid);
        if layers.is_empty() {
            self.note = Some(format!("{} merged, so there is nothing of it to rebase; b on what was on it rebases that past it", bang(iid)));
            return;
        }
        let layers: Vec<model::Mr> = layers.into_iter().cloned().collect();
        let at = cache::settle_dir(&self.cfg.project);
        self.doing.insert(iid, Doing::word("rebasing"));
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let mut step = |word: &'static str, detail: &str| {
                let _ = tx.send(Msg::Step { iid, word, detail: detail.into() });
            };
            let result = rebase::prepare(Path::new("."), &layers, below, &at, &mut step);
            let _ = tx.send(Msg::Rebased { iid, result });
        });
    }

    /// Ready to ask, it never interrupts: the row says so, and enter or b
    /// there asks. One that is due goes on its own when it is only git's
    /// replay of your branches; you see first one where Claude settled
    /// conflicts, that pushes commits of yours GitLab hasn't seen, or that
    /// moves someone else's branch.
    fn on_rebased(&mut self, iid: u64, result: Result<rebase::Rebased, String>) {
        self.doing.remove(&iid);
        match result {
            Err(e) => {
                self.note = Some(match self.rebasing(iid) {
                    Some(i) => format!("stopped landing {}: {e}", self.unland(i).names()),
                    None => e,
                })
            }
            Ok(r) if !r.moves() => {
                self.rebased(r.past());
                self.note = Some(r.said.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" "))
            }
            Ok(ready) => {
                let yours = |l: &rebase::Layer| l.new == l.remote || self.snap.mr(l.iid).is_some_and(|m| m.author == self.snap.me);
                if self.due().contains(&iid) && ready.plain() && ready.layers.iter().all(yours) {
                    self.push(iid, ready)
                } else {
                    self.park(iid, ready)
                }
            }
        }
    }

    /// Asks about the MR's ready rebase, if it has one.
    fn ask(&mut self, iid: u64) -> bool {
        let Some(ready) = self.prepared.remove(&iid) else {
            return false;
        };
        self.doing.remove(&iid);
        self.view.mode = Mode::Rebase { iid, ready };
        true
    }

    /// Asks to merge the MR, if its landing says it is ready, or it is one
    /// ot merges itself, whose merge GitLab may have refused.
    fn ask_landing(&mut self, iid: u64) -> bool {
        let ready = self.landing_of(iid).is_some_and(|i| {
            let step = self.landings[i].next(&self.snap, &self.cfg.rules);
            step == land::Step::Ready(iid) || step == land::Step::Merge(iid)
        });
        let Some(plan) = self.snap.mr(iid).and_then(|m| model::plan(Act::Merge, m)).filter(|_| ready) else {
            return false;
        };
        self.view.mode = Mode::Confirm { iid, plan };
        true
    }

    fn park(&mut self, iid: u64, ready: rebase::Rebased) {
        self.prepared.insert(iid, ready);
        self.doing.insert(
            iid,
            Doing {
                word: "rebase ready".into(),
                detail: "the rebase is ready to push: enter or b shows what it will do and asks".into(),
                on_you: true,
            },
        );
    }

    /// y on a prepared rebase: pushed, the local branches following.
    fn push(&mut self, iid: u64, ready: rebase::Rebased) {
        self.doing.insert(iid, Doing::word("pushing"));
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let mut step = |word: &'static str, detail: &str| {
                let _ = tx.send(Msg::Step { iid, word, detail: detail.into() });
            };
            let result = rebase::push(Path::new("."), &ready, &mut step);
            let _ = tx.send(Msg::Pushed { iid, past: ready.past(), result });
        });
    }

    /// The input of who an MR's description cc's opens on who it does,
    /// unless you have gone into something else meanwhile.
    fn on_described(&mut self, iid: u64, result: Result<String, String>) {
        match result {
            Ok(description) => {
                if matches!((&self.shown, &self.view.mode), (Shown::Board, Mode::Board)) {
                    let ccs = model::ccs(&description);
                    let text = if ccs.is_empty() { String::new() } else { format!("{} ", ccs.join(" ")) };
                    let sets = Sets::Cc { description };
                    self.view.mode = Mode::Typing(Typing { iid, text, cycling: None, sets });
                }
            }
            Err(e) => self.note = Some(format!("reading {}'s description failed: {e}", bang(iid))),
        }
    }

    /// The review checkout, off this thread and once nothing else is at it
    /// (a push nvim started, still going), after `work` runs there for an
    /// MR, its row saying `doing` meanwhile; what of yours it holds then
    /// comes back to on_checkout.
    fn checkout(&mut self, work: Option<(u64, &'static str, Work)>) {
        if let Some((iid, doing, _)) = &work {
            self.doing.insert(*iid, Doing::word(doing));
        }
        // Yours, by the head GitLab last said, or the one ot last pushed
        // or fetched for it, which a board not refreshed yet hasn't seen.
        let mine: Vec<(u64, String, String)> = self.snap.mrs.iter().filter(|m| m.author == self.snap.me).map(|m| (m.iid, m.source.clone(), m.head.clone())).collect();
        let (tx, tree) = (self.tx.clone(), cache::review_tree(&self.cfg.project));
        std::thread::spawn(move || {
            let _one = worktree::CHECKOUT.lock().unwrap_or_else(|e| e.into_inner());
            let went = work.map(|(iid, _, w)| (iid, w(&tree)));
            let change = worktree::change(&tree);
            let owner = change.as_ref().and_then(|c| {
                let on = |(_, branch, head): &&(u64, String, String)| *head == c.on || git::rev(&tree, &format!("refs/remotes/origin/{branch}")).as_ref() == Some(&c.on);
                mine.iter().find(on).map(|(iid, branch, _)| (*iid, branch.clone()))
            });
            let _ = tx.send(Msg::Checkout { went, change, owner });
        });
    }

    /// Your change in the review checkout asked about on the board, with
    /// why the last try didn't go; or said, while the board is busy.
    fn on_checkout(&mut self, went: Option<(u64, Result<String, String>)>, change: Option<worktree::Change>, owner: Option<(u64, String)>) {
        let mut why = None;
        if let Some((iid, result)) = went {
            self.doing.remove(&iid);
            match result {
                Ok(said) => {
                    self.note = Some(said);
                    self.wrote = Some(Instant::now());
                    self.refresh();
                }
                Err(e) => why = Some(e),
            }
        }
        let Some(change) = change else {
            self.note = why.or(self.note.take());
            return;
        };
        let free = matches!((&self.shown, &self.view.mode), (Shown::Board, Mode::Board));
        self.note = Some(match owner {
            Some((iid, branch)) if free => {
                self.view.mode = Mode::Unpushed(Left { iid, branch, change, why });
                return;
            }
            Some((iid, _)) => format!("your change to {} is still in the review checkout; leave a review and ot asks about it", bang(iid)),
            None => format!(
                "the review checkout holds edits on none of your MRs ({}), so it can't move to another MR: {}",
                change.files.join(", "),
                cache::review_tree(&self.cfg.project).display()
            ),
        });
    }

    fn on_pushed(&mut self, iid: u64, past: Option<(u64, u64)>, result: Result<Vec<String>, String>) {
        match result {
            Ok(left) => {
                for l in &mut self.landings {
                    l.pushed(iid);
                }
                self.keep_landings();
                self.rebased(past);
                self.written(iid);
                if !left.is_empty() {
                    self.note = Some(format!("rebased and pushed, but {}", left.join("; ")));
                }
            }
            Err(e) => {
                self.doing.remove(&iid);
                self.note = Some(e);
            }
        }
    }

    fn stamp(&self) -> String {
        let at = (self.snap.as_of > 0).then(|| {
            let t = jiff::Timestamp::from_second(self.snap.as_of)
                .unwrap_or_default()
                .to_zoned(jiff::tz::TimeZone::system());
            format!("as of {}", t.strftime("%H:%M"))
        });
        match (at, self.fetching) {
            (Some(at), true) => format!("{at}  ·  asking GitLab…"),
            (Some(at), false) => at,
            (None, true) => "asking GitLab…".into(),
            (None, false) => String::new(),
        }
    }

    /// Claude's tour of `iid`, read in the background so the review opens on it
    /// at once; the board says how it comes along.
    fn read_tour(&mut self, iid: u64) {
        let Some(mr) = self.snap.mr(iid).cloned() else { return };
        match self.tours.get(&iid) {
            Some(boardui::Toured::Ready) => {
                self.note = Some(format!("the tour of {} is ready; enter opens it", model::bang(iid)));
                return;
            }
            Some(boardui::Toured::Reading(_)) => return,
            _ => {}
        }
        self.tours.insert(iid, boardui::Toured::Reading(0));
        self.note = Some(format!("reading the tour of {} in the background", model::bang(iid)));
        let (tx, project, gitlab) = (self.tx.clone(), self.cfg.project.clone(), self.gitlab.clone());
        std::thread::spawn(move || {
            let head = mr.head.clone();
            let send = |state| {
                let _ = tx.send(Msg::Toured { iid, head: head.clone(), state });
            };
            let have = if git::have(&[&mr.base, &mr.head]) { Ok(()) } else { git::fetch_mr(mr.iid) };
            let read = have
                .and_then(|()| git::sides(&mr.base, &mr.head))
                .map(|sides| review::review(&sides))
                .and_then(|r| {
                    let description = gitlab.get().and_then(|g| g.description(&project, mr.iid))?;
                    tour::read(&mr, &description, &r, &tour::run_dir(&project, &mr), &mut |t| send(boardui::Toured::Reading(t.steps.len())))
                });
            send(match read {
                Ok(_) => boardui::Toured::Ready,
                Err(e) => boardui::Toured::Failed(e),
            });
        });
    }

    /// Which MRs already have their tour kept, so the board says so after a restart.
    fn tours_kept(&mut self) {
        let project = self.cfg.project.clone();
        self.tours.retain(|iid, t| *t != boardui::Toured::Ready || self.snap.mr(*iid).is_some_and(|m| tour::told(&tour::run_dir(&project, m))));
        for m in &self.snap.mrs {
            if !self.tours.contains_key(&m.iid) && m.reach.is_none() && tour::told(&tour::run_dir(&project, m)) {
                self.tours.insert(m.iid, boardui::Toured::Ready);
            }
        }
    }

    /// Hides the picked MR's stack from the board, or brings back a hidden one.
    fn hide(&mut self) {
        let Some(iid) = self.picked().map(|m| m.iid) else { return };
        let all = model::stack_of(&self.snap, &self.was_on, iid);
        let back = all.iter().all(|i| self.hidden.contains(i));
        let what = if all.len() > 1 { format!("{} and its stack", model::bang(iid)) } else { model::bang(iid) };
        if back {
            all.iter().for_each(|i| {
                self.hidden.remove(i);
            });
            self.note = Some(format!("{what} back on the board"));
        } else {
            self.hidden.extend(&all);
            self.note = Some(format!("{what} hidden; H shows what you hid"));
        }
        // What has left GitLab's list is forgotten, so the file stays small.
        if !self.snap.mrs.is_empty() {
            let open: HashSet<u64> = self.snap.mrs.iter().map(|m| m.iid).collect();
            self.hidden.retain(|i| open.contains(i));
        }
        if let Err(e) = cache::save_hidden(&self.cfg.project, &self.hidden) {
            self.note = Some(format!("can't keep what you hid: {e}"));
        }
        self.rebuild();
    }

    fn picked(&self) -> Option<&model::Mr> {
        match self.view.at {
            Some(Pick::Mr(iid)) => self.snap.mr(iid),
            _ => None,
        }
    }

    /// The T3 thread what is being read takes a reply in: the one shown, or
    /// the one whose claude the session shown is.
    fn reply_to(&self) -> Option<hc::Agent> {
        match &self.shown {
            Shown::Thread { agent, .. } => Some(agent.clone()),
            Shown::Session { id, agent: None, .. } => {
                let s = self.sessions.iter().find(|s| s.id == *id)?;
                self.agents.iter().find(|a| t3::owns(&a.address) && a.pid.is_some() && a.pid == s.pid).cloned()
            }
            _ => None,
        }
    }

    /// The review of `mr` in nvim, on Claude's tour if `tour`.
    fn review(&mut self, mr: &model::Mr, tour: bool) -> Next {
        self.reviews_opened += 1;
        Next::Review(nvim::start(&self.server, &self.cfg.project, mr, &self.snap.me, self.gitlab.clone(), self.reviews_opened, tour))
    }

    /// Your open MRs, for the finder.
    fn open_finder(&mut self) {
        let mine = self.snap.mrs.iter().filter(|m| m.author == self.snap.me && m.reach.is_none()).cloned().collect();
        self.finder = Some(finder::Finder::open(mine));
    }

    /// Shows a T3 thread, read through T3; what it said when last read until then.
    fn open_thread(&mut self, agent: hc::Agent) {
        if let Some(i) = self.images.as_mut() {
            i.forget();
        }
        let turns = self.read.get(&agent.address).cloned().unwrap_or_default();
        self.panel.view = ui::mr::View::default();
        self.shown = Shown::Thread { agent, turns, view: sessionui::View::at_end() };
        self.load_thread();
    }

    /// What was last said in each T3 thread, for the thread list, from T3's database.
    fn read_said(&mut self) {
        let ids: Vec<&str> = self.agents.iter().filter(|a| t3::owns(&a.address)).map(|a| a.address.as_str()).collect();
        self.said = finder::latest(&ids);
    }

    /// The MR each thread in the list is on, as "!78877".
    fn side_mrs(&self, threads: &[hc::Agent]) -> HashMap<String, String> {
        threads
            .iter()
            .filter_map(|a| {
                let opened = t3::mr(&a.address, &self.cfg.project).and_then(|iid| self.snap.mr(iid));
                let m = opened.or_else(|| self.snap.mrs.iter().find(|m| !a.branch.is_empty() && m.source == a.branch && m.reach.is_none()))?;
                Some((a.address.clone(), format!("!{}", m.iid)))
            })
            .collect()
    }

    /// Whether the MR panel is drawn beside the conversation now.
    fn panel_shows(&self) -> bool {
        !self.panel.hidden && self.width >= 140 && self.thread_mr().is_some()
    }

    /// The MR of the thread being read: the one it opened, else the one its own branch is;
    /// for a session of Claude's, its branch's.
    fn thread_mr(&self) -> Option<&model::Mr> {
        let Some(a) = self.reply_to() else {
            // A session of Claude's: the open MR its checkout's branch is.
            let Shown::Session { id, .. } = &self.shown else { return None };
            let s = self.sessions.iter().find(|s| s.id == *id)?;
            return self.snap.mrs.iter().find(|m| !s.branch.is_empty() && m.source == s.branch && m.reach.is_none());
        };
        let opened = t3::mr(&a.address, &self.cfg.project).and_then(|iid| self.snap.mr(iid));
        opened.or_else(|| self.snap.mrs.iter().find(|m| !a.branch.is_empty() && m.source == a.branch && m.reach.is_none()))
    }

    /// Reads the diff of the thread's MR off the UI thread, once for each head it has.
    fn ask_patch(&mut self) {
        let Some(m) = self.thread_mr() else { return };
        if self.patches.get(&m.iid).is_some_and(|(head, _)| *head == m.head) {
            return;
        }
        let (iid, base, head, tx) = (m.iid, m.base.clone(), m.head.clone(), self.tx.clone());
        self.patches.insert(iid, (head.clone(), Err("reading the diff…".into())));
        std::thread::spawn(move || {
            let result = git::patch(iid, &base, &head);
            let _ = tx.send(Msg::Patch { iid, head, result });
        });
    }

    /// The T3 threads beside a conversation, newest first, the one being read among them.
    fn side_threads(&self) -> Vec<hc::Agent> {
        let mut threads: Vec<hc::Agent> = self.agents.iter().filter(|a| t3::owns(&a.address)).cloned().collect();
        if let Some(r) = self.reply_to() {
            if !threads.iter().any(|a| a.address == r.address) {
                threads.push(r);
            }
        }
        threads.sort_by_key(|a| std::cmp::Reverse(a.at));
        threads
    }

    /// Whether the thread list is drawn beside the conversation, `w` columns wide.
    fn side_shows(&self, w: u16) -> bool {
        (self.side.cursor.is_some() || !self.side.hidden && w >= ui::side::FROM) && self.agents.iter().any(|a| t3::owns(&a.address))
    }

    /// Reads the thread shown again, off the UI thread.
    fn load_thread(&self) {
        if let Shown::Thread { agent, .. } = &self.shown {
            let (tx, id) = (self.tx.clone(), agent.address.clone());
            std::thread::spawn(move || {
                let turns = t3::conversation(&id);
                tx.send(Msg::Thread { id, turns })
            });
        }
    }

    fn picked_agent(&self) -> Option<&hc::Agent> {
        let address = match self.view.item(&self.board)? {
            model::Item::Hc { address, .. } => address,
            model::Item::Claude { terminal, .. } => terminal.as_ref()?,
            _ => return None,
        };
        self.agents.iter().find(|a| a.address == *address)
    }

    fn picked_ticket(&self) -> Option<&model::Ticket> {
        match &self.view.at {
            Some(Pick::Ticket(id)) => self.tickets.iter().find(|t| t.id == *id),
            _ => None,
        }
    }
}

fn main() {
    if std::env::args().len() > 1 {
        eprintln!("usage: ot\n  run it in a checkout whose origin is a GitLab project");
        std::process::exit(2);
    }
    // Both git calls at once, since the first frame waits on them.
    let here = std::thread::spawn(git::branch);
    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ot: {e}");
            std::process::exit(1);
        }
    };
    let mut snap = cache::load(&cfg.project).unwrap_or_default();
    snap.here = here.join().unwrap_or_default();
    let gitlab = Shared::new(&cfg.host);
    let writer: Arc<dyn Writer> = Arc::new(gitlab::Live {
        gitlab: gitlab.clone(),
        project: cfg.project.clone(),
    });
    let (mut app, rx) = App::new(cfg, snap, gitlab, writer);
    // Builds seam's language rules now, so the first review doesn't wait.
    std::thread::spawn(review::rules);

    let mut term = ratatui::init();
    app.images = image_picker().map(sessionui::Images::new);
    // A late answer to the terminal query would read as keys; drop what is waiting.
    while event::poll(Duration::from_millis(150)).unwrap_or(false) {
        let _ = event::read();
    }
    let _ = execute!(std::io::stdout(), EnableFocusChange, EnableBracketedPaste);
    let result = run(&mut term, &mut app, rx);
    let _ = execute!(std::io::stdout(), DisableFocusChange, DisableBracketedPaste);
    ratatui::restore();
    let _ = std::fs::remove_dir_all(nvim::scratch());
    if let Err(e) = result {
        eprintln!("ot: {e}");
        std::process::exit(1);
    }
}

/// The input thread lends the terminal out (to nvim) on request: it says it
/// has parked, stops touching the terminal, polling included, since a poll
/// reads keys, and waits to be given it back.
struct Lend {
    parked: Sender<()>,
    back: Receiver<()>,
}

fn input(tx: Sender<Msg>, lend: Receiver<Lend>) {
    loop {
        if let Ok(l) = lend.try_recv() {
            let _ = l.parked.send(());
            let _ = l.back.recv();
            continue;
        }
        match event::poll(Duration::from_millis(50)) {
            Ok(true) => {
                let Ok(ev) = event::read() else { return };
                if tx.send(Msg::Term(ev)).is_err() {
                    return;
                }
            }
            Ok(false) => {}
            Err(_) => return,
        }
    }
}

fn run(
    term: &mut ratatui::DefaultTerminal,
    app: &mut App,
    rx: Receiver<Msg>,
) -> std::io::Result<()> {
    app.refresh();
    draw(term, app)?;
    let (lend, lent) = mpsc::channel();
    let tx = app.tx.clone();
    std::thread::spawn(move || input(tx, lent));
    watch_claude(app.tx.clone(), cache::review_tree(&app.cfg.project));
    let tx = app.tx.clone();
    stats::watch(move |st| tx.send(Msg::Stats(st)).is_ok());
    let tx = app.tx.clone();
    hc::watch(move |agents| {
        let _ = tx.send(Msg::Hc(agents));
    });
    // Read once a run: who works here changes slower than ot restarts.
    if let Some(token) = app.cfg.slack_token.clone() {
        let tx = app.tx.clone();
        std::thread::spawn(move || tx.send(Msg::Slack(slack::emails(&token))));
    }
    let (tx, gitlab, project) = (app.tx.clone(), app.gitlab.clone(), app.cfg.project.clone());
    std::thread::spawn(move || tx.send(Msg::Members(gitlab.get().and_then(|g| g.members(&project)))));
    let tx = app.tx.clone();
    music::feed(move |frame| {
        nvim::music(&frame);
        let _ = tx.send(Msg::Music(frame));
    });
    loop {
        let due = app
            .seek_due
            .map_or(app.next_fetch, |d| d.min(app.next_fetch));
        let wait = due.saturating_duration_since(Instant::now());
        let mut msgs: Vec<Msg> = match rx.recv_timeout(wait) {
            Ok(m) => vec![m],
            Err(RecvTimeoutError::Timeout) => vec![],
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        };
        // Everything already waiting is handled before the next frame, so a
        // burst of keys costs one draw.
        msgs.extend(std::iter::from_fn(|| rx.try_recv().ok()));
        if Instant::now() >= app.next_fetch {
            app.refresh();
        }
        app.send_seek();
        for m in msgs {
            match m {
                Msg::Fetched(answer) => app.on_fetched(answer),
                Msg::Claude(sessions) => app.on_sessions(sessions),
                Msg::Hc(agents) => {
                    if let Shown::Thread { agent, .. } = &mut app.shown {
                        if let Some(now) = agents.iter().find(|a| a.address == agent.address) {
                            let moved = now.at != agent.at || now.status != agent.status;
                            *agent = now.clone();
                            if moved {
                                app.load_thread();
                            }
                        }
                    }
                    app.agents = agents;
                    app.rebuild();
                    app.read_said();
                }
                Msg::Toured { iid, head, state } => {
                    if app.snap.mr(iid).is_some_and(|m| m.head == head) {
                        if let boardui::Toured::Failed(e) = &state {
                            app.note = Some(format!("no tour of {}: {e}", model::bang(iid)));
                        } else if state == boardui::Toured::Ready {
                            app.note = Some(format!("the tour of {} is ready; enter opens it", model::bang(iid)));
                        }
                        app.tours.insert(iid, state);
                    }
                }
                Msg::Stats(st) => app.stats = Some(st),
                Msg::Popup(false) => {}
                Msg::Popup(true) => {
                    if let Some((p, c)) = app.popup.take() {
                        p.close();
                        send_composed(app, c);
                    }
                }
                Msg::Term(Event::Key(k)) if app.popup.is_some() => {
                    if k.kind != KeyEventKind::Release {
                        if let Some((p, _)) = app.popup.as_mut() {
                            p.key(&k);
                        }
                    }
                }
                Msg::Term(Event::Paste(text)) if app.popup.is_some() => match (compose::dropped(&text), compose::mac()) {
                    (Some(paths), Some(mac)) => {
                        app.note = Some(format!("copying {} from the Mac…", if paths.len() == 1 { "a file".to_string() } else { format!("{} files", paths.len()) }));
                        let tx = app.tx.clone();
                        std::thread::spawn(move || {
                            let got: Result<Vec<_>, _> = paths.iter().map(|p| compose::fetch(&mac, p)).collect();
                            let _ = tx.send(Msg::Dropped { text, got: got.map(|g| g.len()) });
                        });
                    }
                    _ => {
                        if let Some((p, _)) = app.popup.as_mut() {
                            p.paste(&text);
                        }
                    }
                },
                Msg::Dropped { text, got } => match got {
                    Ok(n) => app.note = Some(format!("{n} file{} from the Mac attached", if n == 1 { "" } else { "s" })),
                    Err(e) => {
                        app.note = Some(format!("can't copy that from the Mac ({e}); its path went in instead"));
                        if let Some((p, _)) = app.popup.as_mut() {
                            p.paste(&text);
                        }
                    }
                },
                Msg::Started { text, result } => match result {
                    Ok(id) => {
                        app.note = None;
                        let name = text.lines().next().unwrap_or_default().chars().take(60).collect();
                        let now = jiff::Timestamp::now().as_second();
                        app.open_thread(hc::Agent { address: id, name, status: hc::Status::Working, said: String::new(), at: now, pid: None, branch: String::new() });
                    }
                    Err(e) => app.note = Some(format!("no thread started: {e}")),
                },
                Msg::Patch { iid, head, result } => {
                    app.patches.insert(iid, (head, result));
                }
                Msg::Thread { id, turns } => {
                    if let Ok(t) = &turns {
                        app.read.insert(id.clone(), t.clone());
                    }
                    if let Shown::Thread { agent, turns: shown, .. } = &mut app.shown {
                        if agent.address == id {
                            match turns {
                                Ok(t) => *shown = t,
                                Err(e) => app.note = Some(format!("can't read {}: {e}", agent.name)),
                            }
                        }
                    }
                }
                Msg::Found { query, hits } => app.on_found(&query, hits),
                Msg::Played { url, result } => app.on_played(&url, result),
                Msg::Music(frame) => app.on_music(frame),
                Msg::Wrote { iid, doing, result } => app.on_wrote(iid, doing, result),
                Msg::Step { iid, word, detail } => {
                    app.doing.insert(iid, Doing { word: word.into(), detail, on_you: false });
                }
                Msg::Rebased { iid, result } => app.on_rebased(iid, result),
                Msg::Pushed { iid, past, result } => app.on_pushed(iid, past, result),
                Msg::Judged { read, result } => app.on_judged(read, result),
                Msg::Brief { worker, result } => app.on_brief(worker, result),
                Msg::Asked(how) => {
                    app.note = Some(how);
                    app.load_thread();
                }
                Msg::Described { iid, result } => app.on_described(iid, result),
                Msg::Blurb { iid, result } => {
                    if let Mode::Blurb { iid: shown, text, .. } = &mut app.view.mode {
                        if *shown == iid {
                            *text = Some(result);
                        }
                    }
                }
                Msg::Slack(Ok(emails)) => app.slack = emails,
                Msg::Slack(Err(e)) => app.note = Some(format!("c can't complete from Slack: {e}")),
                Msg::Members(Ok(names)) => app.members = names,
                Msg::Members(Err(e)) => app.note = Some(format!("a completes only from the board: {e}")),
                Msg::Tickets(answer) => app.on_tickets(answer),
                Msg::Checkout { went, change, owner } => app.on_checkout(went, change, owner),
                Msg::Term(Event::FocusGained) => {
                    app.focused = true;
                    app.refresh();
                }
                Msg::Term(Event::FocusLost) => app.focused = false,
                Msg::Term(Event::Key(k)) if k.kind == KeyEventKind::Press => {
                    let name = keys::name(&k);
                    if let Some((screen, places)) = app.places() {
                        cache::pressed(screen, &name, keys::act(&name, &places));
                    }
                    match key(app, &name) {
                        Next::Stay => {}
                        Next::Quit => return Ok(()),
                        Next::Review(open) => review_in_nvim(term, &lend, app, open),
                        Next::Attach(agent) if t3::owns(&agent.address) => app.open_thread(agent),
                        Next::Compose(to) => compose_reply(term, app, to),
                        Next::Nvim(args) => {
                            let mut nvim = Command::new("nvim");
                            nvim.args(&args);
                            if let Err(e) = lend_terminal(term, &lend, nvim) {
                                app.note = Some(e);
                            }
                        }
                        Next::Shell(dir) => {
                            let mut sh = Command::new(std::env::var_os("SHELL").unwrap_or_else(|| "bash".into()));
                            sh.current_dir(&dir);
                            if let Err(e) = lend_terminal(term, &lend, sh) {
                                app.note = Some(e);
                            }
                        }
                        Next::Attach(agent) => match lend_terminal(term, &lend, hc::attach(&agent.address)) {
                            Ok(left) if left.success() => {}
                            Ok(_) => app.note = Some(format!("hc couldn't attach to {}", agent.name)),
                            Err(e) => app.note = Some(e),
                        },
                    }
                }
                Msg::Term(Event::Paste(text)) => pasted(app, &text),
                Msg::Term(_) => {}
            }
        }
        app.brief();
        app.tell();
        app.ask_patch();
        draw(term, app)?;
    }
}

/// The review, in nvim, which has the terminal until it quits. Then what
/// the review did (approved, commented) shows on the board.
fn review_in_nvim(
    term: &mut ratatui::DefaultTerminal,
    lend: &Sender<Lend>,
    app: &mut App,
    open: nvim::Open,
) {
    if let Err(e) = lend_terminal(term, lend, nvim::command(&open)) {
        app.note = Some(e);
    }
    app.server.end(open);
    // However nvim was left (:wq, a crash), a change of yours it leaves in
    // the checkout is asked about here.
    app.checkout(None);
    app.refresh();
}

/// A key goes to the screen that is up: to what it is asking or taking as
/// text, else as the act the key is there (keys.rs).
fn key(app: &mut App, name: &str) -> Next {
    app.note = None;
    if let Shown::Music(view) = &mut app.shown {
        view.said = None;
    }
    if app.finder.is_some() {
        return find_typed(app, name);
    }
    let Some((_, places)) = app.places() else {
        match &app.shown {
            Shown::Music(_) => search_typed(app, name),
            _ => answer(app, name),
        }
        return Next::Stay;
    };
    let Some(act) = keys::act(name, &places) else {
        return Next::Stay;
    };
    match &app.shown {
        Shown::Music(_) => music_act(app, act),
        Shown::Session { .. } | Shown::Thread { .. } if act == Act::Terminal => return terminal(app),
        Shown::Session { .. } | Shown::Thread { .. } if act == Act::Select => return select(app),
        Shown::Session { .. } | Shown::Thread { .. } if act == Act::Tell && app.side.cursor.is_none() => {
            if let Some(to) = app.reply_to() {
                return Next::Compose(to);
            }
        }
        Shown::Session { .. } | Shown::Thread { .. } if act == Act::Review && app.side.cursor.is_none() => {
            if let Some(mr) = app.thread_mr().cloned() {
                return app.review(&mr, false);
            }
        }
        Shown::Session { .. } | Shown::Thread { .. } => session_act(app, act),
        Shown::Board => return board_act(app, act),
    }
    Next::Stay
}

/// A reply written in nvim, in a box over ot, which has every key until nvim quits.
/// :w in it sends the reply.
fn compose_reply(term: &mut ratatui::DefaultTerminal, app: &mut App, to: hc::Agent) {
    let c = match compose::prepare(&to) {
        Ok(c) => c,
        Err(e) => {
            app.note = Some(format!("can't write a reply: {e}"));
            return;
        }
    };
    let mut cmd = portable_pty::CommandBuilder::new("nvim");
    cmd.args(compose::nvim_args(&c));
    if let Ok(cwd) = std::env::current_dir() {
        cmd.cwd(cwd);
    }
    let area = term.size().map(|s| ratatui::layout::Rect::new(0, 0, s.width, s.height)).unwrap_or_default();
    let tx = app.tx.clone();
    match popup::Popup::open(cmd, format!("reply to {}", to.name), area, move |done| {
        let _ = tx.send(Msg::Popup(done));
    }) {
        Ok(p) => app.popup = Some((p, c)),
        Err(e) => app.note = Some(format!("can't open nvim: {e}")),
    }
}

/// Sends what :w wrote, its images attached for a T3 thread; else the draft stays.
fn send_composed(app: &mut App, c: compose::Compose) {
    let Some((text, images)) = compose::written(&c) else {
        app.note = Some(format!("not sent; your draft to {} is kept for the next s", c.to.name));
        return;
    };
    let to = c.to.clone();
    app.note = Some(format!("sending to {}…", to.name));
    let tx = app.tx.clone();
    std::thread::spawn(move || {
        let sent = if !images.is_empty() && t3::owns(&to.address) {
            t3::send_with_images(&to.address, &text, &images, to.status == hc::Status::Working)
        } else if images.is_empty() {
            hc::send(&to.address, &text)
        } else {
            Err("images go only to T3 threads".into())
        };
        let how = match sent {
            Ok(()) => {
                compose::done(&c);
                let n = images.len();
                if n > 0 { format!("sent to {}, with {n} image{}", to.name, if n == 1 { "" } else { "s" }) } else { format!("sent to {}", to.name) }
            }
            Err(e) => format!("not sent to {}: {e}; your draft is kept", to.name),
        };
        let _ = tx.send(Msg::Asked(how));
    });
}

/// The conversation being read, in nvim to select and yank from, in this terminal
/// until nvim quits.
fn select(app: &mut App) -> Next {
    let (name, turns) = match &app.shown {
        Shown::Thread { agent, turns, .. } => (agent.name.clone(), turns.clone()),
        Shown::Session { id, agent: None, .. } => match app.sessions.iter().find(|s| s.id == *id) {
            Some(s) => (s.title.clone(), s.turns.clone()),
            None => return Next::Stay,
        },
        Shown::Session { id, agent: Some(a), .. } => match app.sessions.iter().find(|s| s.id == *id).and_then(|s| s.agent(a)) {
            Some(ag) => (a.clone(), ag.turns.clone()),
            None => return Next::Stay,
        },
        _ => return Next::Stay,
    };
    let args = match compose::reader(&name, &turns) {
        Ok(a) => a,
        Err(e) => {
            app.note = Some(format!("can't open the conversation in nvim: {e}"));
            return Next::Stay;
        }
    };
    Next::Nvim(args)
}

/// A shell where the session or thread being read works: a zellij pane beside ot
/// inside zellij, else this terminal until the shell exits.
fn terminal(app: &mut App) -> Next {
    let dir = match &app.shown {
        Shown::Session { id, .. } => app.sessions.iter().find(|s| s.id == *id).map(|s| s.cwd.clone()),
        Shown::Thread { agent, .. } => finder::cwd(&agent.address),
        _ => None,
    };
    let Some(dir) = dir.or_else(|| std::env::current_dir().ok()) else {
        app.note = Some("no folder to open a shell in".into());
        return Next::Stay;
    };
    if std::env::var_os("ZELLIJ").is_none() {
        return Next::Shell(dir);
    }
    let ran = Command::new("zellij").arg("action").arg("new-pane").arg("--cwd").arg(&dir).stdout(Stdio::null()).stderr(Stdio::piped()).output();
    app.note = Some(match ran {
        Ok(o) if o.status.success() => format!("a shell in {}, in a new zellij pane", dir.display()),
        Ok(o) => format!("zellij opened no pane: {}", String::from_utf8_lossy(&o.stderr).trim()),
        Err(e) => format!("zellij: {e}"),
    });
    Next::Stay
}

/// A key for the finder: moves, enter opens, esc closes, the rest is the query.
fn find_typed(app: &mut App, name: &str) -> Next {
    let Some(f) = app.finder.as_mut() else { return Next::Stay };
    match name {
        "esc" | "ctrl-c" => app.finder = None,
        "down" | "ctrl-n" | "ctrl-j" => f.step(1),
        "up" | "ctrl-p" | "ctrl-k" => f.step(-1),
        "ctrl-u" => {
            f.query.clear();
            f.search();
        }
        "enter" => {
            let t = match f.picked() {
                None => return Next::Stay,
                Some(finder::Picked::Mr(m)) => {
                    let iid = m.iid;
                    app.finder = None;
                    return app.snap.mr(iid).cloned().map_or(Next::Stay, |mr| app.review(&mr, false));
                }
                Some(finder::Picked::Thread(t)) => t.clone(),
            };
            app.finder = None;
            let known = app.agents.iter().find(|a| a.address == t.id).cloned();
            let agent = known.unwrap_or(hc::Agent {
                name: t.title,
                said: String::new(),
                status: hc::Status::Stopped,
                at: t.updated,
                pid: None,
                branch: t.branch,
                address: t.id,
            });
            app.open_thread(agent);
        }
        _ => f.typed(name),
    }
    Next::Stay
}

/// A paste goes into what you are typing as one line, and nowhere else:
/// taken as keys, its line ends would send what is typed and the rest
/// would act on the board.
fn pasted(app: &mut App, text: &str) {
    if let Some(f) = app.finder.as_mut() {
        f.query.push_str(&text.split_whitespace().collect::<Vec<_>>().join(" "));
        f.search();
        return;
    }
    let into = match (&mut app.shown, &mut app.view.mode) {
        (Shown::Music(view), _) => match &mut view.search {
            musicui::Search::Typing(typed) => typed,
            _ => return,
        },
        (_, Mode::Tell { text, .. } | Mode::Rename { text, .. } | Mode::NewThread(text)) => text,
        (Shown::Board, Mode::Note { text, .. } | Mode::Message(_, text)) => text,
        (Shown::Board, Mode::Typing(t)) => {
            t.cycling = None;
            &mut t.text
        }
        _ => return,
    };
    into.push_str(&text.split_whitespace().collect::<Vec<_>>().join(" "));
}

/// A key typed into the music screen's search.
fn search_typed(app: &mut App, name: &str) {
    let Shown::Music(view) = &mut app.shown else { return };
    let musicui::Search::Typing(text) = &mut view.search else { return };
    match name {
        "esc" => view.search = musicui::Search::Off,
        "enter" => {
            let query = text.trim().to_string();
            if !query.is_empty() {
                let tx = app.tx.clone();
                let q = query.clone();
                std::thread::spawn(move || {
                    let hits = music::search(&q);
                    let _ = tx.send(Msg::Found { query: q, hits });
                });
                view.search = musicui::Search::Searching(query);
            }
        }
        _ => {
            keys::edit(text, name);
        }
    }
}

/// The music screen's keys, and those of a search's results on it.
fn music_act(app: &mut App, act: Act) {
    let Shown::Music(view) = &mut app.shown else { return };
    match act {
        Act::Down => view.step(1),
        Act::Up => view.step(-1),
        Act::PlayNow | Act::PlayNext => {
            if let Some(hit) = view.picked() {
                let (url, tx) = (hit.url.clone(), app.tx.clone());
                let now = act == Act::PlayNow;
                app.loading = Some(Load {
                    url: url.clone(),
                    title: hit.title.clone(),
                });
                std::thread::spawn(move || {
                    let result = music::play(&url, now);
                    let _ = tx.send(Msg::Played { url, result });
                });
                view.search = musicui::Search::Off;
            }
        }
        // From a search's results, back to the player; from that, the board.
        Act::Leave if matches!(view.search, musicui::Search::Hits { .. }) => view.search = musicui::Search::Off,
        Act::Leave => app.shown = Shown::Board,
        Act::PlayPause => off_thread("cliamp", &["toggle"], String::new()),
        Act::NextTrack => off_thread("cliamp", &["next"], String::new()),
        Act::PrevTrack => off_thread("cliamp", &["prev"], String::new()),
        Act::Shuffle => off_thread("cliamp", &["shuffle"], String::new()),
        Act::Louder => volume(2),
        Act::Softer => volume(-2),
        Act::SeekForward => app.seek(10.0),
        Act::SeekBack => app.seek(-10.0),
        Act::Search => view.search = musicui::Search::Typing(String::new()),
        _ => {}
    }
}

/// The keys of the thread list beside a conversation, while it has them.
fn side_act(app: &mut App, act: Act) {
    let threads = app.side_threads();
    let at = threads.iter().position(|a| Some(&a.address) == app.side.cursor.as_ref()).unwrap_or(0);
    match act {
        Act::Down | Act::Up if !threads.is_empty() => {
            let by = if act == Act::Down { 1 } else { -1 };
            let next = (at as isize + by).clamp(0, threads.len() as isize - 1) as usize;
            app.side.cursor = Some(threads[next].address.clone());
        }
        Act::Into => {
            app.side.cursor = None;
            if let Some(a) = threads.get(at) {
                if app.reply_to().is_none_or(|r| r.address != a.address) {
                    app.open_thread(a.clone());
                }
            }
        }
        Act::Threads | Act::FocusRight => app.side.cursor = None,
        Act::NewThread => {
            app.side.cursor = None;
            app.view.mode = Mode::NewThread(String::new());
        }
        Act::Rename => {
            if let Some(a) = threads.get(at) {
                app.view.mode = Mode::Rename { text: a.name.clone(), to: a.clone() };
            }
        }
        Act::Leave => {
            app.side.cursor = None;
            app.shown = Shown::Board;
        }
        _ => {}
    }
}

/// The keys of a session being read.
fn session_act(app: &mut App, act: Act) {
    if app.side.cursor.is_some() {
        return side_act(app, act);
    }
    if app.panel.view.focused && !app.panel_shows() {
        app.panel.view.focused = false;
    }
    if app.panel.view.focused {
        let v = &mut app.panel.view;
        match act {
            Act::Down => v.step(1),
            Act::Up => v.step(-1),
            Act::PageDown => v.page(1),
            Act::PageUp => v.page(-1),
            Act::Top => v.scroll = 0,
            Act::Bottom => v.scroll = usize::MAX,
            Act::FocusLeft => v.focused = false,
            Act::Leave => {
                v.focused = false;
                app.shown = Shown::Board;
            }
            _ => {}
        }
        return;
    }
    match act {
        Act::Threads => {
            let threads = app.side_threads();
            if threads.is_empty() {
                app.note = Some("no T3 threads to switch to".into());
                return;
            }
            app.side.hidden = false;
            let reading = app.reply_to().map(|a| a.address);
            app.side.cursor = reading.filter(|r| threads.iter().any(|a| a.address == *r)).or(Some(threads[0].address.clone()));
            return;
        }
        Act::Sidebar => {
            app.side.hidden = !app.side.hidden;
            return;
        }
        Act::MrPanel => {
            app.panel.hidden = !app.panel.hidden;
            return;
        }
        Act::DiffDown | Act::DiffUp => {
            app.panel.view.page(if act == Act::DiffDown { 2 } else { -2 });
            return;
        }
        Act::FocusRight => {
            if app.panel_shows() {
                app.panel.view.focused = true;
            }
            return;
        }
        Act::FocusLeft => {
            if !app.side_threads().is_empty() {
                return session_act(app, Act::Threads);
            }
            return;
        }
        Act::NextThread | Act::PrevThread => {
            let threads = app.side_threads();
            let reading = app.reply_to().map(|a| a.address);
            let at = threads.iter().position(|a| Some(&a.address) == reading.as_ref());
            let next = match (at, act == Act::NextThread) {
                (Some(i), true) => i + 1,
                (Some(i), false) => i.wrapping_sub(1),
                (None, _) => 0,
            };
            if let Some(a) = threads.get(next) {
                app.open_thread(a.clone());
            }
            return;
        }
        Act::Tell => {
            if let Some(to) = app.reply_to() {
                app.view.mode = Mode::Tell { to, text: String::new() };
            }
            return;
        }
        Act::Web => {
            if let Some(link) = app.reply_to().and_then(|a| t3::link(&a.address)) {
                app.note = term::open(&link);
            }
            return;
        }
        Act::Stop => {
            if let Some(to) = app.reply_to() {
                app.view.mode = Mode::Stop(to);
            }
            return;
        }
        Act::Settle => {
            if let Some(a) = app.reply_to() {
                settle(app, a);
                app.shown = Shown::Board;
            }
            return;
        }
        Act::Find => {
            app.open_finder();
            return;
        }
        Act::Rename => {
            if let Some(a) = app.reply_to() {
                app.view.mode = Mode::Rename { text: a.name.clone(), to: a };
            }
            return;
        }
        Act::NewThread => {
            app.view.mode = Mode::NewThread(String::new());
            return;
        }
        _ => {}
    }
    let (Shown::Session { view, .. } | Shown::Thread { view, .. }) = &mut app.shown else { return };
    match act {
        Act::Down => view.step(1),
        Act::Up => view.step(-1),
        Act::PageDown => view.page(1),
        Act::PageUp => view.page(-1),
        Act::Top => view.top(),
        Act::Bottom => view.bottom(),
        Act::Leave => app.shown = Shown::Board,
        _ => {}
    }
}

/// A key for what the board is asking or taking as text.
fn answer(app: &mut App, name: &str) {
    match std::mem::take(&mut app.view.mode) {
        Mode::Board => {}
        // Any key leaves the keys' screen.
        Mode::Help => {}
        // A question waits for its answer: y for yes, n or esc for no. Any
        // other key is not one, so a stray one can't throw the work away.
        Mode::Confirm { iid, plan } => {
            match name {
                "y" => carry_out(app, iid, plan),
                "n" | "esc" => {}
                _ => app.view.mode = Mode::Confirm { iid, plan },
            }
        }
        // esc keeps it for later, on its row; n lets it go, and the
        // landing waiting on it.
        Mode::Rebase { iid, ready } => {
            match name {
                "y" => app.push(iid, ready),
                "n" => {
                    if let Some(i) = app.rebasing(iid) {
                        let l = app.unland(i);
                        app.note = Some(format!("stopped landing {}: you dropped {}'s rebase", l.names(), bang(iid)));
                    }
                }
                "esc" => app.park(iid, ready),
                _ => app.view.mode = Mode::Rebase { iid, ready },
            }
        }
        // On y it waits for GitLab's next answer to take its first step.
        Mode::Land(l) => {
            match name {
                "y" => {
                    app.landings.push(l);
                    app.keep_landings();
                    app.refresh();
                }
                "n" | "esc" => {}
                _ => app.view.mode = Mode::Land(l),
            }
        }
        // Approving asks: y as it is, m with a note first.
        Mode::Approve { iid } => {
            match name {
                "y" => {
                    if let Some(plan) = app.snap.mr(iid).map(|mr| model::approve(mr, "")) {
                        carry_out(app, iid, plan);
                    }
                }
                "m" => {
                    app.view.mode = Mode::Note {
                        iid,
                        text: String::new(),
                    }
                }
                "n" | "esc" => {}
                _ => app.view.mode = Mode::Approve { iid },
            }
        }
        Mode::Note { iid, mut text } => {
            match name {
                "esc" => {}
                "enter" => {
                    if let Some(plan) = app.snap.mr(iid).map(|mr| model::approve(mr, &text)) {
                        carry_out(app, iid, plan);
                    }
                }
                _ => {
                    keys::edit(&mut text, name);
                    app.view.mode = Mode::Note { iid, text };
                }
            }
        }
        // Your change in the review checkout: a amends the commit it is on,
        // c takes a message first, d puts it in git's stash, esc keeps it.
        Mode::Unpushed(l) => {
            match name {
                "a" => {
                    let Left { iid, branch, change, .. } = l;
                    app.checkout(Some((iid, "pushing", Box::new(move |tree| worktree::push(tree, &branch, &change.on, worktree::Commit::Amend)))));
                }
                "c" => app.view.mode = Mode::Message(l, String::new()),
                "d" => app.checkout(Some((l.iid, "putting aside", Box::new(worktree::put_aside)))),
                "esc" => app.note = Some(format!("your change to {} stays in the review checkout, which can't move to another MR until it goes", bang(l.iid))),
                _ => app.view.mode = Mode::Unpushed(l),
            }
        }
        Mode::Message(l, mut text) => {
            match name {
                "esc" => app.view.mode = Mode::Unpushed(l),
                "enter" => {
                    let Left { iid, branch, change, .. } = l;
                    app.checkout(Some((iid, "pushing", Box::new(move |tree| worktree::push(tree, &branch, &change.on, worktree::Commit::New(text))))));
                }
                _ => {
                    keys::edit(&mut text, name);
                    app.view.mode = Mode::Message(l, text);
                }
            }
        }
        Mode::Typing(t) => app.view.mode = typing(app, t, name),
        Mode::Tell { to, mut text } => {
            match name {
                "esc" => {}
                "enter" if text.trim().is_empty() => {}
                "enter" => {
                    app.note = Some(format!("sending to {}, once nobody is typing there…", to.name));
                    let tx = app.tx.clone();
                    std::thread::spawn(move || {
                        let how = match hc::send(&to.address, &text) {
                            Ok(()) => format!("sent to {}", to.name),
                            Err(e) => format!("not sent to {}: {e}", to.name),
                        };
                        tx.send(Msg::Asked(how))
                    });
                }
                _ => {
                    keys::edit(&mut text, name);
                    app.view.mode = Mode::Tell { to, text };
                }
            }
        }
        Mode::Blurb { iid, text, mut scroll } => {
            match name {
                "esc" | "q" | "l" | "enter" => return,
                "j" | "down" => scroll += 1,
                "k" | "up" => scroll = scroll.saturating_sub(1),
                "ctrl-d" | "space" => scroll += 10,
                "ctrl-u" => scroll = scroll.saturating_sub(10),
                "g" => scroll = 0,
                "G" => scroll = usize::MAX / 2,
                "w" => {
                    if let Some(m) = app.snap.mr(iid) {
                        app.note = term::open(&m.url);
                    }
                }
                "T" => {
                    app.read_tour(iid);
                    return;
                }
                "y" => {
                    if let Some(m) = app.snap.mr(iid) {
                        term::copy(&m.url);
                        app.note = Some(format!("copied {}", m.url));
                    }
                }
                _ => {}
            }
            app.view.mode = Mode::Blurb { iid, text, scroll };
        }
        Mode::Rename { to, mut text } => {
            match name {
                "esc" => {}
                "enter" if text.trim().is_empty() => app.view.mode = Mode::Rename { to, text },
                "enter" => {
                    let title = text.trim().to_string();
                    app.note = Some(format!("renaming {}…", to.name));
                    let (tx, address) = (app.tx.clone(), to.address.clone());
                    std::thread::spawn(move || {
                        let how = match t3::rename(&to.address, &title) {
                            Ok(()) => format!("renamed {title}"),
                            Err(e) => format!("{} not renamed: {e}", to.name),
                        };
                        let _ = tx.send(Msg::Asked(how));
                    });
                    if let Shown::Thread { agent, .. } = &mut app.shown {
                        if agent.address == address {
                            agent.name = text.trim().to_string();
                        }
                    }
                }
                _ => {
                    keys::edit(&mut text, name);
                    app.view.mode = Mode::Rename { to, text };
                }
            }
        }
        Mode::NewThread(mut text) => {
            match name {
                "esc" => {}
                "enter" if text.trim().is_empty() => app.view.mode = Mode::NewThread(text),
                "enter" => {
                    app.note = Some("starting a T3 thread…".into());
                    let tx = app.tx.clone();
                    std::thread::spawn(move || {
                        let _ = tx.send(Msg::Started { text: text.clone(), result: t3::start(&text) });
                    });
                }
                _ => {
                    keys::edit(&mut text, name);
                    app.view.mode = Mode::NewThread(text);
                }
            }
        }
        Mode::Stop(to) => {
            match name {
                "y" => {
                    app.note = Some(format!("stopping {}…", to.name));
                    let tx = app.tx.clone();
                    std::thread::spawn(move || {
                        let how = match hc::interrupt(&to.address) {
                            Ok(true) => format!("{} stopped", to.name),
                            Ok(false) => format!("{} is still working", to.name),
                            Err(e) => format!("{} not stopped: {e}", to.name),
                        };
                        tx.send(Msg::Asked(how))
                    });
                }
                "n" | "esc" => {}
                _ => app.view.mode = Mode::Stop(to),
            }
        }
    }
}

/// The board's keys on the row the cursor is on.
fn board_act(app: &mut App, act: Act) -> Next {
    let b = &app.board;
    match act {
        Act::Down => app.view.step(b, 1),
        Act::Up => app.view.step(b, -1),
        Act::Top => app.view.top(b),
        Act::Bottom => app.view.bottom(b),
        Act::Unfold => app.view.fold_open = true,
        Act::Fold => app.view.fold_open = false,
        Act::ShowHidden => {
            app.view.hidden_open = !app.view.hidden_open;
            if app.view.hidden_open && app.board.hidden.is_empty() {
                app.note = Some("nothing hidden; h on an MR hides it".into());
            }
            app.view.settle(&app.board);
        }
        Act::Hide => app.hide(),
        Act::ReadTour => {
            if let Some(iid) = app.picked().map(|m| m.iid) {
                app.read_tour(iid);
            }
        }
        // On a row whose rebase is ready, or that its landing says is
        // ready to merge, enter asks about that instead.
        Act::Review | Act::Tour => {
            if act == Act::Review && app.picked().map(|mr| mr.iid).is_some_and(|iid| app.ask(iid) || app.ask_landing(iid)) {
                return Next::Stay;
            }
            if let Some(mr) = app.picked().cloned() {
                let ready = app.tours.get(&mr.iid) == Some(&boardui::Toured::Ready);
                return app.review(&mr, act == Act::Tour || ready);
            }
        }
        Act::Music => app.shown = Shown::Music(musicui::View::default()),
        Act::Find => app.open_finder(),
        Act::Read => {
            if let Some(Pick::Session(id)) = &app.view.at {
                if let Some(i) = app.images.as_mut() {
                    i.forget();
                }
app.shown = Shown::Session {
                    id: id.clone(),
                    agent: None,
                    view: sessionui::View::at_end(),
                };
            }
        }
        Act::Attach => {
            if let Some(a) = app.picked_agent() {
                return Next::Attach(a.clone());
            }
        }
        Act::Tell => {
            if let Some(a) = app.picked_agent() {
                return Next::Compose(a.clone());
            }
        }
        Act::Stop => {
            if let Some(a) = app.picked_agent() {
                app.view.mode = Mode::Stop(a.clone());
            }
        }
        Act::Settle => {
            if let Some(a) = app.picked_agent().filter(|a| t3::owns(&a.address)).cloned() {
                settle(app, a);
            }
        }
        Act::Rename => {
            if let Some(a) = app.picked_agent().filter(|a| t3::owns(&a.address)).cloned() {
                app.view.mode = Mode::Rename { text: a.name.clone(), to: a };
            }
        }
        Act::NewThread => app.view.mode = Mode::NewThread(String::new()),
        Act::Agent => {
            let agent = match app.view.item(&app.board) {
                Some(model::Item::Mr { agent, .. } | model::Item::Ticket { agent, .. }) => agent.as_ref(),
                _ => None,
            };
            if let Some(w @ model::Worker { session: Some(id), .. }) = agent {
                if let Some(i) = app.images.as_mut() {
                    i.forget();
                }
app.shown = Shown::Session {
                    id: id.clone(),
                    agent: w.agent.clone(),
                    view: sessionui::View::at_end(),
                };
            }
        }
        Act::Describe => {
            if let Some(iid) = app.picked().map(|m| m.iid) {
                app.view.mode = Mode::Blurb { iid, text: None, scroll: 0 };
                let (tx, gitlab, project) = (app.tx.clone(), app.gitlab.clone(), app.cfg.project.clone());
                std::thread::spawn(move || {
                    let result = gitlab.get().and_then(|g| g.description(&project, iid));
                    let _ = tx.send(Msg::Blurb { iid, result });
                });
            }
        }
        Act::Ticket => {
            if let Some(model::Item::Mr { ticket: Some(id), .. }) = app.view.item(&app.board) {
                if let Some(t) = app.tickets.iter().find(|t| t.id == *id) {
                    app.note = term::open(&t.url);
                }
            }
        }
        Act::Reviewers => {
            if let Some(mr) = app.picked() {
                let now = model::others(&mr.reviewers, &app.snap.me);
                let text = if now.is_empty() {
                    String::new()
                } else {
                    format!("{}, ", now.join(", "))
                };
                app.view.mode = Mode::Typing(Typing {
                    iid: mr.iid,
                    text,
                    cycling: None,
                    sets: Sets::Reviewers,
                });
            }
        }
        // Read on the press, not taken from the board's last answer, so the
        // cc goes onto the description as it was when you opened the input.
        Act::Cc => {
            if let Some(iid) = app.picked().map(|mr| mr.iid) {
                app.note = Some(format!("reading {}'s cc…", bang(iid)));
                let (gitlab, tx, project) = (app.gitlab.clone(), app.tx.clone(), app.cfg.project.clone());
                std::thread::spawn(move || {
                    let result = gitlab.get().and_then(|g| g.description(&project, iid));
                    tx.send(Msg::Described { iid, result })
                });
            }
        }
        // On a ticket's row, the ticket.
        Act::Link => {
            if let Some(mr) = app.picked() {
                term::copy(&mr.url);
                app.note = Some(format!("copied the link to {}", bang(mr.iid)));
            } else if let Some(t) = app.picked_ticket() {
                term::copy(&t.url);
                app.note = Some(format!("copied the link to {}", t.id));
            }
        }
        Act::Web => {
            let url = app.picked().map(|mr| &mr.url).or_else(|| app.picked_ticket().map(|t| &t.url));
            if let Some(url) = url.cloned() {
                app.note = term::open(&url);
            }
        }
        Act::Approve => {
            if let Some(mr) = app.picked() {
                app.view.mode = Mode::Approve { iid: mr.iid };
            }
        }
        Act::Refresh => app.refresh(),
        // Rebased here, not by GitLab (rebase.rs); it asks once it knows
        // what it will do.
        Act::Rebase => {
            if let Some(iid) = app.picked().map(|mr| mr.iid) {
                if !app.ask(iid) {
                    app.rebase(iid);
                }
            }
        }
        // Again on a stack that is landing, it stops: every landing with
        // that MR in it, as the MRs under a fork are in each branch's.
        Act::Land => {
            if let Some(mr) = app.picked() {
                let iid = mr.iid;
                if app.landing_of(iid).is_some() {
                    let (stopped, kept): (Vec<land::Landing>, _) = std::mem::take(&mut app.landings).into_iter().partition(|l| l.iids.contains(&iid));
                    app.landings = kept;
                    app.keep_landings();
                    let names: Vec<String> = stopped.iter().map(|l| l.names()).collect();
                    app.note = Some(format!("stopped landing {}", names.join(" and ")));
                } else if app.cfg.rules.deploy_job.is_none() {
                    app.note = Some("landing waits on production deploys, so it needs git config ot.deployjob".into());
                } else {
                    match land::Landing::of(&app.snap, &app.was_on, mr) {
                        Ok(l) => app.view.mode = Mode::Land(l),
                        Err(e) => app.note = Some(e),
                    }
                }
            }
        }
        Act::Help => app.view.mode = Mode::Help,
        Act::Quit => return Next::Quit,
        // Every other board key is a plan of writes to the MR (model::plan),
        // asked about first when it can't be undone.
        act => {
            let planned = app
                .picked()
                .and_then(|mr| Some((mr.iid, model::plan(act, mr)?)));
            if let Some((iid, plan)) = planned {
                match plan.question {
                    Some(_) => app.view.mode = Mode::Confirm { iid, plan },
                    None => carry_out(app, iid, plan),
                }
            }
        }
    }
    Next::Stay
}


/// The input of names: letters type, tab completes (again cycles), enter
/// sets them, esc drops it. What the board does with the next key.
fn typing(app: &mut App, mut t: Typing, name: &str) -> Mode {
    match name {
        "esc" => return Mode::Board,
        "enter" => {
            let plan = app.snap.mr(t.iid).map(|mr| match &t.sets {
                Sets::Reviewers => model::reviewers(mr, &t.text),
                Sets::Cc { description } => model::cc(mr, description, &t.text),
            });
            if let Some(plan) = plan {
                carry_out(app, t.iid, plan);
            }
            return Mode::Board;
        }
        "tab" => {
            let (typed, nth) = match t.cycling.take() {
                Some((typed, nth)) => (typed, nth + 1),
                None => (t.text.clone(), 0),
            };
            t.text = model::complete(&typed, &people(&t.sets, &app.snap, &app.members, &app.slack), nth);
            t.cycling = Some((typed, nth));
        }
        _ => {
            if keys::edit(&mut t.text, name) {
                t.cycling = None;
            }
        }
    }
    Mode::Typing(t)
}

/// Who the input of names completes from: reviewers from the project and
/// who the board has seen, a cc from everyone in your Slack.
fn people<'a>(sets: &Sets, snap: &Snapshot, members: &[String], slack: &'a [String]) -> Cow<'a, [String]> {
    match sets {
        Sets::Reviewers => Cow::Owned(model::people(snap, members)),
        Sets::Cc { .. } => Cow::Borrowed(slack),
    }
}

/// A plan's writes, off the UI thread, its row saying what is happening.
fn carry_out(app: &mut App, iid: u64, plan: model::Plan) {
    app.doing.insert(iid, Doing::word(plan.doing));
    let (writer, tx) = (app.writer.clone(), app.tx.clone());
    std::thread::spawn(move || {
        let result = write::all(writer.as_ref(), &plan.writes);
        let doing = plan.doing;
        let _ = tx.send(Msg::Wrote { iid, doing, result });
    });
}

/// cliamp's volume `by` dB from where it is. The review's player in nvim
/// runs the same cliamp commands (nvim/ot.lua, player_acts).
fn volume(by: i32) {
    let params = format!("{{\"value\":{by}}}");
    off_thread(
        "cliamp",
        &["remote", "call", "volume.adjust", "--params", &params],
        String::new(),
    );
}

/// How conversations draw images: OT_IMAGES=kitty|halfblocks|off says, else the
/// terminal's answer. zellij drops kitty's escapes, so inside it, half blocks.
fn image_picker() -> Option<ratatui_image::picker::Picker> {
    use ratatui_image::picker::{Picker, ProtocolType};
    let asked = || Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    match std::env::var("OT_IMAGES").as_deref() {
        Ok("off") => None,
        Ok("halfblocks") => Some(Picker::halfblocks()),
        Ok("kitty") => {
            let mut p = asked();
            p.set_protocol_type(ProtocolType::Kitty);
            Some(p)
        }
        _ if std::env::var_os("ZELLIJ").is_some() => Some(Picker::halfblocks()),
        _ => Some(asked()),
    }
}

/// The places the screen's keys come from, or what the reply box leaves showing.
fn places_now(places: &Option<(&'static str, Vec<keys::Place>)>) -> Vec<keys::Place> {
    places.as_ref().map_or(vec![keys::Place::Transcript], |(_, p)| p.clone())
}

/// How a T3 thread stands, in a word.
fn hc_word(s: hc::Status) -> &'static str {
    match s {
        hc::Status::Working => "working",
        hc::Status::Blocked => "waiting on you",
        hc::Status::Stopped => "done",
    }
}

/// What a conversation being read asks in a box over it.
enum Asking {
    Reply(String, String),
    Stop(String),
    Rename(String, String),
    New(String),
}

/// The box a reply is typed in, or a stop is confirmed, over a conversation.
fn reply_box(f: &mut ratatui::Frame, asking: Option<&Asking>) {
    use ratatui::text::Line;
    match asking {
        None => {}
        Some(Asking::Reply(name, text)) => ui::ask(f, |w| {
            let mut lines = vec![Line::styled(format!("reply to {name}:"), ui::fg(ui::DIM))];
            lines.extend(ui::typed(text, w));
            lines.push(Line::default());
            lines.push(ui::keys(&[("enter", " sends  "), ("esc", " drops")]));
            lines
        }),
        Some(Asking::Rename(name, text)) => ui::ask(f, |w| {
            let mut lines = vec![Line::styled(format!("rename {name}:"), ui::fg(ui::DIM))];
            lines.extend(ui::typed(text, w));
            lines.push(Line::default());
            lines.push(ui::keys(&[("enter", " renames  "), ("esc", " drops")]));
            lines
        }),
        Some(Asking::New(text)) => ui::ask(f, |w| {
            let mut lines = vec![Line::styled("a new T3 thread, in this checkout's project. what should it do?", ui::fg(ui::DIM))];
            lines.extend(ui::typed(text, w));
            lines.push(Line::default());
            lines.push(ui::keys(&[("enter", " starts it  "), ("esc", " drops")]));
            lines
        }),
        Some(Asking::Stop(name)) => ui::ask(f, |_| {
            vec![
                Line::styled(format!("stop {name}? its running turn ends, as T3's Stop does"), ui::fg(ui::AMBER)),
                Line::default(),
                ui::keys(&[("y", " yes  "), ("n", " no")]),
            ]
        }),
    }
}

/// Settles a T3 thread off the UI thread; the note says how it went.
fn settle(app: &mut App, a: hc::Agent) {
    app.note = Some(format!("settling {}…", a.name));
    let tx = app.tx.clone();
    std::thread::spawn(move || {
        let how = match t3::settle(&a.address) {
            Ok(()) => format!("settled {}", a.name),
            Err(e) => format!("{} not settled: {e}", a.name),
        };
        let _ = tx.send(Msg::Asked(how));
    });
}

/// A command run for its effect, fed `input`, with nobody waiting on it.
fn off_thread(cmd: &str, args: &[&str], input: String) {
    let (cmd, args): (String, Vec<String>) =
        (cmd.into(), args.iter().map(|a| a.to_string()).collect());
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

/// The terminal is the command's until it exits; then the board's again.
fn lend_terminal(
    term: &mut ratatui::DefaultTerminal,
    lend: &Sender<Lend>,
    mut cmd: Command,
) -> Result<std::process::ExitStatus, String> {
    let (parked, wait_parked) = mpsc::channel();
    let (give_back, back) = mpsc::channel();
    lend.send(Lend { parked, back })
        .map_err(|e| e.to_string())?;
    let _ = wait_parked.recv();
    let _ = execute!(std::io::stdout(), DisableFocusChange, DisableBracketedPaste);
    ratatui::restore();
    let ran = cmd.status();
    let _ = enable_raw_mode();
    let _ = execute!(std::io::stdout(), EnterAlternateScreen, EnableFocusChange, EnableBracketedPaste);
    let _ = term.clear();
    let _ = give_back.send(());
    ran.map_err(|e| format!("{}: {e}", cmd.get_program().to_string_lossy()))
}

fn draw(term: &mut ratatui::DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    if let (Some((p, _)), Ok(s)) = (app.popup.as_mut(), term.size()) {
        p.fit(ratatui::layout::Rect::new(0, 0, s.width, s.height));
    }
    let pop = app.popup.as_ref().map(|(p, _)| p.shows());
    if let Shown::Music(view) = &app.shown {
        return term
            .draw(|f| {
                let aim = app.sought.map(|(_, at, _)| at);
                let loading = app.loading.as_ref().map(|l| l.title.as_str());
                musicui::draw(f, app.playing.as_ref(), &app.hills, aim, loading, view)
            })
            .map(|_| ());
    }
    let tell = match &app.view.mode {
        Mode::Tell { to, text } => Some(Asking::Reply(to.name.clone(), text.clone())),
        Mode::Rename { to, text } => Some(Asking::Rename(to.name.clone(), text.clone())),
        Mode::NewThread(text) => Some(Asking::New(text.clone())),
        Mode::Stop(to) => Some(Asking::Stop(to.name.clone())),
        _ => None,
    };
    let places = places_now(&app.places());
    let side_threads = app.side_threads();
    let side_mrs = app.side_mrs(&side_threads);
    let reading = app.reply_to().map(|a| a.address);
    let width = term.size().map_or(0, |s| s.width);
    app.width = width as usize;
    let side_shows = app.side_shows(width);
    let mr = app.thread_mr().filter(|_| !app.panel.hidden && width >= 140).cloned();
    let patch = mr.as_ref().and_then(|m| app.patches.get(&m.iid)).map(|(_, p)| p.clone());
    let panel = mr.as_ref().map(|m| (m, patch.as_ref()));
    let panel_scroll = &mut app.panel.view;
    if let Shown::Session { id, agent, view } = &mut app.shown {
        // on_sessions keeps the one shown on the board, and a session's
        // agents stay in it.
        let s = app.sessions.iter().find(|s| s.id == *id).unwrap();
        let now = jiff::Timestamp::now().as_second();
        let t = match agent {
            Some(name) => sessionui::Transcript::agent(s, name, now).unwrap(),
            None => sessionui::Transcript::session(s, now),
        };
        let note = app.note.as_deref();
        let images = app.images.as_mut();
        let (finder, statuses) = (app.finder.as_mut(), statuses(&app.agents));
        let threads = side_threads;
        let side = ui::side::Side { threads: &threads, mrs: &side_mrs, said: &app.said, reading: reading.as_deref(), cursor: app.side.cursor.as_deref() };
        return term.draw(|f| {
            let mut area = f.area();
            if let Some((m, patch)) = panel {
                let w = (area.width * 2 / 5).max(50);
                let at = ratatui::layout::Rect { x: area.right() - w, width: w, ..area };
                ui::mr::panel(f, at, m, patch, panel_scroll, now);
                area.width -= w;
            }
            if side_shows && area.width >= ui::side::FROM {
                let w = ui::side::WIDTH.min(area.width / 3);
                ui::side::draw(f, ratatui::layout::Rect { width: w, ..area }, &side, now);
                let rest = ratatui::layout::Rect { x: area.x + w + 1, width: area.width - w - 1, ..area };
                sessionui::draw_in(f, rest, &t, now, view, &places, note, images);
            } else {
                sessionui::draw(f, &t, now, view, &places, note, images);
            }
            reply_box(f, tell.as_ref());
            if let Some(fd) = finder {
                ui::finder::draw(f, fd, &statuses, now);
            }
            if let Some(p) = &pop {
                popup::draw(f, p);
            }
        })
        .map(|_| ());
    }
    if let Shown::Thread { .. } = &app.shown {
        let now = jiff::Timestamp::now().as_second();
        let Shown::Thread { agent, turns, view } = &mut app.shown else { unreachable!() };
        let mut facts = vec![hc_word(agent.status).to_string(), model::ago(now - agent.at), "t3".to_string()];
        if turns.is_empty() {
            facts.insert(0, "reading…".to_string());
        }
        let t = sessionui::Transcript { title: agent.name.clone(), facts, turns, voices: ["you", "claude"] };
        let note = app.note.as_deref();
        let images = app.images.as_mut();
        let (finder, statuses) = (app.finder.as_mut(), statuses(&app.agents));
        let threads = side_threads;
        let side = ui::side::Side { threads: &threads, mrs: &side_mrs, said: &app.said, reading: reading.as_deref(), cursor: app.side.cursor.as_deref() };
        return term.draw(|f| {
            let mut area = f.area();
            if let Some((m, patch)) = panel {
                let w = (area.width * 2 / 5).max(50);
                let at = ratatui::layout::Rect { x: area.right() - w, width: w, ..area };
                ui::mr::panel(f, at, m, patch, panel_scroll, now);
                area.width -= w;
            }
            if side_shows && area.width >= ui::side::FROM {
                let w = ui::side::WIDTH.min(area.width / 3);
                ui::side::draw(f, ratatui::layout::Rect { width: w, ..area }, &side, now);
                let rest = ratatui::layout::Rect { x: area.x + w + 1, width: area.width - w - 1, ..area };
                sessionui::draw_in(f, rest, &t, now, view, &places, note, images);
            } else {
                sessionui::draw(f, &t, now, view, &places, note, images);
            }
            reply_box(f, tell.as_ref());
            if let Some(fd) = finder {
                ui::finder::draw(f, fd, &statuses, now);
            }
            if let Some(p) = &pop {
                popup::draw(f, p);
            }
        })
        .map(|_| ());
    }
    let stamp = app.stamp();
    let people = match &app.view.mode {
        Mode::Typing(t) => people(&t.sets, &app.snap, &app.members, &app.slack),
        _ => Cow::default(),
    };
    let landing = land::said(&app.landings, &app.snap, &app.cfg.rules);
    app.width = term.size().map_or(app.width, |s| s.width as usize);
    let statuses = statuses(&app.agents);
    term.draw(|f| {
        let s = boardui::Screen {
            snap: &app.snap,
            board: &app.board,
            rules: &app.cfg.rules,
            project: &app.cfg.project,
            stamp: &stamp,
            error: app.error.as_deref(),
            note: app.note.as_deref(),
            doing: &app.doing,
            landing: &landing,
            people: &people,
            tickets: &app.tickets,
            briefs: &app.briefs,
            now: jiff::Timestamp::now().as_second(),
            playing: app.playing.as_ref(),
            hills: &app.hills,
            tours: &app.tours,
            stats: app.stats.as_ref(),
        };
        boardui::draw(f, &s, &mut app.view);
        if let Mode::Blurb { iid, text, scroll } = &mut app.view.mode {
            if let Some(m) = app.snap.mrs.iter().find(|m| m.iid == *iid) {
                ui::blurb::draw(f, m, text.as_ref(), scroll);
            }
        }
        if let Some(fd) = app.finder.as_mut() {
            ui::finder::draw(f, fd, &statuses, s.now);
        }
        if let Some(p) = &pop {
            popup::draw(f, p);
        }
    })?;
    Ok(())
}

/// What each T3 thread on the board is doing, for the finder's rows.
fn statuses(agents: &[hc::Agent]) -> HashMap<String, hc::Status> {
    agents.iter().filter(|a| t3::owns(&a.address)).map(|a| (a.address.clone(), a.status)).collect()
}

/// How often the board looks at what Claude is doing.
const CLAUDE_EVERY: Duration = Duration::from_secs(2);

/// The thread that watches Claude's sessions on this repo's checkouts and
/// says when they change; not ot's own review checkout, `review`, where its
/// tour chats run.
fn watch_claude(tx: Sender<Msg>, review: PathBuf) {
    std::thread::spawn(move || {
        let mut reader = claude::Reader::default();
        let mut last = vec![];
        loop {
            let checkouts: Vec<git::Worktree> = git::worktrees().into_iter().filter(|w| w.path != review).collect();
            let found = reader.sessions(&checkouts, jiff::Timestamp::now().as_second());
            if found != last {
                last = found.clone();
                if tx.send(Msg::Claude(found)).is_err() {
                    return;
                }
            }
            std::thread::sleep(CLAUDE_EVERY);
        }
    });
}

/// The thread that refreshes the board, asking GitLab and, with a key,
/// Linear on a thread of its own, so a slow Linear never holds GitLab's
/// answer up, then bringing your local main up to origin's. Each () asks
/// once; asks that pile up while one is out are answered by that one.
fn fetcher(cfg: Config, gitlab: Shared, tx: Sender<Msg>) -> Sender<()> {
    let (fetches, asked) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        while asked.recv().is_ok() {
            while asked.try_recv().is_ok() {}
            if let Some(key) = cfg.linear_key.clone() {
                let (tx, since) = (tx.clone(), cfg.linear_since);
                std::thread::spawn(move || tx.send(Msg::Tickets(linear::tickets(&key, since))));
            }
            let answer = gitlab.get().and_then(|g| snapshot(&cfg, &g));
            let main = answer.as_ref().ok().map(|s| s.default_branch.clone());
            if tx.send(Msg::Fetched(answer)).is_err() {
                break;
            }
            // After the board has its answer, so it never waits on the fetch.
            if let Some(main) = main {
                git::forward(Path::new("."), &main);
            }
        }
    });
    fetches
}

/// GitLab's answer with what local git knows added (the branch you are on,
/// how old your MRs' bases are), and, with ot.deployJob set, yours that
/// merged and production doesn't have yet; kept for the next launch.
fn snapshot(cfg: &Config, gl: &gitlab::GitLab) -> Result<Snapshot, String> {
    let mut snap = gl.snapshot(&cfg.project)?;
    snap.here = git::branch();
    let me = snap.me.clone();
    let mine: Vec<&mut model::Mr> = snap.mrs.iter_mut().filter(|m| m.author == me).collect();
    let pairs: Vec<(&str, &str)> = mine
        .iter()
        .map(|m| (m.head.as_str(), m.target.as_str()))
        .collect();
    let ages = git::split_ages(&pairs);
    for (m, age) in mine.into_iter().zip(ages) {
        m.split_age = age;
    }
    // Yours that merged stay until production has them.
    if let Some(job) = &cfg.rules.deploy_job {
        let main = snap.default_branch.clone();
        let (merged, prod) = std::thread::scope(|s| {
            let prod = s.spawn(|| gl.production(&cfg.project, &main, job));
            (gl.merged(&cfg.project, &main), prod.join().unwrap())
        });
        let (merged, prod) = (merged?, prod?);
        let commits: Vec<&str> = merged.iter().map(|m| m.merge_commit.as_str()).collect();
        let reach = git::reach(Path::new("."), &prod, &main, &commits);
        // Read after the board, so one that merged in between is merged.
        snap.mrs.retain(|o| merged.iter().all(|m| m.iid != o.iid));
        for (m, r) in merged.into_iter().zip(reach) {
            if r != model::Reach::Live {
                snap.mrs.push(model::Mr { reach: Some(r), ..m });
            }
        }
        // Yours on main still with one of those's own commits, which ot
        // rebases past it on its own (App::due).
        let merged: Vec<(u64, String)> = snap.mrs.iter().filter(|m| m.reach.is_some()).map(|m| (m.iid, m.head.clone())).collect();
        let merged: Vec<(u64, &str)> = merged.iter().map(|(i, h)| (*i, h.as_str())).collect();
        let on_main: Vec<&mut model::Mr> = snap.mrs.iter_mut().filter(|m| m.author == me && m.reach.is_none() && m.target == main).collect();
        let heads: Vec<&str> = on_main.iter().map(|m| m.head.as_str()).collect();
        let carries = git::carries(Path::new("."), &main, &merged, &heads);
        for (m, c) in on_main.into_iter().zip(carries) {
            m.carries = c;
        }
    }
    // The last board kept is what this one's sizes are read from.
    let known = cache::load(&cfg.project).map(|s| s.mrs).unwrap_or_default();
    gl.size(&cfg.project, &mut snap.mrs, &known)?;
    // A cache that can't be written costs the next launch its head start,
    // nothing more.
    let _ = cache::save(&cfg.project, &snap);
    Ok(snap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::fixture;
    use write::{At, Recorder, Write};

    fn app(writer: Arc<Recorder>) -> (App, Receiver<Msg>) {
        let cfg = Config {
            host: "git.example".into(),
            project: "team/app".into(),
            rules: fixture::rules(),
            slack_token: None,
            linear_key: None,
            linear_since: None,
        };
        App::new(cfg, fixture::snapshot(), Shared::new("git.example"), writer)
    }

    /// Keys as typed, then what the writes came to.
    fn press(app: &mut App, rx: &Receiver<Msg>, keys: &[&str]) -> Vec<Msg> {
        for k in keys {
            key(app, k);
        }
        std::iter::from_fn(|| rx.recv_timeout(Duration::from_millis(200)).ok()).collect()
    }

    fn onto(app: &mut App, iid: u64) {
        while app.view.at != Some(Pick::Mr(iid)) {
            app.view.step(&app.board, 1);
        }
    }

    #[test]
    fn ctrl_l_focuses_a_threads_mr_and_ctrl_h_comes_back() {
        let (mut app, _rx) = app(Arc::new(Recorder::default()));
        let mr = app.snap.mrs.iter().find(|m| m.reach.is_none()).unwrap().clone();
        let agent = hc::Agent { address: "bba1a549-60f8-4425-bb4e-569c328a3cc4".into(), name: "a thread".into(), status: hc::Status::Stopped, said: String::new(), at: 0, pid: None, branch: mr.source.clone() };
        app.shown = Shown::Thread { agent, turns: vec![], view: sessionui::View::at_end() };
        app.width = 200;
        assert_eq!(app.thread_mr().map(|m| m.iid), Some(mr.iid));
        key(&mut app, "ctrl-l");
        assert!(app.panel.view.focused, "ctrl-l focuses the MR");
        assert_eq!(app.places().map(|p| p.0), Some("mr"));
        key(&mut app, "j");
        assert_eq!(app.panel.view.scroll, 1, "j scrolls the MR's diff, not the conversation");
        key(&mut app, "ctrl-h");
        assert!(!app.panel.view.focused, "ctrl-h goes back to the conversation");
        app.width = 100;
        key(&mut app, "ctrl-l");
        assert!(!app.panel.view.focused, "no MR shown, nothing to focus");
        app.agents.push(hc::Agent { address: "0621b23e-9a63-41b0-bd6f-1798e8a08f3b".into(), ..match &app.shown { Shown::Thread { agent, .. } => agent.clone(), _ => unreachable!() } });
        key(&mut app, "tab");
        assert!(app.side.cursor.is_some(), "tab goes into the thread list");
        key(&mut app, "n");
        assert!(matches!(app.view.mode, Mode::NewThread(_)) && app.side.cursor.is_none(), "n there asks what a new thread should do");
    }

    #[test]
    fn a_ready_rebase_waits_on_its_row_until_enter_and_esc_keeps_it_there() {
        use crate::rebase::{Layer, Rebased, Said};
        let (mut app, _rx) = app(Arc::new(Recorder::default()));
        let ready = || Rebased {
            layers: vec![Layer {
                iid: 105,
                branch: "sam/retry-budget".into(),
                remote: "a1".into(),
                local: None,
                start: "a1".into(),
                new: "b2".into(),
            }],
            said: vec![Said { detail: false, text: "!105  Change 105".into() }],
            question: "rebase it?".into(),
            left_out: None,
            settled: 0,
            changed: vec![],
        };
        app.on_rebased(105, Ok(ready()));
        assert!(matches!(app.view.mode, Mode::Board), "nothing pops up");
        let row = app.doing.get(&105).unwrap();
        assert!(row.on_you && row.word == "rebase ready");
        onto(&mut app, 105);
        key(&mut app, "enter");
        assert!(matches!(app.view.mode, Mode::Rebase { iid: 105, .. }), "enter on its row asks");
        key(&mut app, "j");
        assert!(matches!(app.view.mode, Mode::Rebase { .. }), "a stray key is not an answer");
        key(&mut app, "esc");
        assert!(matches!(app.view.mode, Mode::Board) && app.prepared.contains_key(&105), "esc keeps it for later");
        key(&mut app, "b");
        key(&mut app, "n");
        assert!(!app.prepared.contains_key(&105) && !app.doing.contains_key(&105), "n lets it go");
    }

    #[test]
    fn t_on_a_row_an_agent_is_at_work_on_reads_that_agent() {
        let (mut app, _rx) = app(Arc::new(Recorder::default()));
        (app.snap, app.tickets, app.sessions) = fixture::swarm();
        app.rebuild();
        onto(&mut app, 302);
        key(&mut app, "t");
        assert!(matches!(&app.shown, Shown::Session { id, agent: Some(a), .. } if id == "s3" && a == "ap-2"));
    }

    #[test]
    fn merge_asks_first_and_only_y_merges() {
        let rec = Arc::new(Recorder::default());
        let (mut app, rx) = app(rec.clone());
        onto(&mut app, 105);
        press(&mut app, &rx, &["m", "n"]);
        assert!(rec.took().is_empty(), "anything but y leaves it");
        let done = press(&mut app, &rx, &["m", "y"]);
        assert_eq!(
            rec.took(),
            [Write::Merge {
                iid: 105,
                head: String::new()
            }]
        );
        assert!(matches!(
            done[..],
            [Msg::Wrote {
                iid: 105,
                doing: "merging",
                result: Ok(())
            }]
        ));
        assert_eq!(
            app.doing.get(&105).map(|d| d.word.as_str()),
            Some("merging"),
            "the row says so until GitLab's next answer"
        );
    }

    #[test]
    fn your_change_left_in_the_review_is_asked_about_until_you_say() {
        let (mut app, rx) = app(Arc::new(Recorder::default()));
        let change = worktree::Change { on: "h102".into(), subject: "Sum".into(), files: vec!["f".into()] };
        app.on_checkout(None, Some(change.clone()), Some((102, "sam/sum".into())));
        assert!(matches!(&app.view.mode, Mode::Unpushed(l) if l.iid == 102 && l.change == change && l.why.is_none()));
        press(&mut app, &rx, &["x", "c", "S", "a", "y", " ", "i", "t"]);
        assert!(matches!(&app.view.mode, Mode::Message(_, text) if text == "Say it"), "a stray key is no answer; c types a message");
        press(&mut app, &rx, &["esc"]);
        assert!(matches!(app.view.mode, Mode::Unpushed(_)), "esc goes back to the question");
        press(&mut app, &rx, &["esc"]);
        assert!(matches!(app.view.mode, Mode::Board) && app.note.as_deref().is_some_and(|n| n.contains("stays in the review checkout")));
        // A try that didn't go asks again, saying why; one that did says what went.
        app.on_checkout(Some((102, Err("refused".into()))), Some(change), Some((102, "sam/sum".into())));
        assert!(matches!(&app.view.mode, Mode::Unpushed(l) if l.why.as_deref() == Some("refused")));
        app.view.mode = Mode::Board;
        app.on_checkout(Some((102, Ok("amended and pushed".into()))), None, None);
        assert_eq!(app.note.as_deref(), Some("amended and pushed"));
    }

    #[test]
    fn reviewers_are_typed_completed_and_set() {
        let rec = Arc::new(Recorder::default());
        let (mut app, rx) = app(rec.clone());
        onto(&mut app, 102);
        press(
            &mut app,
            &rx,
            &["a", "a", "l", "tab", ",", " ", "k", "tab", "enter"],
        );
        assert_eq!(
            rec.took(),
            [Write::Reviewers {
                iid: 102,
                names: vec!["alex".into(), "kim".into()]
            }]
        );
    }

    #[test]
    fn cc_opens_on_who_the_description_ccs_completes_from_slack_and_is_set() {
        let rec = Arc::new(Recorder::default());
        let (mut app, rx) = app(rec.clone());
        app.slack = vec!["alex@team.example".into(), "kim@team.example".into()];
        onto(&mut app, 102);
        app.on_described(102, Ok("Fixes it.\n\ncc: alex@team.example".into()));
        press(&mut app, &rx, &["k", "tab", "enter"]);
        assert_eq!(
            rec.took(),
            [Write::Description {
                iid: 102,
                text: "Fixes it.\n\ncc: alex@team.example kim@team.example".into()
            }]
        );
    }

    #[test]
    fn approving_asks_first() {
        let rec = Arc::new(Recorder::default());
        let (mut app, rx) = app(rec.clone());
        onto(&mut app, 201);
        press(&mut app, &rx, &["A", "esc"]);
        assert!(rec.took().is_empty());
        press(&mut app, &rx, &["A", "y"]);
        assert_eq!(
            rec.took(),
            [Write::Approve {
                iid: 201,
                head: String::new()
            }]
        );
    }

    #[test]
    fn approving_with_a_note_says_it_first() {
        let rec = Arc::new(Recorder::default());
        let (mut app, rx) = app(rec.clone());
        onto(&mut app, 201);
        press(&mut app, &rx, &["A", "m", "o", "k", "enter"]);
        assert_eq!(
            rec.took(),
            [
                Write::Comment {
                    iid: 201,
                    at: At::Mr,
                    body: "ok".into()
                },
                Write::Approve {
                    iid: 201,
                    head: String::new()
                }
            ]
        );
    }
}
