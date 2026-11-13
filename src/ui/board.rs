//! The board: your MRs and the ones you review as a tree, the row under the
//! cursor in full below it, and the keys that work there.
use super::music::{Hills, DEEPEST};
use super::{ask, fg, keys as key_line, pack, put, tone, typed, width, wrap, Out, AMBER, BAND, DIM, FAINT, GREEN, TEAL, TEXT};
use crate::keys::{self, Act, Place};
use crate::land::{Landing, Said};
use crate::claude::State;
use crate::brief::Briefs;
use crate::hc::{Agent, Status};
use crate::model::{self, bang, panel, Board, Item, Move, Plan, Row, Rules, Snapshot, Stage, Ticket, Tone, Worker};
use crate::music::Playing;
use crate::rebase::Rebased;
use crate::worktree::Change;
use crate::write::Write;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

/// What the cursor can rest on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    Mr(u64),
    /// One of your tickets with no MR yet, by its id.
    Ticket(String),
    Session(String),
    /// One of hc's agents, by its address.
    Hc(String),
    Fold,
}

/// What the cursor rests on when it is on the row of `item`, if anything.
fn pick_of(item: &Item) -> Option<Pick> {
    match item {
        Item::Mr { iid, .. } => Some(Pick::Mr(*iid)),
        Item::Ticket { id, .. } => Some(Pick::Ticket(id.clone())),
        Item::Claude { id, .. } => Some(Pick::Session(id.clone())),
        Item::Hc { address, .. } => Some(Pick::Hc(address.clone())),
        Item::Label(_) | Item::Gap | Item::Here(_) => None,
    }
}

/// What the board does with your keys: moves around it, shows its keys,
/// waits on y for a question, asks how to approve, takes the note an
/// approval goes with, takes the reviewers you type, or what you tell
/// one of hc's agents.
#[derive(Debug, Default)]
pub enum Mode {
    #[default]
    Board,
    Help,
    Confirm {
        iid: u64,
        plan: Plan,
    },
    /// A rebase prepared, shown in the panel, waiting on y to push it.
    Rebase {
        iid: u64,
        ready: Rebased,
    },
    /// A stack to land, waiting on y.
    Land(Landing),
    Approve {
        iid: u64,
    },
    Note {
        iid: u64,
        text: String,
    },
    Typing(Typing),
    /// What you are typing to one of hc's agents.
    Tell {
        to: Agent,
        text: String,
    },
    /// Its stop key, waiting on y.
    Stop(Agent),
    /// An MR's title and description, read over the board: what GitLab said, None
    /// while it is asked, and how far it is scrolled.
    Blurb {
        iid: u64,
        text: Option<Result<String, String>>,
        scroll: usize,
    },
    /// A new title for a T3 thread, being typed.
    Rename {
        to: Agent,
        text: String,
    },
    /// The first message of a new T3 thread, being typed.
    NewThread(String),
    /// Your change left in the review checkout, waiting on how it goes.
    Unpushed(Left),
    /// Its message as a new commit, being typed.
    Message(Left, String),
}

/// Your change in the review checkout, the MR of yours it goes to, and why
/// the last try didn't go, if one didn't.
#[derive(Debug)]
pub struct Left {
    pub iid: u64,
    pub branch: String,
    pub change: Change,
    pub why: Option<String>,
}

/// The board's own state: where the cursor is and what is open.
#[derive(Debug, Default)]
pub struct View {
    pub at: Option<Pick>,
    /// Where the cursor was in the list, to land near it when its row goes.
    index: usize,
    pub fold_open: bool,
    /// The MRs you hid show under the board.
    pub hidden_open: bool,
    scroll: usize,
    pub mode: Mode,
}

/// The input of names, asked in the box over the board.
#[derive(Debug)]
pub struct Typing {
    pub iid: u64,
    pub text: String,
    /// While tab cycles: what was typed before it, and which name it is on.
    pub cycling: Option<(String, usize)>,
    pub sets: Sets,
}

/// What the names typed are for, which decides who tab completes from.
#[derive(Debug)]
pub enum Sets {
    /// The MR's reviewers, from the project and who the board has seen.
    Reviewers,
    /// The cc line of its description, as it was read when the input
    /// opened, from everyone in your Slack.
    Cc { description: String },
}

impl View {
    fn picks(&self, b: &Board) -> Vec<Pick> {
        let of = |rows: &[Row]| rows.iter().filter_map(|r| pick_of(&r.item)).collect::<Vec<_>>();
        let mut picks = of(&b.tree);
        picks.extend(of(&b.claude));
        if !b.not_sent.is_empty() {
            picks.push(Pick::Fold);
            if self.fold_open {
                picks.extend(of(&b.not_sent));
            }
        }
        if self.hidden_open {
            picks.extend(of(&b.hidden));
        }
        picks
    }

    /// The row the cursor is on.
    pub fn item<'b>(&self, b: &'b Board) -> Option<&'b Item> {
        let at = self.at.as_ref()?;
        let rows = b.tree.iter().chain(&b.claude).chain(&b.not_sent).chain(&b.hidden);
        rows.map(|r| &r.item).find(|i| pick_of(i).as_ref() == Some(at))
    }

    fn go(&mut self, picks: &[Pick], i: usize) {
        self.index = i.min(picks.len().saturating_sub(1));
        self.at = picks.get(self.index).cloned();
    }

    pub fn step(&mut self, b: &Board, by: isize) {
        let picks = self.picks(b);
        let now = self
            .at
            .as_ref()
            .and_then(|a| picks.iter().position(|p| p == a))
            .unwrap_or(0);
        self.go(&picks, now.saturating_add_signed(by));
    }

    pub fn top(&mut self, b: &Board) {
        self.go(&self.picks(b), 0);
    }

    pub fn bottom(&mut self, b: &Board) {
        self.go(&self.picks(b), usize::MAX);
    }

    /// After the board is rebuilt: the same row if it is still there, else
    /// whatever now sits where it was.
    pub fn settle(&mut self, b: &Board) {
        let picks = self.picks(b);
        match self
            .at
            .as_ref()
            .and_then(|a| picks.iter().position(|p| p == a))
        {
            Some(i) => self.index = i,
            None => self.go(&picks, self.index),
        }
    }

    /// Every place the cursor is in, for which keys work (keys.rs).
    pub fn places(&self, snap: &Snapshot, b: &Board) -> Vec<Place> {
        let mut places = self.places_of_pick(snap);
        match self.item(b) {
            Some(Item::Mr { ticket, agent, .. }) => {
                places.extend(ticket.as_ref().map(|_| Place::TicketMr));
                places.extend(readable(agent));
            }
            Some(Item::Ticket { agent, .. }) => places.extend(readable(agent)),
            Some(Item::Hc { status, address, .. }) => {
                places.push(Place::HcAlone);
                places.extend((*status == Status::Working).then_some(Place::HcWorking));
                places.extend(crate::t3::owns(address).then_some(Place::T3));
            }
            Some(Item::Claude { terminal: Some(t), state, .. }) => {
                places.extend([Place::Hc, Place::HcSession]);
                places.extend((*state == State::Busy).then_some(Place::HcWorking));
                places.extend(crate::t3::owns(t).then_some(Place::T3));
            }
            _ => {}
        }
        places
    }

    fn places_of_pick(&self, snap: &Snapshot) -> Vec<Place> {
        match &self.at {
            Some(Pick::Mr(iid)) => {
                let iid = *iid;
                let mut places = vec![Place::Board, Place::Mr];
                match snap.mr(iid) {
                    // One that merged takes only the keys that read it, and
                    // if it is yours, cc, until it is live.
                    Some(m) if m.reach.is_some() => {
                        if m.author == snap.me {
                            places.push(Place::MyMerged);
                        }
                    }
                    Some(m) => {
                        let mine = m.author == snap.me;
                        places.push(if mine { Place::Mine } else { Place::Theirs });
                        if mine && m.draft {
                            places.push(Place::MyDraft);
                        }
                        if mine && m.pipeline_failed() {
                            places.push(Place::MyFailed);
                        }
                    }
                    None => {}
                }
                places
            }
            Some(Pick::Ticket(_)) => vec![Place::Board, Place::Ticket],
            Some(Pick::Session(_)) => vec![Place::Board, Place::Session],
            Some(Pick::Hc(_)) => vec![Place::Board, Place::Hc],
            Some(Pick::Fold) if self.fold_open => vec![Place::Board, Place::FoldOpen],
            Some(Pick::Fold) => vec![Place::Board, Place::FoldShut],
            None => vec![Place::Board],
        }
    }
}

