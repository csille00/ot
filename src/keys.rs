//! Every key ot answers to, in one table. The event loop, the footer and the
//! help screen all read it, so a key is added in one place.
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What a key does. The review's acts cross to the nvim plugin by name
/// (nvim.rs sends `format!("{:?}", act)`, and ot.lua's list_acts and
/// code_acts are keyed by it), so renaming a variant here unbinds its key
/// there without an error: rename both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Down,
    Up,
    Top,
    Bottom,
    Unfold,
    Fold,
    Hide,
    Chat,
    ReadTour,
    ChatAdd,
    ThreadPane,
    ShowHidden,
    Link,
    /// Copy a symbol and its path from the repo root: "total in src/sum.ts".
    Copy,
    Web,
    Refresh,
    Review,
    /// Open the review with Claude's tour of the change (tour.rs).
    Tour,
    /// The tour's next and previous step.
    NextStep,
    PrevStep,
    /// Read one of Claude's sessions.
    Read,
    /// Read the agent at work on a row.
    Agent,
    /// Take over the terminal of one of hc's agents.
    Attach,
    /// Type something to it.
    Tell,
    /// Press its stop key.
    Stop,
    /// Settle a T3 thread.
    Settle,
    /// A shell where the session or thread being read works.
    Terminal,
    /// The conversation in nvim, to select and yank.
    Select,
    /// Rename one, or start a new one.
    Rename,
    NewThread,
    /// Find any T3 thread by its title or what was said in it.
    Find,
    /// The thread list beside a conversation: into it and out, shown or not.
    Threads,
    Sidebar,
    NextThread,
    PrevThread,
    /// The MR panel beside a thread: shown or not, and its diff scrolled.
    MrPanel,
    DiffDown,
    DiffUp,
    /// Focus the part of a conversation's view to the left or right: its thread list,
    /// the conversation, its MR.
    FocusLeft,
    FocusRight,
    /// Read an MR's title and description over the board.
    Describe,
    /// Open a row's ticket in Linear.
    Ticket,
    /// The music screen.
    Music,
    PlayPause,
    NextTrack,
    PrevTrack,
    SeekForward,
    SeekBack,
    Louder,
    Softer,
    Shuffle,
    /// Search YouTube.
    Search,
    PlayNow,
    PlayNext,
    Into,
    Open,
    Collapse,
    NextHunk,
    PrevHunk,
    /// The review's next and previous comment, in the whole change.
    NextComment,
    PrevComment,
    PageDown,
    PageUp,
    LineDown,
    LineUp,
    Tab,
    Aside,
    Viewed,
    Merge,
    Rebase,
    /// Land the stack up to it (land.rs).
    Land,
    Close,
    Ready,
    Pipeline,
    Retry,
    Reviewers,
    /// Who its description cc's.
    Cc,
    Approve,
    Submit,
    Comment,
    CommentGone,
    Reply,
    Resolve,
    Leave,
    Help,
    Quit,
}

/// Where the cursor is, which decides the keys that work. A row can be in
/// several places at once (an MR of yours that is a draft is in Board, Mr,
/// Mine and MyDraft), and a key works in any place it lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// Anywhere on the board.
    Board,
    /// On any MR's row.
    Mr,
    /// On one of yours.
    Mine,
    /// On one of yours that is a draft.
    MyDraft,
    /// On one of yours whose head pipeline failed.
    MyFailed,
    /// On one of yours that merged; it leaves the board once it is live.
    MyMerged,
    /// On someone else's.
    Theirs,
    /// On one of Claude's sessions.
    Session,
    /// On one of your tickets that has no MR yet.
    Ticket,
    /// On an MR for one of your tickets.
    TicketMr,
    /// On a row one of Claude's agents is at work on.
    Agent,
    /// On an agent hc runs, or the session of Claude's that is one.
    Hc,
    /// On one that is no session of Claude's to read.
    HcAlone,
    /// On a session of Claude's that hc runs.
    HcSession,
    /// On a T3 thread, or the session of Claude's that one runs.
    T3,
    /// On one that is on a turn.
    HcWorking,
    /// In one, reading it.
    Transcript,
    /// Reading a T3 thread, which takes a reply.
    Reply,
    /// In the thread list beside a conversation.
    Side,
    /// Reading a thread that has an MR, shown beside it.
    ThreadMr,
    /// In that MR, focused.
    MrFocus,
    /// On the music screen.
    Music,
    /// On a search's results there.
    Hits,
    /// On the fold of MRs not sent, while it is shut.
    FoldShut,
    /// On it while it is open.
    FoldOpen,
    /// In nvim's review, which the plugin maps: its list.
    List,
    /// And its code, which on your own MR you edit: the review's keys
    /// there are none vim edits with, so its own are under <leader>m.
    Code,
    /// Lines of the code you selected.
    Selection,
    /// On the page of Claude's tour.
    Tour,
}

