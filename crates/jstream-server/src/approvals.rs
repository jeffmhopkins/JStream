//! Asking the person before a new agent connects
//! ([ADR-0131](../../../docs/adr/0131-ask-before-a-new-agent-connects.md)).
//!
//! When the host turns it on (`ServerConfig::ask_agents`, the desktop app with MCP on)
//! and the UI setting [`ASK_SETTING`] isn't `false`, a session admitted by the control
//! token whose `hello` names a client not on the approved list is held: [`Approvals::hold`]
//! opens a request, every admitted session hears `agent.approval_requested`, and the
//! session's `hello` waits on its [`Ticket`] until someone answers (`agent.approve`), the
//! request times out ([`APPROVAL_TIMEOUT`]), or the session goes away. Then every session
//! hears `agent.approval_resolved`.
//!
//! The request belongs to the session, not to one `hello`: an MCP client gives `hello`
//! seconds and sends it again on its next tool call, and each one waits on the same
//! request. A denied or timed-out session's later `hello` fails the same way; a new
//! connection asks again.
//!
//! This is a notice with a veto, not a control (ADR-0131 §1): approval is keyed by the
//! name a client gives itself. Sessions admitted by a device token or by the UI's own
//! token are never held.
//!
//! The approved list is kept in a user-only file beside the token file
//! ([`approvals_file`](crate::token_file::approvals_file)), written whole on each change.

use crate::devices::{read_private, write_private};
use crate::error::ServerError;
use crate::ui_settings::UiSettings;
use jstream_protocol::{
    AgentApproval, AgentTransport, ApprovalDecision, ApprovalOutcome, ApprovedAgent, EntityKind,
    Event, ProtocolError, QueryResult,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;
use tokio::sync::{broadcast, watch};
use tokio::time::Instant;

/// How long a held client waits for an answer before it is turned away (ADR-0131 §4).
pub(crate) const APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);

/// The UI setting that turns asking off where the host turned it on: `false` is off,
/// anything else (or nothing) on (ADR-0131 §6).
pub const ASK_SETTING: &str = "agents.ask";

/// The most requests open at once. One beyond it is turned away at once, so a client
/// that opens connections in a loop can't pile prompts up.
const MAX_OPEN: usize = 8;

/// How many approval events a slow session may fall behind by before it misses some.
const EVENT_BUFFER: usize = 64;

/// The approved list file's format version.
const FILE_VERSION: u32 = 1;

/// How a held request ended, for its waiting `hello`s.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Answer {
    Allowed,
    Refused(String),
}

/// One open request.
struct Open {
    approval: AgentApproval,
    answer: watch::Sender<Option<Answer>>,
}

#[derive(Default)]
struct State {
    approved: BTreeSet<ApprovedAgent>,
    open: Vec<Open>,
    next: u64,
}

/// The approved list, the open requests, and the events about them.
pub(crate) struct Approvals {
    /// Whether the host asks at all (`ServerConfig::ask_agents`).
    enabled: bool,
    file: Option<PathBuf>,
    timeout: Duration,
    ui_settings: Arc<UiSettings>,
    state: Mutex<State>,
    events: broadcast::Sender<Event>,
}