pub struct Screen<'a> {
    pub snap: &'a Snapshot,
    pub board: &'a Board,
    pub rules: &'a Rules,
    pub project: &'a str,
    /// "as of 13:44", or that a fetch is out.
    pub stamp: &'a str,
    pub error: Option<&'a str>,
    /// What the last key did, in place of the footer's keys.
    pub note: Option<&'a str>,
    /// What is being done to an MR, in place of its move.
    pub doing: &'a HashMap<u64, Doing>,
    /// What its landing says of the MR it has come to, under that.
    pub landing: &'a HashMap<u64, Said>,
    /// Who the input of names completes from.
    pub people: &'a [String],
    /// Your tickets, as Linear last said.
    pub tickets: &'a [Ticket],
    /// Haiku's line on each agent that stopped.
    pub briefs: &'a Briefs,
    pub now: i64,
    /// What cliamp plays, and its bars.
    pub playing: Option<&'a Playing>,
    pub hills: &'a Hills,
}

/// What is being done to an MR: a word for its row ("merging"), and for a
/// long piece of work the whole of what it is at, for the panel. Or what
/// waits on you there (`on_you`): the word says which key.
#[derive(Debug)]
pub struct Doing {
    pub word: String,
    pub detail: String,
    pub on_you: bool,
}

impl Doing {
    pub fn word(word: &str) -> Doing {
        Doing { word: word.into(), detail: String::new(), on_you: false }
    }
}

/// The columns before a row's rails: two of margin, then the cursor's mark
/// and a space.
const LEAD_W: usize = 4;
/// A row's glyph and the space after it.
const GLYPH_W: usize = 2;
/// The narrowest a branch name wraps to, however small the terminal.
const MIN_BRANCH_W: usize = 8;
/// How many names the input of names suggests.
const SUGGESTED: usize = 3;
/// Where the help screen's descriptions start.
const HELP_COL: usize = 12;
/// The least room worth giving what an agent says on its row.
const MIN_SAID_W: usize = 24;
const HERE: &str = "you are here";
/// What a row Claude is busy on says, in place of its move.
const WORKING: &str = "Claude working…";

pub fn draw(f: &mut Frame, s: &Screen, v: &mut View) {
    let area = f.area();
    // Text stops a column short of the right edge; the left margin is in
    // each line's own lead.
    let right = (area.width as usize).saturating_sub(1);
    let mut top = vec![header(s, right)];
    for l in s
        .error
        .map(|e| wrap(e, right.saturating_sub(2)))
        .unwrap_or_default()
    {
        top.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(l, fg(AMBER)),
        ]));
    }
    top.push(Line::default());
    let (body, cur, under) = if matches!(v.mode, Mode::Help) {
        (help(), 0..0, vec![])
    } else {
        let (body, cur) = tree(s, v, right);
        (body, cur, panel_lines(s, v, right))
    };
    let foot = footer(s, v, right);
    let tune = s.playing.filter(|p| p.playing());
    let fixed = top.len() + under.len() + tune.iter().len() + 1;
    let room = (area.height as usize).saturating_sub(fixed);
    // Scroll only as far as keeps the cursor's lines in view; the first row
    // brings the branch name above it along.
    if v.index == 0 {
        v.scroll = 0;
    }
    if cur.end > v.scroll + room {
        v.scroll = cur.end - room;
    }
    if cur.start < v.scroll {
        v.scroll = cur.start;
    }
    v.scroll = v.scroll.min(body.len().saturating_sub(room));
    let shown = body.len().saturating_sub(v.scroll).min(room);
    let mut y = 0;
    let head = top.len();
    for l in top
        .into_iter()
        .chain(body.into_iter().skip(v.scroll).take(room))
    {
        put(f, area, &mut y, l);
    }
    // The hills in the rows nothing else uses, from the bottom of them up,
    // never behind text; the tune line carries their strip only when
    // there is no room for them, so the music shows in one place.
    let empty = room - shown;
    let hills_up = empty >= 3 && !s.hills.quiet();
    if hills_up {
        let depth = (empty - 1).min(DEEPEST);
        y = head + room - depth;
        for l in s.hills.lines(area.width as usize, depth) {
            put(f, area, &mut y, l);
        }
    }
    let tune = tune.map(|p| tune_line(p, (!hills_up).then_some(s.hills), right));
    y = (area.height as usize).saturating_sub(under.len() + tune.iter().len() + 1);
    for l in under.into_iter().chain(tune).chain([foot]) {
        put(f, area, &mut y, l);
    }
    ask(f, |w| asking(s, v, w));
}