/// Every place on the board, for its help screen.
pub const BOARD: &[Place] = &[
    Place::Board,
    Place::Mr,
    Place::Mine,
    Place::MyDraft,
    Place::MyFailed,
    Place::MyMerged,
    Place::Theirs,
    Place::Session,
    Place::Ticket,
    Place::TicketMr,
    Place::Agent,
    Place::Hc,
    Place::HcAlone,
    Place::HcSession,
    Place::T3,
    Place::HcWorking,
    Place::FoldShut,
    Place::FoldOpen,
];

/// Every place in nvim's review, for the plugin.
/// The places the review in nvim has, whose keys it is sent; the player
/// it opens on M has the music screen's.
pub const REVIEW: &[Place] = &[Place::List, Place::Code, Place::Selection, Place::Tour, Place::Music];

pub struct Key {
    /// As name() writes a key: "j", "G", "enter", "ctrl-d".
    pub key: &'static str,
    /// The footer's one word; empty keeps it off the footer.
    pub word: &'static str,
    pub help: &'static str,
    pub on: &'static [Place],
    pub act: Act,
}

const fn k(
    key: &'static str,
    word: &'static str,
    help: &'static str,
    on: &'static [Place],
    act: Act,
) -> Key {
    Key {
        key,
        word,
        help,
        on,
        act,
    }
}

use Place::*;

