//! Every change ot makes on GitLab, as plain data, and the one interface
//! that makes them. Which request each write is, is worked out here and
//! nowhere else (`call`, no IO); the GitLab side only sends it, and tests
//! put a recorder in its place.
use serde_json::{json, Value};
#[cfg(test)]
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq)]
pub enum Write {
    /// At the head you looked at, so a push since isn't approved unseen.
    Approve {
        iid: u64,
        head: String,
    },
    Merge {
        iid: u64,
        head: String,
    },
    /// Merged by GitLab once what it waits on is done: the pipeline, approvals.
    AutoMerge {
        iid: u64,
        head: String,
    },
    Close {
        iid: u64,
    },
    /// Out of draft.
    Ready {
        iid: u64,
    },
    Pipeline {
        iid: u64,
    },
    /// The failed jobs of a pipeline, again.
    Retry {
        pipeline: u64,
    },
    Reviewers {
        iid: u64,
        names: Vec<String>,
    },
    /// The whole of it, in place of what is there.
    Description {
        iid: u64,
        text: String,
    },
    /// A comment that goes out now.
    Comment {
        iid: u64,
        at: At,
        body: String,
    },
    /// One that waits, yours alone, until Publish.
    Draft {
        iid: u64,
        at: At,
        body: String,
    },
    EditDraft {
        iid: u64,
        id: u64,
        body: String,
    },
    DeleteDraft {
        iid: u64,
        id: u64,
    },
    Publish {
        iid: u64,
    },
    Resolve {
        iid: u64,
        thread: String,
        resolved: bool,
    },
}

/// Where a comment goes.
#[derive(Clone, Debug, PartialEq)]
pub enum At {
    /// On the MR, not on a line.
    Mr,
    Line(Position),
    /// Under a thread already there.
    Reply(String),
}

/// A line of the diff as GitLab names it: the MR's three commits, both
/// paths, and the line's number on each side it is on.
#[derive(Clone, Debug, PartialEq)]
pub struct Position {
    pub base: String,
    pub start: String,
    pub head: String,
    pub old_path: String,
    pub new_path: String,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    /// For a comment on several lines, its first and last; the last is
    /// the line above.
    pub range: Option<[Spot; 2]>,
}

/// A line still there, by its place on both sides, the old one as the diff
/// counts it (review::File::old_pos).
#[derive(Clone, Debug, PartialEq)]
pub struct Spot {
    pub old: u32,
    pub new: u32,
    pub added: bool,
}

/// How a REST request goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
}

/// One request to GitLab.
#[derive(Debug, PartialEq)]
pub enum Call {
    Rest {
        method: Method,
        path: String,
        body: Value,
    },
    Graphql {
        query: &'static str,
        variables: Value,
    },
}

/// A project in a REST path: its full path, slashes encoded.
pub fn project_path(project: &str) -> String {
    format!("/projects/{}", project.replace('/', "%2F"))
}

/// An MR in a REST path.
pub fn mr_path(project: &str, iid: u64) -> String {
    format!("{}/merge_requests/{iid}", project_path(project))
}

const SET_DRAFT: &str = "mutation($p: ID!, $i: String!) {
  mergeRequestSetDraft(input: {projectPath: $p, iid: $i, draft: false}) { errors } }";

const SET_REVIEWERS: &str = "mutation($p: ID!, $i: String!, $n: [String!]!) {
  mergeRequestSetReviewers(input: {projectPath: $p, iid: $i, reviewerUsernames: $n,
    operationMode: REPLACE}) { errors } }";