/// The lines of the question the board is asking, if it is asking one:
/// what it is about, then the question or what you are typing, then the
/// keys that answer it; none are no wider than `w`.
fn asking(s: &Screen, v: &View, w: usize) -> Vec<Line<'static>> {
    let says = |text: String, color: Color| Line::styled(text, fg(color));
    let mut lines = vec![];
    match &v.mode {
        Mode::Confirm { iid, plan } => {
            // What Claude said of a landing's MR going out with the one
            // below, where you decide to merge it; the panel says it of
            // one ot merges itself.
            let merges = matches!(plan.writes.first(), Some(Write::Merge { .. } | Write::AutoMerge { .. }));
            if let Some(why) = s.landing.get(iid).and_then(|l| l.why.as_ref()).filter(|_| merges) {
                lines.extend(wrap(&format!("Claude: {why}"), w).into_iter().map(|l| says(l, DIM)));
                lines.push(Line::default());
            }
            lines.push(says(plan.question.clone().unwrap_or_default(), AMBER));
            lines.push(Line::default());
            lines.push(key_line(&[("y", " yes  "), ("n", " no")]));
        }
        Mode::Rebase { ready, .. } => {
            for l in &ready.said {
                let (lead, color) = if l.detail { ("  ", DIM) } else { ("", TEXT) };
                for part in wrap(&l.text, w.saturating_sub(lead.len())) {
                    lines.push(Line::from(vec![Span::raw(lead), Span::styled(part, fg(color))]));
                }
            }
            lines.push(Line::default());
            lines.push(says(ready.question.clone(), AMBER));
            lines.push(Line::default());
            lines.push(key_line(&[("y", " yes  "), ("esc", " later  "), ("n", " drop it")]));
        }
        Mode::Land(l) => {
            lines.push(says(format!("land {}?", l.names()), AMBER));
            let how = "one at a time from the bottom. Claude reads them for which can go out in the same deploy as the one below; the rest wait for it to be live in production. once one merges, the next is rebased onto main past it, and so is every other MR of yours that was on it, each on its own; the next one's row says when it is ready to land, and merges once you say y there; but one Claude said can go out with the one below, ot merges itself once it is approved and its pipeline passes. a rebase Claude settled conflicts in waits for you";
            lines.extend(wrap(how, w).into_iter().map(|part| says(part, DIM)));
            lines.push(Line::default());
            lines.push(key_line(&[("y", " yes  "), ("n", " no")]));
        }
        Mode::Approve { iid } => {
            lines.push(says(format!("approve {}?", bang(*iid)), AMBER));
            lines.push(Line::default());
            lines.push(key_line(&[("y", " yes  "), ("m", " with a note  "), ("n", " no")]));
        }
        Mode::Note { iid, text } => {
            lines.push(says(format!("approve {} saying:", bang(*iid)), DIM));
            lines.extend(typed(text, w));
            lines.push(Line::default());
            lines.push(key_line(&[("enter", " approves  "), ("esc", " drops")]));
        }
        Mode::Typing(t) => {
            let what = match t.sets {
                Sets::Reviewers => "reviewers for",
                Sets::Cc { .. } => "cc on",
            };
            lines.push(says(format!("{what} {}:", bang(t.iid)), DIM));
            lines.extend(typed(&t.text, w));
            // Who fits the name being typed, a row each, the rows there
            // whether or not anyone fits, so the box keeps its height.
            let fits = match model::split_at_typing(&t.text).1.trim() {
                "" => vec![],
                _ => model::matches(&t.text, s.people),
            };
            lines.extend((0..SUGGESTED).map(|i| says(fits.get(i).map_or("", |p| p).to_string(), DIM)));
            lines.push(Line::default());
            lines.push(key_line(&[("tab", " completes  "), ("enter", " sets  "), ("esc", " drops")]));
        }
        Mode::Tell { to, text } => {
            lines.push(says(format!("tell {}:", to.name), DIM));
            lines.extend(typed(text, w));
            lines.push(Line::default());
            lines.push(key_line(&[("enter", " sends  "), ("esc", " drops")]));
        }
        Mode::Rename { to, text } => {
            lines.push(says(format!("rename {}:", to.name), DIM));
            lines.extend(typed(text, w));
            lines.push(Line::default());
            lines.push(key_line(&[("enter", " renames  "), ("esc", " drops")]));
        }
        Mode::Blurb { .. } => {}
        Mode::NewThread(text) => {
            lines.push(says("a new T3 thread, in this checkout's project. what should it do?".into(), DIM));
            lines.extend(typed(text, w));
            lines.push(Line::default());
            lines.push(key_line(&[("enter", " starts it  "), ("esc", " drops")]));
        }
        Mode::Stop(to) => {
            lines.push(says(format!("stop {}? it presses the agent's stop key", to.name), AMBER));
            lines.push(Line::default());
            lines.push(key_line(&[("y", " yes  "), ("n", " no")]));
        }
        Mode::Unpushed(l) => {
            if let Some(why) = &l.why {
                lines.extend(wrap(why, w).into_iter().map(|part| says(part, DIM)));
                lines.push(Line::default());
            }
            lines.push(says(format!("your change to {} isn't pushed:", bang(l.iid)), AMBER));
            lines.extend(wrap(&l.change.files.join(", "), w).into_iter().map(|part| says(part, TEXT)));
            let on = format!("on {} \"{}\"", &l.change.on[..l.change.on.len().min(7)], l.change.subject);
            lines.extend(wrap(&on, w).into_iter().map(|part| says(part, DIM)));
            lines.push(Line::default());
            lines.push(key_line(&[("a", " amend that  "), ("c", " new commit  "), ("d", " put aside  "), ("esc", " keep it there")]));
        }
        Mode::Message(l, text) => {
            lines.push(says(format!("a new commit on {}, saying:", bang(l.iid)), DIM));
            lines.extend(typed(text, w));
            lines.push(Line::default());
            lines.push(key_line(&[("enter", " commits and pushes  "), ("esc", " back")]));
        }
        Mode::Board | Mode::Help => {}
    }
    lines
}

/// What plays, and `hills`' bars along the rest of the line.
fn tune_line(p: &Playing, hills: Option<&Hills>, right: usize) -> Line<'static> {
    let mut o = Out::default();
    o.put("  ♪ ", fg(TEAL));
    o.put(&p.title, fg(DIM));
    if !p.artist.is_empty() {
        o.put("  ·  ", fg(FAINT));
        o.put(&p.artist, fg(DIM));
    }
    let Some(hills) = hills else {
        return o.line();
    };
    let strip = right.saturating_sub(o.w + 2).min(32);
    o.pad_to(right.saturating_sub(strip));
    let mut l = o.line();
    l.spans.extend(hills.strip(strip));
    l
}

fn header(s: &Screen, right: usize) -> Line<'static> {
    let mut o = Out::default();
    o.put(" ot", fg(TEAL).bold());
    o.put("  ", Style::new());
    o.put(s.project, fg(DIM));
    o.pad_to(right.saturating_sub(width(s.stamp)).max(o.w + 2));
    o.put(s.stamp, fg(DIM));
    o.line()
}

/// Where a row's move goes, the same for every row, and the right edge its
/// number ends at.
struct Cols {
    move_col: usize,
    right: usize,
    /// The widest MR number, which a ticket's id on a row with none is
    /// kept clear of, so the ids stand in one column.
    num_w: usize,
}