/// A session's request: what its `hello`s wait on. Dropping the last handle while the
/// request is open ends it as `closed`.
pub(crate) struct Ticket {
    approvals: Arc<Approvals>,
    id: String,
    answer: watch::Receiver<Option<Answer>>,
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    approved: Vec<ApprovedAgent>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Approvals {
    /// The approvals kept in `file` (a missing file approves no one), or in memory only
    /// without one. `enabled` is `ServerConfig::ask_agents`.
    pub(crate) fn load(
        enabled: bool,
        file: Option<PathBuf>,
        timeout: Duration,
        ui_settings: Arc<UiSettings>,
    ) -> Result<Approvals, ServerError> {
        let mut approved = BTreeSet::new();
        if let Some(path) = &file {
            let read = read_private(path).and_then(|text| match text {
                Some(text) => parse(&text),
                None => Ok(Vec::new()),
            });
            approved = read
                .map_err(|reason| ServerError::ApprovalsFile {
                    path: path.clone(),
                    reason,
                })?
                .into_iter()
                .collect();
        }
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        Ok(Approvals {
            enabled,
            file,
            timeout,
            ui_settings,
            state: Mutex::new(State {
                approved,
                ..State::default()
            }),
            events,
        })
    }

    /// Whether a new agent is asked about now: the host turned it on, and the person
    /// hasn't turned it off.
    pub(crate) fn asking(&self) -> bool {
        self.enabled
            && self
                .ui_settings
                .get(ASK_SETTING)
                .ok()
                .flatten()
                .is_none_or(|value| value != serde_json::Value::Bool(false))
    }

    /// Whether the host asks at all, which lengthens the handshake deadline of the
    /// sessions the control token admits.
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// How long a request waits for an answer.
    pub(crate) fn timeout(&self) -> Duration {
        self.timeout
    }

    /// The approval events, for one session.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// A request for `client_name` over `transport`, or `None` when it needn't wait:
    /// asking is off, or the client is approved. Must run inside the Tokio runtime,
    /// which times the request out.
    pub(crate) fn hold(
        self: &Arc<Self>,
        client_name: &str,
        transport: AgentTransport,
    ) -> Option<Ticket> {
        if !self.asking() {
            return None;
        }
        let row = ApprovedAgent::new(client_name, transport);
        let (id, answer) = {
            let mut state = lock(&self.state);
            if state.approved.contains(&row) {
                return None;
            }
            let (tx, rx) = watch::channel(None);
            state.next += 1;
            let id = format!("apr_{}", state.next);
            if state.open.len() >= MAX_OPEN {
                tracing::warn!("too many agents waiting for an answer; one was turned away");
                let _ = tx.send(Some(Answer::Refused(
                    "too many connections are waiting for the person's answer".into(),
                )));
                return Some(Ticket {
                    approvals: Arc::clone(self),
                    id,
                    answer: rx,
                });
            }
            let approval = AgentApproval::new(id.clone(), client_name, transport);
            state.open.push(Open {
                approval: approval.clone(),
                answer: tx,
            });
            let _ = self.events.send(Event::AgentApprovalRequested { approval });
            (id, rx)
        };
        tracing::info!(approval_id = %id, ?transport, "an agent is waiting for the person's answer");
        let weak = Arc::downgrade(self);
        let timed = id.clone();
        let deadline = Instant::now() + self.timeout;
        tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            time_out(&weak, &timed);
        });
        Some(Ticket {
            approvals: Arc::clone(self),
            id,
            answer,
        })
    }

    /// Answers request `id` (`agent.approve`).
    pub(crate) fn decide(&self, id: &str, decision: ApprovalDecision) -> Result<(), ProtocolError> {
        let (outcome, answer) = match decision {
            ApprovalDecision::AllowOnce => (ApprovalOutcome::AllowedOnce, Answer::Allowed),
            ApprovalDecision::AlwaysAllow => (ApprovalOutcome::AlwaysAllowed, Answer::Allowed),
            ApprovalDecision::Deny | _ => (
                ApprovalOutcome::Denied,
                Answer::Refused("the person declined this connection".into()),
            ),
        };
        let mut state = lock(&self.state);
        let Some(at) = state.open.iter().position(|o| o.approval.approval_id == id) else {
            return Err(ProtocolError::NotFound {
                entity: EntityKind::Approval,
                id: id.to_owned(),
            });
        };
        let open = state.open.remove(at);
        let mut unsaved = None;
        if outcome == ApprovalOutcome::AlwaysAllowed {
            state.approved.insert(ApprovedAgent::new(
                open.approval.client_name.clone(),
                open.approval.transport,
            ));
            unsaved = self.save(&state).err();
        }
        let _ = open.answer.send(Some(answer));
        let _ = self.events.send(Event::AgentApprovalResolved {
            approval_id: id.to_owned(),
            outcome,
        });
        drop(state);
        tracing::info!(approval_id = %id, ?outcome, "an agent's request was answered");
        match unsaved {
            None => Ok(()),
            Some(error) => {
                tracing::warn!(%error, "the approved agents file could not be written");
                Err(ProtocolError::ActionFailed {
                    reason: format!(
                        "the connection is allowed, and the client stays approved until the \
                         server stops, but the approved agents file could not be written: \
                         {error}"
                    ),
                })
            }
        }
    }

    /// Removes a row from the approved list (`agent.forget`).
    pub(crate) fn forget(
        &self,
        client_name: &str,
        transport: AgentTransport,
    ) -> Result<(), ProtocolError> {
        let mut state = lock(&self.state);
        let row = ApprovedAgent::new(client_name, transport);
        if !state.approved.remove(&row) {
            return Err(ProtocolError::NotFound {
                entity: EntityKind::Approval,
                id: client_name.to_owned(),
            });
        }
        self.save(&state).map_err(|error| {
            tracing::warn!(%error, "the approved agents file could not be written");
            ProtocolError::ActionFailed {
                reason: format!(
                    "the client is forgotten until the server stops, but the approved agents \
                     file could not be written: {error}"
                ),
            }
        })
    }

    /// `agent.approved`'s answer.
    pub(crate) fn list(&self) -> QueryResult {
        let state = lock(&self.state);
        QueryResult::AgentApproved {
            asking: self.asking(),
            approved: state.approved.iter().cloned().collect(),
            pending: state.open.iter().map(|o| o.approval.clone()).collect(),
        }
    }

    /// Ends request `id`, if it is still open, as `outcome`, refusing its `hello`s with
    /// `reason`.
    fn end(&self, id: &str, outcome: ApprovalOutcome, reason: &str) {
        let mut state = lock(&self.state);
        let Some(at) = state.open.iter().position(|o| o.approval.approval_id == id) else {
            return;
        };
        let open = state.open.remove(at);
        let _ = open.answer.send(Some(Answer::Refused(reason.to_owned())));
        let _ = self.events.send(Event::AgentApprovalResolved {
            approval_id: id.to_owned(),
            outcome,
        });
        drop(state);
        tracing::info!(approval_id = %id, ?outcome, "an agent's request ended unanswered");
    }

    fn save(&self, state: &State) -> std::io::Result<()> {
        let Some(path) = &self.file else {
            return Ok(());
        };
        let file = File {
            version: FILE_VERSION,
            approved: state.approved.iter().cloned().collect(),
        };
        let mut text = serde_json::to_string_pretty(&file).map_err(std::io::Error::other)?;
        text.push('\n');
        write_private(path, &text)
    }
}