pub fn call(project: &str, w: &Write) -> Call {
    use Method::*;
    let rest = |method, path: String, body| Call::Rest { method, path, body };
    let mr = |iid: &u64| mr_path(project, *iid);
    match w {
        Write::Approve { iid, head } => rest(Post, mr(iid) + "/approve", json!({ "sha": head })),
        Write::Merge { iid, head } => rest(Put, mr(iid) + "/merge", json!({ "sha": head })),
        Write::AutoMerge { iid, head } => rest(
            Put,
            mr(iid) + "/merge",
            json!({ "sha": head, "auto_merge": true }),
        ),
        Write::Close { iid } => rest(Put, mr(iid), json!({ "state_event": "close" })),
        Write::Pipeline { iid } => rest(Post, mr(iid) + "/pipelines", json!({})),
        Write::Retry { pipeline } => {
            let path = format!("{}/pipelines/{pipeline}/retry", project_path(project));
            rest(Post, path, json!({}))
        }
        Write::Ready { iid } => Call::Graphql {
            query: SET_DRAFT,
            variables: json!({ "p": project, "i": iid.to_string() }),
        },
        Write::Reviewers { iid, names } => Call::Graphql {
            query: SET_REVIEWERS,
            variables: json!({ "p": project, "i": iid.to_string(), "n": names }),
        },
        Write::Description { iid, text } => rest(Put, mr(iid), json!({ "description": text })),
        Write::Comment { iid, at, body } => match at {
            At::Mr => rest(Post, mr(iid) + "/notes", json!({ "body": body })),
            At::Line(p) => {
                let body = json!({ "body": body, "position": position(p) });
                rest(Post, mr(iid) + "/discussions", body)
            }
            At::Reply(thread) => {
                let path = format!("{}/discussions/{thread}/notes", mr(iid));
                rest(Post, path, json!({ "body": body }))
            }
        },
        Write::Draft { iid, at, body } => {
            let body = match at {
                At::Mr => json!({ "note": body }),
                At::Line(p) => json!({ "note": body, "position": position(p) }),
                At::Reply(thread) => json!({ "note": body, "in_reply_to_discussion_id": thread }),
            };
            rest(Post, mr(iid) + "/draft_notes", body)
        }
        Write::EditDraft { iid, id, body } => {
            let path = format!("{}/draft_notes/{id}", mr(iid));
            rest(Put, path, json!({ "note": body }))
        }
        Write::DeleteDraft { iid, id } => {
            rest(Delete, format!("{}/draft_notes/{id}", mr(iid)), json!({}))
        }
        Write::Publish { iid } => rest(Post, mr(iid) + "/draft_notes/bulk_publish", json!({})),
        Write::Resolve {
            iid,
            thread,
            resolved,
        } => {
            let path = format!("{}/discussions/{thread}", mr(iid));
            rest(Put, path, json!({ "resolved": resolved }))
        }
    }
}

fn position(p: &Position) -> Value {
    let mut v = json!({
        "position_type": "text", "base_sha": p.base, "start_sha": p.start, "head_sha": p.head,
        "old_path": p.old_path, "new_path": p.new_path, "old_line": p.old_line, "new_line": p.new_line,
    });
    if let Some([first, last]) = &p.range {
        let file = sha1_smol::Sha1::from(&p.new_path).digest();
        let end = |s: &Spot| {
            json!({
                "line_code": format!("{file}_{}_{}", s.old, s.new),
                "type": s.added.then_some("new"),
                "old_line": (!s.added).then_some(s.old),
                "new_line": s.new,
            })
        };
        v["line_range"] = json!({ "start": end(first), "end": end(last) });
    }
    v
}

/// Makes writes: GitLab's, or a recorder's.
pub trait Writer: Send + Sync {
    fn write(&self, w: &Write) -> Result<(), String>;
}

/// Each in order, stopping at the first that fails.
pub fn all(writer: &dyn Writer, ws: &[Write]) -> Result<(), String> {
    ws.iter().try_for_each(|w| writer.write(w))
}

/// Keeps what it is asked to write, for tests.
#[cfg(test)]
#[derive(Default)]
pub struct Recorder(pub Mutex<Vec<Write>>);

#[cfg(test)]
impl Writer for Recorder {
    fn write(&self, w: &Write) -> Result<(), String> {
        self.0.lock().unwrap().push(w.clone());
        Ok(())
    }
}