impl Cols {
    fn new(b: &Board, right: usize) -> Cols {
        let rows = || b.tree.iter().chain(&b.claude).chain(&b.not_sent);
        let widest = |f: &dyn Fn(&Row) -> usize| rows().map(f).max().unwrap_or(0);
        let move_w = widest(&|r| match &r.item {
            Item::Mr { mv, agent, .. } => width(&mv.word()).max(agent.as_ref().map_or(0, |_| width(WORKING))),
            Item::Ticket { .. } => width(&ticket_move(&r.item).2),
            Item::Here(_) => width(HERE),
            Item::Claude { state, .. } => width(state.word()),
            Item::Hc { status, .. } => width(hc_look(*status).1),
            Item::Label(_) | Item::Gap => 0,
        });
        let num_w = widest(&|r| match &r.item {
            Item::Mr { iid, .. } => width(&bang(*iid)),
            _ => 0,
        });
        let iid_w = widest(&|r| match &r.item {
            Item::Mr { iid, ticket, .. } => width(&handle(*iid, ticket.as_deref())),
            Item::Ticket { id, .. } => width(id) + 2 + num_w,
            Item::Claude { ago, .. } => width(ago),
            Item::Hc { mr: Some(iid), .. } => width(&bang(*iid)),
            _ => 0,
        });
        let text_end = widest(&|r| match &r.item {
            Item::Mr { branch: t, .. }
            | Item::Ticket { title: t, .. }
            | Item::Here(t)
            | Item::Claude { title: t, .. }
            | Item::Hc { name: t, .. } => text_x(r) + width(t),
            Item::Label(_) | Item::Gap => 0,
        });
        // Next to the longest branch when there is room, so a wide terminal
        // doesn't leave a gap to read across; else as far right as still
        // leaves room for the move and the number.
        let after_longest_branch = text_end + 3;
        let last_that_fits = right.saturating_sub(iid_w + 2 + move_w);
        Cols {
            move_col: after_longest_branch.min(last_that_fits),
            right,
            num_w,
        }
    }
}

/// Where a row's text starts: after its lead, its rails and its glyph.
fn text_x(r: &Row) -> usize {
    LEAD_W + width(&r.rails) + GLYPH_W
}

/// What an MR's row says at its right: its number, after its ticket's id
/// if it is for one of yours.
fn handle(iid: u64, ticket: Option<&str>) -> String {
    match ticket {
        Some(id) => format!("{id}  {}", bang(iid)),
        None => bang(iid),
    }
}

/// A ticket's row's glyph and its color, and what it says in place of a
/// move: the agent on it, else the ticket it waits on, else its state.
fn ticket_move(item: &Item) -> (&'static str, Color, String) {
    let Item::Ticket { state, waits_on, agent, .. } = item else {
        return ("", DIM, String::new());
    };
    match (agent, waits_on) {
        (Some(w), _) => (w.state.glyph(), tone(w.state.tone()), agent_word(w).to_string()),
        (None, Some(b)) => ("·", FAINT, format!("waits on {b}")),
        (None, None) => ("·", FAINT, state.to_lowercase()),
    }
}

/// How one of hc's agents looks on its row: its glyph, what it says at
/// its right, and the colour of both.
fn hc_look(status: Status) -> (&'static str, &'static str, Color) {
    match status {
        Status::Working => ("◐", "working", AMBER),
        Status::Blocked => ("●", "needs you", tone(Tone::Orange)),
        Status::Stopped => ("·", "done", DIM),
    }
}

/// The agent on a row, as a place `t` reads, when ot has its transcript.
fn readable(agent: &Option<Worker>) -> Option<Place> {
    agent.as_ref().filter(|w| w.session.is_some()).map(|_| Place::Agent)
}

fn agent_word(w: &Worker) -> &'static str {
    match (w.state, &w.agent) {
        (State::Busy, _) => WORKING,
        (_, Some(_)) => "agent waiting",
        (_, None) => "Claude waiting",
    }
}