pub const KEYS: &[Key] = &[
    k("j", "", "next row", &[Board, List], Act::Down),
    k("k", "", "previous row", &[Board, List], Act::Up),
    k("j", "", "next line", &[Code], Act::Down),
    k("k", "", "previous line", &[Code], Act::Up),
    k("g", "", "first row", &[Board], Act::Top),
    k("G", "", "last row", &[Board], Act::Bottom),
    k("enter", "open", "open the review; on a row whose rebase is ready, see it and say y", &[Mr], Act::Review),
    k("T", "tour", "open the review on Claude's tour: what the change does, then its code in the order it runs", &[Mr], Act::Tour),
    k("P", "", "read Claude's tour in the background; the row says when it's ready, and enter opens the review on it", &[Mr], Act::ReadTour),
    k("n", "", "the tour's next step", &[List], Act::NextStep),
    k("p", "", "the tour's previous step", &[List], Act::PrevStep),
    k("n", "next step", "the next step", &[Tour], Act::NextStep),
    k("p", "previous", "the step before", &[Tour], Act::PrevStep),
    k("enter", "its code", "into the file, at the step you are reading", &[Tour], Act::Into),
    k("T", "thread", "show or hide the conversation of the T3 thread at work on this MR, at the right; i or enter there tells it something", &[List, Tour], Act::ThreadPane),
    k("<leader>ma", "", "add the lines selected to the chat, for your next question to Claude about them", &[Selection], Act::ChatAdd),
    k("C", "chat", "talk with Claude about the change, in a split beside it; it reads the code at the MR's head", &[Tour, List], Act::Chat),
    k("q", "", "back to the board", &[Tour], Act::Leave),
    k("<leader>mn", "", "back to the tour, at its next step", &[Code], Act::NextStep),
    k("<leader>mp", "", "back to the tour, at the step you came from", &[Code], Act::PrevStep),
    k("enter", "read", "read the session", &[Session], Act::Read),
    k("t", "agent", "read what Claude, at work on it, says and does", &[Agent], Act::Agent),
    k("enter", "open", "read a T3 thread here, or take over an hc agent's terminal (ctrl+\\ there comes back)", &[HcAlone], Act::Attach),
    k("a", "attach", "the same on a session hc runs, where enter reads it", &[HcSession], Act::Attach),
    k("s", "tell", "type something to the agent; enter sends it, once nobody is typing there", &[Hc], Act::Tell),
    k("x", "stop", "press the agent's stop key, once you say y", &[HcWorking], Act::Stop),
    k("S", "settle", "settle the T3 thread: T3 marks it done and it leaves the board", &[T3, Reply], Act::Settle),
    k("e", "rename", "rename the T3 thread; enter renames it", &[T3, Reply, Side], Act::Rename),
    k("n", "", "start a new T3 thread in this checkout's project and read it", &[Board, Transcript], Act::NewThread),
    k("n", "new thread", "start a new T3 thread in this checkout's project and read it", &[Side], Act::NewThread),
    k("ctrl-k", "", "find any T3 thread by its title or what was said in it", &[Board, Transcript], Act::Find),
    k("/", "", "the same", &[Board, Transcript], Act::Find),
    k("l", "", "read its title and description; j/k scroll, esc closes", &[Mr], Act::Describe),
    k("h", "hide", "hide it from the board, its whole stack; on a hidden one, bring it back", &[Mr], Act::Hide),
    k("H", "", "show or hide the MRs you hid, under the board", &[Board], Act::ShowHidden),
    k("i", "ticket", "open the ticket it is for in Linear", &[TicketMr], Act::Ticket),
    k("j", "", "down a line", &[Transcript], Act::Down),
    k("k", "", "up a line", &[Transcript], Act::Up),
    k(
        "ctrl-d",
        "",
        "down half a screen",
        &[Transcript],
        Act::PageDown,
    ),
    k("ctrl-u", "", "up half a screen", &[Transcript], Act::PageUp),
    k("{", "", "up half a screen", &[Transcript], Act::PageUp),
    k("}", "", "down half a screen", &[Transcript], Act::PageDown),
    k("g", "", "the start", &[Transcript], Act::Top),
    k(
        "G",
        "end",
        "the end, and stay with it",
        &[Transcript],
        Act::Bottom,
    ),
    k("s", "reply", "reply to the thread in T3; enter sends it, and a busy thread gets it next", &[Reply], Act::Tell),
    k("w", "T3", "open the thread in T3, or copy its link over ssh", &[Reply], Act::Web),
    k("x", "stop", "stop the thread's running turn, once you say y", &[Reply], Act::Stop),
    k("q", "back", "back to the board", &[Transcript], Act::Leave),
    k("v", "select", "the conversation in nvim, read-only: visual mode, search, yank to copy", &[Transcript], Act::Select),
    k("t", "terminal", "a shell where it works: a new zellij pane, or this terminal until you exit", &[Transcript], Act::Terminal),
    k("tab", "threads", "into the thread list beside it, to switch threads", &[Transcript], Act::Threads),
    k("]", "", "the next thread in the list", &[Transcript], Act::NextThread),
    k("[", "", "the thread before in the list", &[Transcript], Act::PrevThread),
    k("b", "", "show or hide the thread list", &[Transcript], Act::Sidebar),
    k("o", "review", "open the review of the thread's MR", &[ThreadMr], Act::Review),
    k("ctrl-f", "", "the MR's diff down a page", &[ThreadMr], Act::DiffDown),
    k("ctrl-b", "", "the MR's diff up a page", &[ThreadMr], Act::DiffUp),
    k("m", "", "show or hide the thread's MR beside it", &[ThreadMr], Act::MrPanel),
    k("ctrl-l", "", "focus the MR beside it; from the thread list, the conversation", &[ThreadMr, Side], Act::FocusRight),
    k("ctrl-h", "", "focus the thread list", &[Transcript], Act::FocusLeft),
    k("backspace", "", "the same, where ctrl-h comes as backspace", &[Transcript], Act::FocusLeft),
    k("j", "", "the diff down a line", &[MrFocus], Act::Down),
    k("k", "", "the diff up a line", &[MrFocus], Act::Up),
    k("down", "", "the diff down a line", &[MrFocus], Act::Down),
    k("up", "", "the diff up a line", &[MrFocus], Act::Up),
    k("ctrl-d", "", "the diff down half a page", &[MrFocus], Act::PageDown),
    k("ctrl-u", "", "the diff up half a page", &[MrFocus], Act::PageUp),
    k("ctrl-f", "", "the diff down half a page", &[MrFocus], Act::PageDown),
    k("ctrl-b", "", "the diff up half a page", &[MrFocus], Act::PageUp),
    k("g", "", "the diff's top", &[MrFocus], Act::Top),
    k("G", "", "the diff's end", &[MrFocus], Act::Bottom),
    k("o", "review", "open the MR's review", &[MrFocus], Act::Review),
    k("ctrl-h", "back", "back to the conversation", &[MrFocus], Act::FocusLeft),
    k("backspace", "", "the same, where ctrl-h comes as backspace", &[MrFocus], Act::FocusLeft),
    k("esc", "", "back to the conversation", &[MrFocus], Act::FocusLeft),
    k("q", "", "back to the board", &[MrFocus], Act::Leave),
    k("j", "", "next thread", &[Side], Act::Down),
    k("k", "", "previous thread", &[Side], Act::Up),
    k("down", "", "next thread", &[Side], Act::Down),
    k("up", "", "previous thread", &[Side], Act::Up),
    k("enter", "open", "read the thread", &[Side], Act::Into),
    k("tab", "back", "back to the conversation", &[Side], Act::Threads),
    k("esc", "", "back to the conversation", &[Side], Act::Threads),
    k("q", "", "back to the board", &[Side], Act::Leave),
    k(
        "M",
        "",
        "music: what cliamp is playing",
        &[Board, List, Code, Music],
        Act::Music,
    ),
    k(
        "space",
        "pause|play",
        "pause, or play again",
        &[Music],
        Act::PlayPause,
    ),
    k("n", "next", "the next track", &[Music], Act::NextTrack),
    k(
        "p",
        "previous",
        "the track before",
        &[Music],
        Act::PrevTrack,
    ),
    k("right", "", "10 seconds on", &[Music], Act::SeekForward),
    k("l", "seek", "10 seconds on", &[Music], Act::SeekForward),
    k("left", "", "10 seconds back", &[Music], Act::SeekBack),
    k("h", "", "10 seconds back", &[Music], Act::SeekBack),
    k("+", "louder", "2 dB louder", &[Music], Act::Louder),
    k("=", "", "2 dB louder", &[Music], Act::Louder),
    k("-", "softer", "2 dB softer", &[Music], Act::Softer),
    k("s", "shuffle", "shuffle, or not", &[Music], Act::Shuffle),
    k("/", "search", "search YouTube", &[Music], Act::Search),
    k("q", "back", "back to the board", &[Music], Act::Leave),
    k("j", "", "next result", &[Hits], Act::Down),
    k("k", "", "previous result", &[Hits], Act::Up),
    k("enter", "play", "play it now", &[Hits], Act::PlayNow),
    k(
        "a",
        "play next",
        "play it after this",
        &[Hits],
        Act::PlayNext,
    ),
    k("esc", "back", "back to the player", &[Hits], Act::Leave),
    k("q", "", "back to the player", &[Hits], Act::Leave),
    k(
        "enter",
        "open",
        "show the MRs nobody was asked to review",
        &[FoldShut],
        Act::Unfold,
    ),
    k(
        "enter",
        "close",
        "fold them away again",
        &[FoldOpen],
        Act::Fold,
    ),
    k("enter", "code", "into the code", &[List], Act::Into),
    k(
        "l",
        "open",
        "open a symbol to what it uses",
        &[List],
        Act::Open,
    ),
    k(
        "h",
        "",
        "close a symbol, or the one it is under",
        &[List],
        Act::Collapse,
    ),
    k(
        "]",
        "hunk",
        "next hunk of this symbol; stops at its last",
        &[List, Code],
        Act::NextHunk,
    ),
    k(
        "[",
        "",
        "previous hunk of this symbol; stops at its first",
        &[List, Code],
        Act::PrevHunk,
    ),
    k("}", "comment", "the next comment in the change, threads and your drafts; from the last, the first", &[List], Act::NextComment),
    k("{", "", "the comment before", &[List], Act::PrevComment),
    k("<leader>m}", "", "the next comment in the change", &[Code], Act::NextComment),
    k("<leader>m{", "", "the comment before", &[Code], Act::PrevComment),
    k(
        "ctrl-d",
        "",
        "scroll the code down",
        &[List, Code],
        Act::PageDown,
    ),
    k(
        "ctrl-u",
        "",
        "scroll the code up",
        &[List, Code],
        Act::PageUp,
    ),
    k("ctrl-e", "", "scroll the code down a line", &[List], Act::LineDown),
    k("ctrl-y", "", "scroll the code up a line", &[List], Act::LineUp),
    k("tab", "", "symbols or files", &[List], Act::Tab),
    k(
        "t",
        "",
        "show or hide tests and generated files",
        &[List],
        Act::Aside,
    ),
    k(
        "v",
        "viewed",
        "mark the row viewed, or not",
        &[List],
        Act::Viewed,
    ),
    k(
        "m",
        "merge",
        "merge it, now or once its pipeline passes and it's approved, once you say y",
        &[Mine],
        Act::Merge,
    ),
    k(
        "b",
        "rebase",
        "rebase it here with the stack it is in, Claude settling conflicts; pushed once you see it and say y",
        &[Mine],
        Act::Rebase,
    ),
    k(
        "R",
        "ready",
        "mark it ready and start a pipeline, once you say y",
        &[MyDraft],
        Act::Ready,
    ),
    k(
        "r",
        "retry",
        "retry its failed jobs",
        &[MyFailed],
        Act::Retry,
    ),
    k(
        "N",
        "pipeline",
        "start a new pipeline",
        &[Mine],
        Act::Pipeline,
    ),
    k(
        "L",
        "land",
        "land it and the MRs under it, one at a time, each once the one below is live in production; again stops it",
        &[Mine],
        Act::Land,
    ),
    k(
        "a",
        "reviewers",
        "change who reviews it",
        &[Mine],
        Act::Reviewers,
    ),
    k(
        "c",
        "cc",
        "change who its description cc's, from your Slack; until it is live in production",
        &[Mine, MyMerged],
        Act::Cc,
    ),
    k(
        "X",
        "close",
        "close it, once you say y",
        &[Mine],
        Act::Close,
    ),
    k("A", "approve", "approve it, once you say how", &[Theirs, List], Act::Approve),
    k(
        "S",
        "submit",
        "finish the review: send your drafts and a comment, not approving",
        &[List],
        Act::Submit,
    ),
    k(
        "<leader>mc",
        "comment",
        "comment on this line; on your draft, change it (saved empty, it goes)",
        &[Code],
        Act::Comment,
    ),
    k(
        "<leader>mc",
        "",
        "comment on the lines selected",
        &[Selection],
        Act::Comment,
    ),
    k(
        "<leader>mC",
        "",
        "comment on the removed lines just above this one",
        &[Code],
        Act::CommentGone,
    ),
    k(
        "<leader>mr",
        "reply",
        "reply to the thread on this line",
        &[Code],
        Act::Reply,
    ),
    k(
        "<leader>mx",
        "resolve",
        "resolve the thread on this line, or reopen it",
        &[Code],
        Act::Resolve,
    ),
    k("y", "link", "copy the MR's link", &[Mr], Act::Link),
    k("y", "link", "copy the ticket's link", &[Ticket], Act::Link),
    k(
        "y",
        "",
        "copy the symbol and its path from the repo root: \"total in src/sum.ts\"",
        &[List],
        Act::Copy,
    ),
    k("w", "web", "open the MR in the browser", &[Mr, List], Act::Web),
    k("w", "web", "open the ticket in Linear", &[Ticket], Act::Web),
    k(
        "u",
        "",
        "ask GitLab now; it also asks on its own, and when you come back",
        &[Board],
        Act::Refresh,
    ),
    k("?", "", "these keys", &[Board, List, Code], Act::Help),
    k("q", "", "back to the board", &[List, Code], Act::Leave),
    k("q", "", "quit", &[Board], Act::Quit),
];