#[cfg(test)]
impl Recorder {
    pub fn took(&self) -> Vec<Write> {
        self.0.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rest(c: Call) -> (Method, String, Value) {
        match c {
            Call::Rest { method, path, body } => (method, path, body),
            Call::Graphql { .. } => panic!("not REST"),
        }
    }

    #[test]
    fn each_write_is_one_request() {
        let p = "team/app";
        assert_eq!(
            rest(call(
                p,
                &Write::Approve {
                    iid: 7,
                    head: "abc".into()
                }
            )),
            (
                Method::Post,
                "/projects/team%2Fapp/merge_requests/7/approve".into(),
                json!({ "sha": "abc" })
            )
        );
        assert_eq!(
            rest(call(
                p,
                &Write::AutoMerge {
                    iid: 7,
                    head: "abc".into()
                }
            )),
            (
                Method::Put,
                "/projects/team%2Fapp/merge_requests/7/merge".into(),
                json!({ "sha": "abc", "auto_merge": true })
            )
        );
        assert_eq!(
            rest(call(p, &Write::Close { iid: 7 })),
            (
                Method::Put,
                "/projects/team%2Fapp/merge_requests/7".into(),
                json!({ "state_event": "close" })
            )
        );
        assert_eq!(
            rest(call(p, &Write::Description { iid: 7, text: "x\n\ncc: kim@team.example".into() })),
            (
                Method::Put,
                "/projects/team%2Fapp/merge_requests/7".into(),
                json!({ "description": "x\n\ncc: kim@team.example" })
            )
        );
        assert_eq!(
            rest(call(p, &Write::Retry { pipeline: 99 })).1,
            "/projects/team%2Fapp/pipelines/99/retry"
        );
        assert_eq!(
            rest(call(p, &Write::Publish { iid: 7 })).1,
            "/projects/team%2Fapp/merge_requests/7/draft_notes/bulk_publish"
        );
        match call(
            p,
            &Write::Reviewers {
                iid: 7,
                names: vec!["kim".into()],
            },
        ) {
            Call::Graphql { variables, .. } => assert_eq!(
                variables,
                json!({ "p": "team/app", "i": "7", "n": ["kim"] })
            ),
            c => panic!("{c:?}"),
        }
    }

    #[test]
    fn a_comment_on_a_line_carries_its_position() {
        let at = At::Line(Position {
            base: "b".into(),
            start: "s".into(),
            head: "h".into(),
            old_path: "a.ts".into(),
            new_path: "a.ts".into(),
            old_line: None,
            new_line: Some(4),
            range: None,
        });
        let (method, path, body) = rest(call(
            "team/app",
            &Write::Draft {
                iid: 7,
                at,
                body: "why?".into(),
            },
        ));
        assert_eq!(
            (method, path.as_str()),
            (
                Method::Post,
                "/projects/team%2Fapp/merge_requests/7/draft_notes"
            )
        );
        assert_eq!(body["note"], "why?");
        assert_eq!(body["position"]["new_line"], 4);
        assert_eq!(body["position"]["old_line"], Value::Null);
        assert_eq!(body["position"]["head_sha"], "h");
    }

    #[test]
    fn a_comment_on_several_lines_names_both_ends_by_line_code() {
        let p = Position {
            base: "b".into(),
            start: "s".into(),
            head: "h".into(),
            old_path: "a.ts".into(),
            new_path: "a.ts".into(),
            old_line: None,
            new_line: Some(4),
            range: Some([
                Spot {
                    old: 3,
                    new: 3,
                    added: false,
                },
                Spot {
                    old: 7,
                    new: 4,
                    added: true,
                },
            ]),
        };
        let sha = "02699cd6ca65aa6819d7e387678d8ea5359b5e59";
        assert_eq!(
            position(&p)["line_range"],
            json!({
                "start": { "line_code": format!("{sha}_3_3"), "type": null, "old_line": 3, "new_line": 3 },
                "end": { "line_code": format!("{sha}_7_4"), "type": "new", "old_line": null, "new_line": 4 },
            })
        );
    }

    #[test]
    fn a_reply_goes_under_its_thread_now_or_as_a_draft() {
        let at = At::Reply("d1".into());
        assert_eq!(
            rest(call(
                "t/a",
                &Write::Comment {
                    iid: 7,
                    at: at.clone(),
                    body: "done".into()
                }
            ))
            .1,
            "/projects/t%2Fa/merge_requests/7/discussions/d1/notes"
        );
        let (_, path, body) = rest(call(
            "t/a",
            &Write::Draft {
                iid: 7,
                at,
                body: "done".into(),
            },
        ));
        assert_eq!(
            (path.as_str(), &body["in_reply_to_discussion_id"]),
            ("/projects/t%2Fa/merge_requests/7/draft_notes", &json!("d1"))
        );
    }
}
