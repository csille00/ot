//! Haiku's line on where an agent's work stands once it has stopped and
//! waits, which the panel shows in place of the agent's last message, more
//! often than not a paragraph. One read per turn an agent stops on, and one
//! read at a time.
use crate::ask;
use crate::claude::{Session, State, Turn, Who};
use crate::model::{Board, Item, Worker};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// Which agent, and the turn it stopped on (Worker::at).
type Key = (Option<String>, Option<String>, i64);

fn key(w: &Worker) -> Key {
    (w.session.clone(), w.agent.clone(), w.at)
}

/// What Haiku said of each agent's turn; None while it reads.
#[derive(Debug, Default)]
pub struct Briefs(HashMap<Key, Option<String>>);

impl Briefs {
    /// Haiku's line on `w`, once it has read the turn `w` stopped on.
    pub fn of(&self, w: &Worker) -> Option<&str> {
        self.0.get(&key(w))?.as_deref()
    }

    /// The agents on `board` waiting on a turn Haiku hasn't been asked
    /// about, each with the end of its transcript, now marked asked.
    pub fn due(&mut self, board: &Board, sessions: &[Session]) -> Vec<(Worker, String)> {
        let rows = board.tree.iter().chain(&board.not_sent);
        let workers = rows.filter_map(|r| match &r.item {
            Item::Mr { agent, .. } | Item::Ticket { agent, .. } => agent.as_ref(),
            _ => None,
        });
        let mut due = vec![];
        for w in workers.filter(|w| w.state == State::Idle) {
            let Some(turns) = turns(w, sessions) else {
                continue;
            };
            if !self.0.contains_key(&key(w)) {
                self.0.insert(key(w), None);
                due.push((w.clone(), log(turns)));
            }
        }
        due
    }

    pub fn heard(&mut self, w: &Worker, said: String) {
        self.0.insert(key(w), Some(said));
    }
}

/// The turns of `w`: its agent's, or its session's own.
fn turns<'s>(w: &Worker, sessions: &'s [Session]) -> Option<&'s [Turn]> {
    let s = sessions.iter().find(|s| w.session.as_ref() == Some(&s.id))?;
    match &w.agent {
        Some(name) => s.agent(name).map(|a| a.turns.as_slice()),
        None => Some(&s.turns),
    }
}

/// How much of the end of a transcript Haiku reads, in bytes: the agent's
/// last message whole, and what led up to it. More only drew Haiku into
/// the history.
const READS: usize = 8_000;

/// The end of `turns` as Haiku reads it, a turn a line.
fn log(turns: &[Turn]) -> String {
    let mut lines = vec![];
    let mut size = 0;
    for t in turns.iter().rev() {
        let who = match t.who {
            Who::You => "asked",
            Who::Claude => "said",
            Who::Tool => "did",
        };
        let times = if t.times > 1 { format!(" ({} times)", t.times) } else { String::new() };
        let line = format!("{who}: {}{times}", t.text.trim());
        size += line.len();
        if size > READS && !lines.is_empty() {
            break;
        }
        lines.push(line);
    }
    lines.reverse();
    lines.join("\n")
}

/// Haiku's reads queue here, so a board of agents that all stopped
/// doesn't start a Claude for each at once.
static READING: Mutex<()> = Mutex::new(());

const SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"required":["state"],"properties":{"state":{"type":"string"}}}"#;

/// Where the work of the agent `name` stands, as Haiku reads `log`, the
/// end of its transcript; the run kept in `run`.
pub fn read(name: &str, log: &str, run: &Path) -> Result<String, String> {
    let _one = READING.lock().unwrap_or_else(|e| e.into_inner());
    std::fs::create_dir_all(run).map_err(|e| e.to_string())?;
    let prompt = format!(
        "This is the end of the transcript of {name}, a Claude Code agent building tickets for a developer. It has stopped and waits, and its last message is where it left things; what came before is only there to make sense of it.\n\n\
In one plain sentence of at most 25 words, tell the developer where the work stands now: what is done, and anything it asks of them. Name merge requests, tickets and commits as the transcript does. Start with the work, not the agent.\n\n\
<transcript>\n{log}\n</transcript>\n"
    );
    let said = ask::claude("haiku", run, run, "", SCHEMA, &prompt)?;
    match said["state"].as_str().map(str::trim) {
        Some(s) if !s.is_empty() => Ok(s.to_string()),
        _ => Err("Haiku stopped without a word".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{board, fixture};

    #[test]
    fn an_agent_that_stopped_is_read_once_for_the_turn_it_stopped_on() {
        let (snap, tickets, mut sessions) = fixture::swarm();
        let b = board(&snap, &fixture::rules(), &HashMap::new(), &sessions, &[], &tickets, fixture::NOW);
        let mut briefs = Briefs::default();
        let due = briefs.due(&b, &sessions);
        let names: Vec<&str> = due.iter().map(|(w, _)| w.name.as_str()).collect();
        assert_eq!(names, ["ap-4"], "ap-2 is still at work");
        assert_eq!(due[0].1, "asked: Build it.\ndid: Run the tests");
        assert!(briefs.due(&b, &sessions).is_empty(), "asked once");
        assert_eq!(briefs.of(&due[0].0), None, "while Haiku reads");
        briefs.heard(&due[0].0, "Tests pass; nothing waits on you.".into());
        assert_eq!(briefs.of(&due[0].0), Some("Tests pass; nothing waits on you."));
        // ap-4 is told more, and stops again.
        let a = sessions[0].turns[1].agent.as_mut().unwrap();
        a.turns.push(Turn { who: Who::Claude, text: "Done.".into(), at: fixture::NOW - 60, times: 1, agent: None });
        let b = board(&snap, &fixture::rules(), &HashMap::new(), &sessions, &[], &tickets, fixture::NOW);
        assert_eq!(briefs.due(&b, &sessions).len(), 1, "its new turn is read");
    }
}