/// Times request `id` out, if the server and the request are still there.
fn time_out(approvals: &Weak<Approvals>, id: &str) {
    if let Some(approvals) = approvals.upgrade() {
        approvals.end(
            id,
            ApprovalOutcome::TimedOut,
            "the person didn't answer this connection in time",
        );
    }
}

fn parse(text: &str) -> Result<Vec<ApprovedAgent>, String> {
    let file: File = serde_json::from_str(text).map_err(|error| error.to_string())?;
    if file.version != FILE_VERSION {
        return Err(format!(
            "it is not a version {FILE_VERSION} approved agents file"
        ));
    }
    Ok(file.approved)
}

impl Ticket {
    /// Waits for the person's answer: `Ok` when allowed, `not_allowed` otherwise.
    pub(crate) async fn wait(&self) -> Result<(), ProtocolError> {
        let mut answer = self.answer.clone();
        let answer = answer
            .wait_for(Option::is_some)
            .await
            .map(|a| a.clone())
            .ok()
            .flatten();
        match answer {
            Some(Answer::Allowed) => Ok(()),
            Some(Answer::Refused(reason)) => Err(ProtocolError::NotAllowed { reason }),
            // The server is going away.
            None => Err(ProtocolError::NotAllowed {
                reason: "the server stopped before the person answered".into(),
            }),
        }
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.approvals.end(
            &self.id,
            ApprovalOutcome::Closed,
            "the connection closed before the person answered",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approvals(file: Option<PathBuf>) -> Arc<Approvals> {
        match Approvals::load(
            true,
            file,
            Duration::from_secs(60),
            Arc::new(UiSettings::load(None)),
        ) {
            Ok(approvals) => Arc::new(approvals),
            Err(error) => panic!("approvals: {error}"),
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => panic!("runtime: {error}"),
        }
    }

    /// Two `hello`s waiting on one request are both let in by one answer, as an MCP
    /// client's retried `hello` is.
    #[test]
    fn every_hello_waiting_on_a_request_gets_its_answer() {
        runtime().block_on(async {
            let approvals = approvals(None);
            let mut events = approvals.subscribe();
            let ticket = Arc::new(
                approvals
                    .hold("claude-code", AgentTransport::McpHttp)
                    .expect("held"),
            );
            let (first, second) = (Arc::clone(&ticket), Arc::clone(&ticket));
            let waiting = tokio::spawn(async move {
                let (a, b) = tokio::join!(first.wait(), second.wait());
                a.and(b)
            });
            let Ok(Event::AgentApprovalRequested { approval }) = events.recv().await else {
                panic!("no request");
            };
            assert!(
                approvals
                    .decide(&approval.approval_id, ApprovalDecision::AllowOnce)
                    .is_ok()
            );
            assert_eq!(waiting.await.ok(), Some(Ok(())));
            // A request answered can't be answered again.
            let again = approvals.decide(&approval.approval_id, ApprovalDecision::Deny);
            assert_eq!(again.err().map(|e| e.code()), Some("not_found"));
        });
    }

    /// Always allow is written to the file, user-only, and read back by the next server.
    #[test]
    fn the_approved_list_is_kept_in_its_file() {
        let dir = std::env::temp_dir().join(format!("jstream-approvals-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("token.agents");
        runtime().block_on(async {
            let approvals = approvals(Some(file.clone()));
            let ticket = approvals
                .hold("cursor", AgentTransport::Websocket)
                .expect("held");
            assert!(
                approvals
                    .decide(&ticket.id, ApprovalDecision::AlwaysAllow)
                    .is_ok()
            );
            assert_eq!(ticket.wait().await, Ok(()));
        });
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).expect("file").permissions().mode();
            assert_eq!(mode & 0o077, 0, "user-only");
        }
        runtime().block_on(async {
            let again = approvals(Some(file.clone()));
            assert!(again.hold("cursor", AgentTransport::Websocket).is_none());
            assert!(again.hold("cursor", AgentTransport::McpHttp).is_some());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A request beyond the open limit is turned away at once.
    #[test]
    fn requests_beyond_the_limit_are_turned_away() {
        runtime().block_on(async {
            let approvals = approvals(None);
            let open: Vec<Ticket> = (0..MAX_OPEN)
                .filter_map(|n| approvals.hold(&format!("a{n}"), AgentTransport::Websocket))
                .collect();
            assert_eq!(open.len(), MAX_OPEN);
            let over = approvals
                .hold("one-more", AgentTransport::Websocket)
                .expect("a ticket");
            assert_eq!(
                over.wait().await.err().map(|e| e.code()),
                Some("not_allowed")
            );
        });
    }
}