/// What an MR's row says at its right.
enum Says<'a> {
    /// What waits on you there, ready (Doing::on_you).
    Yours(&'a str),
    Doing(&'a str),
    /// Its landing's word, and whether it is ready for you to merge.
    Landing(&'a str, bool),
    /// Claude is busy on it.
    Working,
    Move(Move),
}

/// The first that applies: what is being done to it, what its landing
/// waits on, an agent busy on it, its move.
fn says<'a>(mv: Move, agent: &Option<Worker>, doing: Option<&'a Doing>, landing: Option<&'a Said>) -> Says<'a> {
    match (doing, landing) {
        (Some(d), _) if d.on_you => Says::Yours(&d.word),
        (Some(d), _) => Says::Doing(&d.word),
        (None, Some(Said { word: Some(w), ready, .. })) => Says::Landing(w, *ready),
        (None, _) if agent.as_ref().is_some_and(|w| w.state == State::Busy) => Says::Working,
        (None, _) => Says::Move(mv),
    }
}

/// What each row waits on you for, by what a notification calls the row:
/// an entry that appears or changes is news (App::tell). An MR's is what
/// its row says when that is yours to do, read past work under way on it,
/// so a merge that GitLab is slow to show isn't news again; Claude's is
/// that it stopped.
pub fn waits(snap: &Snapshot, b: &Board, doing: &HashMap<u64, Doing>, landing: &HashMap<u64, Said>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let stopped = |w: &Worker| w.state == State::Idle;
    for r in b.tree.iter().chain(&b.not_sent).chain(&b.claude) {
        match &r.item {
            Item::Mr { iid, branch, mv, agent, .. } => {
                let what = match says(*mv, agent, doing.get(iid).filter(|d| d.on_you), landing.get(iid)) {
                    Says::Yours(w) | Says::Landing(w, true) => Some(w.to_string()),
                    // GitLab looking again at a conflict isn't one yet.
                    Says::Move(mv) if mv.on_you() && !snap.mr(*iid).is_some_and(|m| m.rechecking) => Some(mv.word()),
                    _ => None,
                };
                out.extend(what.map(|w| (format!("{branch} {}", bang(*iid)), w)));
                out.extend(agent.as_ref().filter(|w| stopped(w)).map(|w| (w.name.clone(), agent_word(w).to_string())));
            }
            Item::Ticket { agent: Some(w), .. } if stopped(w) => {
                out.insert(w.name.clone(), agent_word(w).to_string());
            }
            Item::Claude { title, state: State::Idle, .. } => {
                out.insert(title.clone(), State::Idle.word().to_string());
            }
            Item::Hc { name, status: status @ (Status::Blocked | Status::Stopped), .. } => {
                out.insert(name.clone(), hc_look(*status).1.to_string());
            }
            _ => {}
        }
    }
    out
}

/// A line's lead: the margin, then the cursor's mark on the row it is on.
fn lead(o: &mut Out, marked: bool) {
    o.put("  ", Style::new());
    o.put(if marked { "▌ " } else { "  " }, fg(TEAL));
}

fn tree(s: &Screen, v: &View, right: usize) -> (Vec<Line<'static>>, Range<usize>) {
    let b = s.board;
    let mut out: Vec<Line<'static>> = vec![];
    let mut cur = 0..0;
    if b.tree.is_empty() && b.claude.is_empty() && b.not_sent.is_empty() && b.hidden.is_empty() {
        if s.snap.as_of > 0 {
            out.push(Line::styled("   nothing open", fg(DIM)));
        }
        return (out, cur);
    }
    let cols = Cols::new(b, right);
    let rows = |rows: &[Row], out: &mut Vec<Line<'static>>, cur: &mut Range<usize>| {
        for r in rows {
            let on = pick_of(&r.item).is_some_and(|p| v.at.as_ref() == Some(&p));
            let start = out.len();
            row(r, &cols, on, s, out);
            if on {
                *cur = start..out.len();
            }
        }
    };
    if !b.tree.is_empty() {
        out.push(Line::styled(
            format!("   {}", s.snap.default_branch),
            fg(DIM),
        ));
        out.push(Line::styled(format!("{}│", " ".repeat(LEAD_W)), fg(FAINT)));
        rows(&b.tree, &mut out, &mut cur);
    }
    if !b.claude.is_empty() {
        if !out.is_empty() {
            out.push(Line::default());
        }
        out.push(Line::styled("   claude", fg(DIM)));
        rows(&b.claude, &mut out, &mut cur);
    }
    if !b.not_sent.is_empty() {
        out.push(Line::default());
        let on = v.at == Some(Pick::Fold);
        let n = b
            .not_sent
            .iter()
            .filter(|r| matches!(r.item, Item::Mr { .. }))
            .count();
        let mut o = Out::default();
        lead(&mut o, on);
        o.put(if v.fold_open { "▾ " } else { "▸ " }, fg(DIM));
        o.put(&format!("{n} not sent"), fg(DIM));
        if on {
            cur = out.len()..out.len() + 1;
        }
        out.push(band(o.line(), on));
        if v.fold_open {
            rows(&b.not_sent, &mut out, &mut cur);
        }
    }
    let n = b.hidden.iter().filter(|r| matches!(r.item, Item::Mr { .. })).count();
    if n > 0 {
        out.push(Line::default());
        let (what, hint) = if v.hidden_open { ("hidden", "h brings one back · H hides them") } else { ("hidden", "H shows them") };
        out.push(Line::from(vec![Span::styled(format!("   {n} {what}"), fg(DIM)), Span::styled(format!("  ·  {hint}"), fg(FAINT))]));
        if v.hidden_open {
            rows(&b.hidden, &mut out, &mut cur);
        }
    }
    (out, cur)
}

fn band(l: Line<'static>, on: bool) -> Line<'static> {
    if on {
        l.style(Style::new().bg(BAND))
    } else {
        l
    }
}

/// A row as lines: its rails, glyph and text (wrapped under itself when
/// long), and on the first line its move and its number.
fn row(
    r: &Row,
    cols: &Cols,
    on: bool,
    s: &Screen,
    out: &mut Vec<Line<'static>>,
) {
    let (glyph, glyph_fg, text, iid) = match &r.item {
        Item::Gap | Item::Label(_) => {
            let mut o = Out::default();
            lead(&mut o, false);
            o.put(&r.rails, fg(FAINT));
            if let Item::Label(name) = &r.item {
                o.put(name, fg(DIM).bold());
            }
            out.push(o.line());
            return;
        }
        Item::Mr { iid, branch, mv, ticket, .. } => (mv.glyph(), tone(mv.tone()), branch, handle(*iid, ticket.as_deref())),
        Item::Ticket { id, title, .. } => {
            let (glyph, color, _) = ticket_move(&r.item);
            (glyph, color, title, format!("{id}  {}", " ".repeat(cols.num_w)))
        }
        Item::Here(b) => ("◆", TEAL, b, String::new()),
        Item::Claude {
            title, state, ago, ..
        } => (state.glyph(), tone(state.tone()), title, ago.clone()),
        Item::Hc { name, status, mr, .. } => {
            let (glyph, _, color) = hc_look(*status);
            (glyph, color, name, mr.map_or(String::new(), bang))
        }
    };
    // What is being done to it says so in place of its move, and what its
    // landing waits on, under that; then an agent busy on it.
    let (word, word_fg) = match &r.item {
        Item::Mr { iid, mv, agent, .. } => match says(*mv, agent, s.doing.get(iid), s.landing.get(iid)) {
            Says::Yours(w) => (w.to_string(), TEAL),
            Says::Doing(w) => (format!("{w}…"), AMBER),
            Says::Landing(w, ready) => (w.to_string(), if ready { TEAL } else { GREEN }),
            Says::Working => (WORKING.to_string(), AMBER),
            Says::Move(mv) => (mv.word(), word_color(mv)),
        },
        Item::Ticket { .. } => {
            let (_, color, word) = ticket_move(&r.item);
            (word, if color == FAINT { DIM } else { color })
        }
        Item::Claude { state, .. } => (state.word().to_string(), tone(state.tone())),
        Item::Hc { status, .. } => {
            let (_, word, color) = hc_look(*status);
            (word.to_string(), color)
        }
        _ => (HERE.to_string(), DIM),
    };
    let room = cols
        .move_col
        .saturating_sub(2 + text_x(r))
        .max(MIN_BRANCH_W);
    // What one of hc's agents says it is on, after its name and wrapped
    // under itself there, when the row has the room.
    let said_x = text_x(r) + width(text) + 2;
    let said = match &r.item {
        Item::Hc { said, .. } if !said.is_empty() && cols.move_col >= said_x + 2 + MIN_SAID_W => wrap(said, cols.move_col - 2 - said_x),
        _ => vec![],
    };
    let parts = wrap(text, room);
    for i in 0..parts.len().max(said.len()) {
        let part = parts.get(i).map_or("", |p| p);
        let first = i == 0;
        let mut o = Out::default();
        lead(&mut o, on && first);
        if first {
            o.put(&r.rails, fg(FAINT));
            o.put(glyph, fg(glyph_fg));
            o.put(" ", Style::new());
        } else {
            o.put(&r.wrap_rails, fg(FAINT));
        }
        o.put(part, fg(TEXT));
        if let Some(said) = said.get(i) {
            o.pad_to(said_x);
            o.put(said, fg(DIM));
        }
        if first {
            o.pad_to(cols.move_col.max(o.w + 2));
            o.put(&word, fg(word_fg));
            o.pad_to(cols.right.saturating_sub(width(&iid)).max(o.w + 2));
            o.put(&iid, fg(DIM));
        }
        out.push(band(o.line(), on));
    }
}

/// A move's word in its glyph's color, but a draft's faint glyph would
/// leave its word unreadable.
fn word_color(mv: Move) -> Color {
    tone(if mv.tone() == Tone::Faint {
        Tone::Dim
    } else {
        mv.tone()
    })
}

fn rule(right: usize) -> Line<'static> {
    Line::styled(
        format!(" {}", "─".repeat(right.saturating_sub(1))),
        fg(FAINT),
    )
}

fn panel_lines(s: &Screen, v: &View, right: usize) -> Vec<Line<'static>> {
    let mut out = vec![rule(right)];
    let text = right.saturating_sub(2);
    match &v.at {
        Some(Pick::Mr(iid)) => {
            let iid = *iid;
            let Some(mr) = s.snap.mr(iid) else {
                return vec![];
            };
            let agent = match v.item(s.board) {
                Some(Item::Mr { agent, .. }) => agent.as_ref(),
                _ => None,
            };
            let p = panel(mr, s.snap, s.rules, agent);
            out.extend(title_lines(&p.title, &bang(iid), right));
            // Work under way on it, the whole of what it is at; and its
            // landing, if it has come to it. The panel is a glance: Claude's
            // reason for the landing waits for the merge question, unless ot
            // merges it itself and asks none, and the ticket's comments for
            // the ticket (l).
            let landing = s.landing.get(&iid).map(|l| match (l.ready, &l.why) {
                (true, _) => format!("landing {}: enter merges this one, once you say y; L stops it", l.names),
                (false, Some(why)) if l.itself => format!("landing {}: ot merges this one itself; L stops it. Claude: {why}", l.names),
                (false, _) => format!("landing {}; L stops it", l.names),
            });
            let detail = s.doing.get(&iid).map(|d| d.detail.clone()).filter(|d| !d.is_empty());
            for line in detail.into_iter().chain(landing) {
                out.extend(prose(&line, AMBER, text));
            }
            // An agent busy on it says so in place of its move, as its row does.
            let head = match agent {
                Some(w) if w.state == State::Busy => Span::styled(format!("◐ {WORKING}"), fg(AMBER)),
                _ => Span::styled(format!("{} {}", p.mv.glyph(), p.mv.word()), fg(word_color(p.mv))),
            };
            let mut facts = vec![head];
            facts.extend(p.facts.into_iter().map(|f| Span::styled(f, fg(TEXT))));
            out.extend(pack(&facts, text).into_iter().map(indent));
            out.extend(agent.into_iter().flat_map(|w| agent_lines(w, s, text)));
        }
        Some(Pick::Ticket(id)) => {
            let (Some(t), Some(item)) = (s.tickets.iter().find(|t| t.id == *id), v.item(s.board)) else {
                return vec![];
            };
            out.extend(title_lines(&t.title, id, right));
            let (glyph, color, word) = ticket_move(item);
            let color = if color == FAINT { DIM } else { color };
            let mut facts = vec![Span::styled(format!("{glyph} {word}"), fg(color)), Span::styled(t.state.clone(), fg(TEXT))];
            let agent = match item {
                Item::Ticket { agent, .. } => agent.as_ref(),
                _ => None,
            };
            let move_names_one = matches!(item, Item::Ticket { agent: None, waits_on: Some(_), .. });
            if t.blocked_by.len() > usize::from(move_names_one) {
                facts.push(Span::styled(format!("waits on {}", t.blocked_by.join(", ")), fg(TEXT)));
            }
            facts.push(Span::styled(format!("in {}", t.project), fg(TEXT)));
            facts.push(Span::styled("no MR yet".to_string(), fg(TEXT)));
            out.extend(pack(&facts, text).into_iter().map(indent));
            out.extend(agent.into_iter().flat_map(|w| agent_lines(w, s, text)));
            // With no MR to say where it stands, the newest comment does,
            // its first line.
            if let Some(first) = t.comment.lines().map(str::trim).find(|l| !l.is_empty()) {
                let said = format!("{}, {}: {first}", t.id, model::ago(s.now - t.commented));
                out.extend(prose(&said, DIM, text));
            }
        }
        Some(Pick::Session(id)) => {
            let row = s.board.claude.iter().find_map(|r| match &r.item {
                Item::Claude {
                    id: i,
                    title,
                    doing,
                    facts,
                    ..
                } if i == id => Some((title, doing, facts)),
                _ => None,
            });
            let Some((title, doing, facts)) = row else {
                return vec![];
            };
            out.extend(prose(title, TEXT, text));
            out.extend(prose(doing, DIM, text));
            let facts: Vec<Span<'static>> = facts
                .iter()
                .map(|f| Span::styled(f.clone(), fg(TEXT)))
                .collect();
            out.extend(pack(&facts, text).into_iter().map(indent));
        }
        Some(Pick::Hc(address)) => {
            let Some(Item::Hc { name, status, said, mr, .. }) = v.item(s.board) else {
                return vec![];
            };
            let at = if crate::t3::owns(address) { "t3".to_string() } else { format!("hc {address}") };
            out.extend(title_lines(name, &at, right));
            out.extend(prose(said, DIM, text));
            let (glyph, word, color) = hc_look(*status);
            let state = Span::styled(format!("{glyph} {word}"), fg(color));
            let on = mr.map(|iid| Span::styled(format!("on {}", bang(iid)), fg(TEXT)));
            let facts: Vec<Span<'static>> = [state].into_iter().chain(on).collect();
            out.extend(pack(&facts, text).into_iter().map(indent));
        }
        Some(Pick::Fold) => {
            let facts = [
                "yours, and nobody was asked to review them".to_string(),
                format!("quiet for {} days or more", model::QUIET / model::DAY),
            ]
            .map(|f| Span::styled(f, fg(TEXT)));
            out.extend(pack(&facts, text).into_iter().map(indent));
        }
        None => return vec![],
    }
    if let Some((item, at)) = v.item(s.board).and_then(|i| Some((i, Stage::of(i)?))) {
        let busy = matches!(item, Item::Mr { agent: Some(w), .. } | Item::Ticket { agent: Some(w), .. } if w.state == State::Busy);
        out.push(Line::default());
        out.extend(track(at, if busy { AMBER } else { TEAL }, text));
    }
    out.push(rule(right));
    out
}