/// A key press as the table writes it.
pub fn name(ev: &KeyEvent) -> String {
    let base = match ev.code {
        KeyCode::Char(' ') => "space".into(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "enter".into(),
        KeyCode::Esc => "esc".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        _ => return String::new(),
    };
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        format!("ctrl-{base}")
    } else {
        base
    }
}

/// A key typed into `text`: a letter, space or backspace. Whether it
/// changed anything.
pub fn edit(text: &mut String, key: &str) -> bool {
    match key {
        "backspace" => text.pop().is_some(),
        "ctrl-u" => !std::mem::take(text).is_empty(),
        "space" => {
            text.push(' ');
            true
        }
        c if c.chars().count() == 1 => {
            text.push_str(c);
            true
        }
        _ => false,
    }
}

fn applies(k: &Key, at: &[Place]) -> bool {
    k.on.iter().any(|p| at.contains(p))
}

/// What a key does where the cursor is. Two rows can share a key in places
/// that are never both true (enter on an MR, enter on the fold); where they
/// could be, the first row in the table wins.
pub fn act(key: &str, at: &[Place]) -> Option<Act> {
    KEYS.iter()
        .find(|k| k.key == key && applies(k, at))
        .map(|k| k.act)
}

/// The keys the footer offers where the cursor is.
pub fn footer(at: &[Place]) -> impl Iterator<Item = &'static Key> + '_ {
    KEYS.iter()
        .filter(move |k| !k.word.is_empty() && applies(k, at))
}

/// The keys the help screen lists, given every place on a screen.
pub fn help(places: &[Place]) -> impl Iterator<Item = &'static Key> + '_ {
    KEYS.iter().filter(move |k| applies(k, places))
}