/// Where a ticket of yours has got, `at` on the track from its ticket to
/// live: the stations passed and the line up to it in `color`, the rest
/// faint, each station's name under it.
fn track(at: Stage, color: Color, text: usize) -> Vec<Line<'static>> {
    // Five gaps between six stations, the last name four wide.
    let gap = (text.saturating_sub(4) / 5).clamp(7, 10);
    let (mut line, mut names) = (Out::default(), Out::default());
    line.put("  ", Style::new());
    for (i, stage) in Stage::ALL.into_iter().enumerate() {
        let style = fg(if stage <= at { color } else { FAINT });
        if i > 0 {
            line.put(&if stage <= at { "━" } else { "─" }.repeat(gap - 1), style);
        }
        line.put(if stage <= at { "●" } else { "○" }, style);
        names.pad_to(2 + gap * i);
        names.put(stage.word(), fg(if stage == at { color } else { DIM }));
    }
    vec![line.line(), names.line()]
}

/// A title wrapped under itself, `handle` at the right of its first line.
fn title_lines(title: &str, handle: &str, right: usize) -> Vec<Line<'static>> {
    let text = right.saturating_sub(2);
    let mut out = vec![];
    for (i, l) in wrap(title, text.saturating_sub(width(handle) + 2)).into_iter().enumerate() {
        let mut o = Out::default();
        o.put("  ", Style::new());
        o.put(&l, fg(TEXT));
        if i == 0 {
            o.pad_to(right.saturating_sub(width(handle)));
            o.put(handle, fg(DIM));
        }
        out.push(o.line());
    }
    out
}

/// What the panel says of the agent on a row: what it is on while it
/// works, and once it stops, Haiku's line on where its work stands.
fn agent_lines(w: &Worker, s: &Screen, text: usize) -> Vec<Line<'static>> {
    let (state, said, color) = match w.state {
        State::Busy => ("working", w.doing.as_str(), AMBER),
        _ => ("waiting", s.briefs.of(w).unwrap_or("Haiku reading…"), DIM),
    };
    let said = if said.is_empty() { String::new() } else { format!(": {said}") };
    prose(&format!("{} {state}, {}{said}", w.name, model::ago(s.now - w.at)), color, text)
}

/// `said` wrapped to `text` columns, each line under the panel's margin.
fn prose(said: &str, color: Color, text: usize) -> Vec<Line<'static>> {
    wrap(said, text)
        .into_iter()
        .map(|part| {
            let mut o = Out::default();
            o.put("  ", Style::new());
            o.put(&part, fg(color));
            o.line()
        })
        .collect()
}

fn indent(l: Line<'static>) -> Line<'static> {
    let mut spans = vec![Span::raw("  ")];
    spans.extend(l.spans);
    Line::from(spans)
}

/// The footer, as the board's mode has it.
fn footer(s: &Screen, v: &View, right: usize) -> Line<'static> {
    let mut o = Out::default();
    o.put("  ", Style::new());
    match &v.mode {
        Mode::Help => o.put("any key goes back", fg(DIM)),
        // A question is asked in its own box over the board, with its own
        // keys; the footer says nothing that could be taken for them.
        Mode::Confirm { .. }
        | Mode::Rebase { .. }
        | Mode::Land(_)
        | Mode::Approve { .. }
        | Mode::Note { .. }
        | Mode::Typing(_)
        | Mode::Tell { .. }
        | Mode::Rename { .. }
        | Mode::Blurb { .. }
        | Mode::NewThread(_)
        | Mode::Stop(_)
        | Mode::Unpushed(_)
        | Mode::Message(..) => {}
        Mode::Board => {
            match s.note {
                Some(n) => o.put(n, fg(DIM)),
                None => board_keys(&mut o, s, v, right),
            }
            o.pad_to(right.saturating_sub(width(KEYS_HINT)));
            put_keys(&mut o, &[("?", " keys")]);
        }
    }
    o.line()
}

const KEYS_HINT: &str = "? keys";

/// Keys and what they do, the keys bright.
fn put_keys(o: &mut Out, keys: &[(&str, &str)]) {
    for (k, does) in keys {
        o.put(k, fg(TEXT));
        o.put(does, fg(DIM));
    }
}

/// The keys that work on the row, as many as fit: enter, then the row's
/// own move (what it wants is what you'd press), then link and web, which
/// you press most, then the rest in the table's order.
fn board_keys(o: &mut Out, s: &Screen, v: &View, right: usize) {
    let wanted = match &v.at {
        Some(Pick::Mr(iid)) => s
            .snap
            .mr(*iid)
            .and_then(|m| model::move_of(m, &s.snap.me, s.rules).act()),
        _ => None,
    };
    let mut ks: Vec<&keys::Key> = keys::footer(&v.places(s.snap, s.board)).collect();
    ks.sort_by_key(|k| {
        (
            k.key != "enter",
            Some(k.act) != wanted,
            !matches!(k.act, Act::Link | Act::Web),
        )
    });
    let end = right.saturating_sub(width(KEYS_HINT));
    for k in ks {
        if o.w + width(k.key) + 1 + width(k.word) + 2 > end {
            break;
        }
        put_keys(o, &[(k.key, &format!(" {}  ", k.word))]);
    }
}

fn help() -> Vec<Line<'static>> {
    keys::help(keys::BOARD)
        .map(|k| {
            let mut o = Out::default();
            o.put("   ", Style::new());
            o.put(k.key, fg(TEXT));
            o.pad_to(HELP_COL);
            o.put(k.help, fg(DIM));
            o.line()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{board, fixture};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn frame(w: u16, h: u16, step: impl Fn(&Board, &mut View)) -> String {
        frame_of((fixture::snapshot(), vec![], fixture::sessions()), &HashMap::new(), w, h, step)
    }

    fn frame_of(
        (snap, tickets, sessions): (Snapshot, Vec<Ticket>, Vec<crate::claude::Session>),
        landing: &HashMap<u64, Said>,
        w: u16,
        h: u16,
        step: impl Fn(&Board, &mut View),
    ) -> String {
        let rules = fixture::rules();
        let b = board(&snap, &rules, &HashMap::new(), &sessions, &[], &tickets, fixture::NOW);
        let mut briefs = Briefs::default();
        for (w, _) in briefs.due(&b, &sessions) {
            briefs.heard(&w, "Line numbers show in errors from 4f1c2d9; it waits on you to say whether the backfill (AP-5) goes in the same MR.".into());
        }
        let mut v = View::default();
        v.top(&b);
        step(&b, &mut v);
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        let doing = HashMap::from([(102, Doing::word("rebasing"))]);
        let s = Screen {
            snap: &snap,
            board: &b,
            rules: &rules,
            project: "team/app",
            stamp: "as of 13:44",
            error: None,
            note: None,
            doing: &doing,
            landing,
            people: &[],
            tickets: &tickets,
            briefs: &briefs,
            now: fixture::NOW,
            playing: None,
            hills: &Hills::default(),
        };
        t.draw(|f| draw(f, &s, &mut v)).unwrap();
        crate::ui::text(&t)
    }

    #[test]
    fn an_agent_hc_runs_takes_the_cursor_and_its_stop_key_only_on_a_turn() {
        let snap = fixture::snapshot();
        let agent = |address: &str, name: &str, status: Status, at: i64| Agent {
            address: address.into(),
            name: name.into(),
            status,
            said: String::new(),
            at: fixture::NOW - at,
            pid: None,
            branch: String::new(),
        };
        let agents = [agent("mini/t3s", "ap-1000", Status::Working, 60), agent("mini/t9", "ap-969", Status::Stopped, 120)];
        let b = board(&snap, &fixture::rules(), &HashMap::new(), &[], &agents, &[], fixture::NOW);
        // The fold of MRs not sent is the last row, under hc's agents.
        let mut v = View::default();
        v.bottom(&b);
        v.step(&b, -1);
        assert_eq!(v.at, Some(Pick::Hc("mini/t9".into())));
        assert_eq!(keys::act("enter", &v.places(&snap, &b)), Some(Act::Attach));
        assert_eq!(keys::act("x", &v.places(&snap, &b)), None, "it is on no turn to stop");
        v.step(&b, -1);
        assert_eq!(v.at, Some(Pick::Hc("mini/t3s".into())));
        assert_eq!(keys::act("x", &v.places(&snap, &b)), Some(Act::Stop));
    }

    #[test]
    fn a_merged_row_of_yours_takes_only_the_keys_that_read_it_and_cc() {
        let mut snap = fixture::snapshot();
        snap.mrs.iter_mut().find(|m| m.iid == 105).unwrap().reach = Some(crate::model::Reach::Deploying);
        let v = View {
            at: Some(Pick::Mr(105)),
            ..View::default()
        };
        let at = v.places(&snap, &Board::default());
        assert_eq!(at, [Place::Board, Place::Mr, Place::MyMerged]);
        assert_eq!(keys::act("c", &at), Some(Act::Cc));
        assert_eq!(keys::act("m", &at), None);
    }

    #[test]
    fn a_row_is_news_once_what_it_says_is_yours_to_do() {
        let none = HashMap::new();
        let of = |snap: &Snapshot, doing: &HashMap<u64, Doing>| {
            let b = board(snap, &fixture::rules(), &HashMap::new(), &fixture::sessions(), &[], &[], fixture::NOW);
            waits(snap, &b, doing, &HashMap::new())
        };
        let said = |w: &BTreeMap<String, String>, row: &str| w.get(row).cloned();
        let snap = fixture::snapshot();
        let was = of(&snap, &none);
        // A pipeline still running waits on nobody; broken, on you.
        assert_eq!(said(&was, "warm-cache !107"), None);
        let mut broke = snap.clone();
        broke.mrs.iter_mut().find(|m| m.iid == 107).unwrap().pipeline = "failed".into();
        assert_eq!(said(&of(&broke, &none), "warm-cache !107"), Some("pipeline broke".into()));
        // While it merges it is no news, and none again if GitLab is slow
        // to show it merged; a rebase parked for your y is.
        assert_eq!(said(&was, "retry-budget !105"), Some("merge it".into()));
        let merging = HashMap::from([(105, Doing::word("merging"))]);
        assert_eq!(of(&snap, &merging), was);
        let parked = HashMap::from([(105, Doing { word: "rebase ready".into(), detail: String::new(), on_you: true })]);
        assert_eq!(said(&of(&snap, &parked), "retry-budget !105"), Some("rebase ready".into()));
        // Claude, once it has stopped.
        assert_eq!(said(&was, "Why the parser's error paths read better apart"), Some("waiting on you".into()));
    }

    #[test]
    fn board_at_80x24() {
        insta::assert_snapshot!(frame(80, 24, |_, _| {}));
    }

    #[test]
    fn board_at_80x24_scrolled_to_the_open_fold() {
        insta::assert_snapshot!(frame(80, 24, |b, v| {
            v.bottom(b);
            v.fold_open = true;
            v.bottom(b);
        }));
    }

    #[test]
    fn a_rebase_asks_in_a_box_over_the_board() {
        use crate::rebase::{Rebased, Said};
        insta::assert_snapshot!(frame(100, 30, |_, v| {
            v.mode = Mode::Rebase {
                iid: 102,
                ready: Rebased {
                    layers: vec![],
                    said: [
                        (false, "!102  Change 102"),
                        (true, "2 commits onto main, 5 new there"),
                        (true, "Claude settled abc1234 in parser.ts: kept main's new error type and raised the parser's errors as it"),
                        (false, "it is pushed with a new pipeline; your branch here follows"),
                    ]
                    .map(|(detail, text)| Said { detail, text: text.into() })
                    .into(),
                    question: "rebase it?".into(),
                    left_out: None,
                    settled: 0,
                    changed: vec![],
                },
            }
        }));
    }

    #[test]
    fn claude_says_why_a_landing_can_go_where_you_say_merge_not_on_the_board() {
        let snap = fixture::snapshot();
        let mr = snap.mr(105).unwrap().clone();
        let why = "It only reads the column !104 adds once it is there.";
        let landing = HashMap::from([(105, Said { names: "!104, !105".into(), word: None, ready: true, why: Some(why.into()), itself: false })]);
        let board = frame_of((snap.clone(), vec![], vec![]), &landing, 100, 30, |_, v| v.at = Some(Pick::Mr(105)));
        assert!(!board.contains("only reads"), "the panel leaves it out:\n{board}");
        insta::assert_snapshot!(frame_of((snap, vec![], vec![]), &landing, 100, 30, |_, v| {
            v.at = Some(Pick::Mr(105));
            v.mode = Mode::Confirm { iid: 105, plan: model::plan(Act::Merge, &mr).unwrap() };
        }));
    }

    #[test]
    fn the_names_you_type_are_asked_in_a_box_not_the_footer() {
        insta::assert_snapshot!(frame(80, 24, |_, v| {
            v.mode = Mode::Typing(Typing {
                iid: 102,
                text: "maggie@acme.com alex".into(),
                cycling: None,
                sets: Sets::Cc { description: String::new() },
            })
        }));
    }

    #[test]
    fn your_change_left_in_the_review_is_asked_about_naming_what_an_amend_rewrites() {
        insta::assert_snapshot!(frame(80, 24, |_, v| {
            v.mode = Mode::Unpushed(Left {
                iid: 102,
                branch: "sam/sum".into(),
                change: Change { on: "1a2b3c4d5e".into(), subject: "Sum with reduce".into(), files: vec!["src/sum.ts".into(), "src/total.ts".into()] },
                why: Some("GitLab's sam/sum had moved on to 9f8e7d6 \"theirs\", so nothing was pushed; your change is on it now, for you to look at before it goes".into()),
            })
        }));
    }

    #[test]
    fn board_at_160x48_on_a_wrapped_branch() {
        insta::assert_snapshot!(frame(160, 48, |b, v| v.step(b, 5)));
    }

    #[test]
    fn a_swarm_on_the_board_with_its_agent_and_ticket_in_the_panel() {
        insta::assert_snapshot!(frame_of(fixture::swarm(), &HashMap::new(), 110, 44, |b, v| {
            while v.at != Some(Pick::Mr(302)) {
                v.step(b, 1);
            }
        }));
    }

    #[test]
    fn an_agent_that_stopped_says_where_it_stands_in_haikus_words() {
        insta::assert_snapshot!(frame_of(fixture::swarm(), &HashMap::new(), 110, 44, |_, v| {
            v.at = Some(Pick::Ticket("AP-4".into()));
        }));
    }

    #[test]
    fn a_ticket_with_no_mr_yet_is_read_on_the_web_and_its_agent_with_t() {
        let (snap, tickets, sessions) = fixture::swarm();
        let b = board(&snap, &fixture::rules(), &HashMap::new(), &sessions, &[], &tickets, fixture::NOW);
        let v = View { at: Some(Pick::Ticket("AP-4".into())), ..View::default() };
        let at = v.places(&snap, &b);
        assert_eq!(at, [Place::Board, Place::Ticket, Place::Agent]);
        assert_eq!(keys::act("w", &at), Some(Act::Web));
        assert_eq!(keys::act("t", &at), Some(Act::Agent));
        assert_eq!(keys::act("m", &at), None, "nothing to merge yet");
        let v = View { at: Some(Pick::Mr(302)), ..View::default() };
        assert_eq!(keys::act("i", &v.places(&snap, &b)), Some(Act::Ticket), "an MR for a ticket opens it with i");
        assert_eq!(keys::act("l", &v.places(&snap, &b)), Some(Act::Describe), "l reads the MR's description");
    }

    #[test]
    fn a_tiny_terminal_still_draws() {
        for (w, h) in [(1, 1), (12, 4), (30, 8)] {
            frame(w, h, |b, v| v.step(b, 5));
        }
    }

    #[test]
    fn the_cursor_stays_on_its_row_when_the_board_changes() {
        let snap = fixture::snapshot();
        let b = board(&snap, &fixture::rules(), &HashMap::new(), &[], &[], &[], fixture::NOW);
        let mut v = View::default();
        v.top(&b);
        v.step(&b, 3);
        assert_eq!(v.at, Some(Pick::Mr(104)));
        let mut less = snap.clone();
        less.mrs.retain(|m| m.iid != 101 && m.iid != 104);
        let b2 = board(&less, &fixture::rules(), &HashMap::new(), &[], &[], &[], fixture::NOW);
        v.settle(&b2);
        assert_eq!(
            v.at,
            Some(Pick::Mr(106)),
            "104 went, so the cursor lands on what took its place"
        );
        v.step(&b2, -1);
        v.settle(&b);
        assert_eq!(v.at, Some(Pick::Mr(105)));
    }
}
