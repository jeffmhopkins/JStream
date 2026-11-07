//! Asking the person before a new agent connects
//! ([ADR-0131](../../../docs/adr/0131-ask-before-a-new-agent-connects.md)).
//!
//! - **The prompt** ([`AgentPrompt`], in the shell): while an agent waits for an answer
//!   (`agent.approval_requested`, or `agent.approved`'s `pending` when the page opens),
//!   a dialog says "<client name> wants to control JStream" and how it connects, with
//!   Allow once, Always allow and Deny (`agent.approve`). Several wait in turn, oldest
//!   first. `agent.approval_resolved` closes one answered anywhere, or timed out. Escape
//!   and the × leave it waiting; it times out on its own.
//! - **The setting** ([`AskAgentsSetting`], the Settings dialog's General page): "Ask
//!   before a new agent connects", the UI setting `agents.ask`, shown when the server
//!   answers `agent.approved`.
//! - **The approved list** ([`ApprovedAgents`], the Devices page): each approved client
//!   and how it connects, with Remove (`agent.forget`).
//!
//! A client's name is its own text, shown as data. A session with no server in front
//! answers the calls `unsupported`, and all three stay hidden.
//!
//! Docs: crates/jstream-ui/README.md#design

use crate::app::SessionContext;
use crate::dialog::Modal;
use crate::ui_settings::{self, Kept};
use futures_util::StreamExt;
use jstream_control::protocol::{
    Action, AgentApproval, AgentTransport, ApprovalDecision, ApprovedAgent, Call, CallResult,
    Event, Query, QueryResult,
};
use jstream_control::{Control, ControlError};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;
use std::rc::Rc;

/// The UI setting that turns asking off: `false` is off (ADR-0131 §6).
pub const ASK_KEY: &str = "agents.ask";

/// What `agent.approved` answered: whether the server asks, the approved list and the
/// open requests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Approvals {
    /// Whether the server asks now.
    pub asking: bool,
    /// The approved list.
    pub approved: Vec<ApprovedAgent>,
    /// The open requests, oldest first.
    pub pending: Vec<AgentApproval>,
}

/// `agent.approved`, or `None` when the session doesn't serve it.
pub async fn read(control: &dyn Control) -> Result<Option<Approvals>, String> {
    let call = Call::Query {
        query: Query::AgentApproved,
    };
    match control.call(call).await {
        Ok(response) => match response.result {
            CallResult::Query {
                result:
                    QueryResult::AgentApproved {
                        asking,
                        approved,
                        pending,
                    },
            } => Ok(Some(Approvals {
                asking,
                approved,
                pending,
            })),
            _ => Err("the engine answered `agent.approved` with something else".into()),
        },
        Err(error) if unserved(&error) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// Whether a call failed because nothing in front of the engine serves it.
fn unserved(error: &ControlError) -> bool {
    matches!(
        error.protocol(),
        Some(jstream_control::protocol::ProtocolError::Unsupported { .. })
    )
}

/// Answers request `approval_id` (`agent.approve`).
pub async fn answer(
    control: &dyn Control,
    approval_id: String,
    decision: ApprovalDecision,
) -> Result<(), String> {
    let call = Call::Action {
        action: Action::AgentApprove {
            approval_id,
            decision,
        },
    };
    control
        .call(call)
        .await
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Removes a row of the approved list (`agent.forget`).
pub async fn forget(control: &dyn Control, row: ApprovedAgent) -> Result<(), String> {
    let call = Call::Action {
        action: Action::AgentForget {
            client_name: row.client_name,
            transport: row.transport,
        },
    };
    control
        .call(call)
        .await
        .map(drop)
        .map_err(|error| error.to_string())
}

/// How a client connects, in words.
pub fn transport_text(transport: AgentTransport) -> &'static str {
    match transport {
        AgentTransport::McpHttp => "MCP over HTTP",
        // `jstream-cli mcp` carries an MCP client over the WebSocket API too.
        AgentTransport::Websocket | _ => "the WebSocket API (or MCP through jstream-cli)",
    }
}

/// The open requests after `event`: a new one added at the end, an ended one removed.
/// `None` when the event says nothing about them.
pub fn after(pending: &[AgentApproval], event: &Event) -> Option<Vec<AgentApproval>> {
    match event {
        Event::AgentApprovalRequested { approval } => {
            let mut next = pending.to_vec();
            if !next.iter().any(|p| p.approval_id == approval.approval_id) {
                next.push(approval.clone());
            }
            Some(next)
        }
        Event::AgentApprovalResolved { approval_id, .. } => Some(
            pending
                .iter()
                .filter(|p| p.approval_id != *approval_id)
                .cloned()
                .collect(),
        ),
        _ => None,
    }
}

/// The session's control, if one is open.
fn control() -> Option<Rc<dyn Control>> {
    use_context::<SessionContext>()
        .and_then(|context| context.get())
        .map(|session| session.control)
}

/// The prompt for the agents waiting for an answer.
#[component]
pub fn AgentPrompt() -> impl IntoView {
    let Some(control) = control() else {
        return ().into_any();
    };
    let pending = RwSignal::new(Vec::<AgentApproval>::new());
    let problem = RwSignal::new(None::<String>);
    let reread = {
        let control = Rc::clone(&control);
        move || {
            let control = Rc::clone(&control);
            spawn_local(async move {
                if let Ok(Some(read)) = read(&*control).await {
                    pending.try_set(read.pending);
                }
            });
        }
    };
    reread();
    let mut events = control.events();
    spawn_local(async move {
        while let Some(event) = events.next().await {
            if matches!(event, Event::EventsDropped { .. }) {
                reread();
                continue;
            }
            if let Some(next) = pending.try_with_untracked(|p| after(p, &event)).flatten() {
                if pending.try_set(next).is_some() {
                    return;
                }
                problem.try_set(None);
            }
        }
    });
    let control = StoredValue::new_local(control);
    let decide = move |approval_id: String, decision: ApprovalDecision| {
        let control = control.get_value();
        spawn_local(async move {
            if let Err(error) = answer(&*control, approval_id, decision).await {
                problem.try_set(Some(error));
            }
        });
    };
    // Escape and the × leave the agent waiting: the prompt comes back while it does.
    let hidden = RwSignal::new(None::<String>);
    let first = move || {
        pending.with(|p| p.first().cloned()).filter(|first| {
            hidden.with(|h| h.as_deref() != Some(first.approval_id.as_str()))
        })
    };
    view! {
        {move || first().map(|approval| {
            let id = approval.approval_id.clone();
            let shown = StoredValue::new(id.clone());
            let hide = move || hidden.set(Some(shown.get_value()));
            let (once, always, deny) = (id.clone(), id.clone(), id);
            let more = pending.with(Vec::len).saturating_sub(1);
            view! {
                <Modal label="Agent wants to connect" class="agent-prompt" cancel=hide>
                    <div class="modal-header">
                        <div class="modal-titles">
                            <h2 class="modal-title">
                                <span class="agent-name">{approval.client_name.clone()}</span>
                                " wants to control JStream"
                            </h2>
                            <span class="modal-sub">
                                "Connecting over " {transport_text(approval.transport)}
                            </span>
                        </div>
                        <button type="button" class="modal-close" aria-label="Close" on:click=move |_| hide()>"×"</button>
                    </div>
                    <div class="agent-prompt-body">
                        <p>
                            "An agent with full control can change scenes, sources and outputs, and go live. Allow it only if you set it up."
                        </p>
                        {(more > 0).then(|| view! {
                            <p class="agent-more">{format!("{more} more waiting")}</p>
                        })}
                        {move || problem.get().map(|p| view! { <p class="problem" role="alert">{p}</p> })}
                    </div>
                    <div class="modal-footer">
                        <button type="button" on:click=move |_| decide(deny.clone(), ApprovalDecision::Deny)>"Deny"</button>
                        <button type="button" on:click=move |_| decide(once.clone(), ApprovalDecision::AllowOnce)>"Allow once"</button>
                        <button type="button" class="primary" on:click=move |_| decide(always.clone(), ApprovalDecision::AlwaysAllow)>"Always allow"</button>
                    </div>
                </Modal>
            }
        })}
    }
    .into_any()
}

/// "Ask before a new agent connects", on the General page.
#[component]
pub fn AskAgentsSetting() -> impl IntoView {
    let Some(control) = control() else {
        return ().into_any();
    };
    let on = RwSignal::new(None::<bool>);
    {
        let control = Rc::clone(&control);
        spawn_local(async move {
            if !matches!(read(&*control).await, Ok(Some(_))) {
                return;
            }
            if let Ok(kept) = ui_settings::get(&*control, ASK_KEY).await
                && kept != Kept::Unserved
            {
                on.try_set(Some(!matches!(kept, Kept::Value(Value::Bool(false)))));
            }
        });
    }
    let control = StoredValue::new_local(control);
    let keep = move |checked: bool| {
        on.set(Some(checked));
        let control = control.get_value();
        spawn_local(async move {
            if let Err(error) = ui_settings::set(&*control, ASK_KEY, Value::Bool(checked)).await {
                leptos::logging::warn!("the agent setting could not be kept: {error}");
            }
        });
    };
    view! {
        <Show when=move || on.get().is_some()>
            <label class="settings-check">
                <input
                    type="checkbox"
                    aria-label="Ask before a new agent connects"
                    prop:checked=move || on.get().unwrap_or(true)
                    on:change=move |event| keep(event_target_checked(&event))
                />
                <span>
                    <span class="settings-check-label">"Ask before a new agent connects"</span>
                    <span class="settings-check-note">
                        "When JStream lets agents in (--mcp), it asks before a client it hasn't seen connects. A notice, not a lock: any program you run can read the token file."
                    </span>
                </span>
            </label>
        </Show>
    }
    .into_any()
}

/// The approved agents, on the Devices page.
#[component]
pub fn ApprovedAgents() -> impl IntoView {
    let Some(control) = control() else {
        return ().into_any();
    };
    let rows = RwSignal::new(None::<Vec<ApprovedAgent>>);
    let problem = RwSignal::new(None::<String>);
    let control = StoredValue::new_local(control);
    let reload = move || {
        let control = control.get_value();
        spawn_local(async move {
            match read(&*control).await {
                Ok(read) => {
                    rows.try_set(read.map(|r| r.approved));
                }
                Err(error) => {
                    problem.try_set(Some(error));
                }
            }
        });
    };
    reload();
    let remove = move |row: ApprovedAgent| {
        let control = control.get_value();
        spawn_local(async move {
            match forget(&*control, row).await {
                Ok(()) => reload(),
                Err(error) => {
                    problem.try_set(Some(error));
                }
            }
        });
    };
    view! {
        <Show when=move || rows.with(Option::is_some)>
            <section class="approved-agents">
                <h3>"Approved agents"</h3>
                {move || problem.get().map(|p| view! { <p class="problem" role="alert">{p}</p> })}
                {move || {
                    let list = rows.get().unwrap_or_default();
                    if list.is_empty() {
                        return view! {
                            <p class="placeholder">"No agent is always allowed."</p>
                        }
                        .into_any();
                    }
                    view! {
                        <ul class="device-list">
                            {list.into_iter().map(|row| {
                                let gone = row.clone();
                                view! {
                                    <li class="device">
                                        <span class="device-name">{row.client_name.clone()}</span>
                                        <span class="device-dates">{transport_text(row.transport)}</span>
                                        <button on:click=move |_| remove(gone.clone())>"Remove"</button>
                                    </li>
                                }
                            }).collect_view()}
                        </ul>
                    }
                    .into_any()
                }}
            </section>
        </Show>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::{after, transport_text};
    use jstream_control::protocol::{AgentApproval, AgentTransport, ApprovalOutcome, Event};

    fn request(id: &str) -> AgentApproval {
        AgentApproval::new(id, "claude-code", AgentTransport::Websocket)
    }

    #[test]
    fn requests_queue_in_order_and_leave_when_resolved() {
        let one = after(
            &[],
            &Event::AgentApprovalRequested {
                approval: request("apr_1"),
            },
        )
        .unwrap_or_default();
        let two = after(
            &one,
            &Event::AgentApprovalRequested {
                approval: request("apr_2"),
            },
        )
        .unwrap_or_default();
        assert_eq!(two, vec![request("apr_1"), request("apr_2")]);
        // The same request twice (read, then heard) is listed once.
        let again = after(
            &two,
            &Event::AgentApprovalRequested {
                approval: request("apr_2"),
            },
        );
        assert_eq!(again.as_ref().map(Vec::len), Some(2));
        let left = after(
            &two,
            &Event::AgentApprovalResolved {
                approval_id: "apr_1".into(),
                outcome: ApprovalOutcome::TimedOut,
            },
        );
        assert_eq!(left, Some(vec![request("apr_2")]));
        assert_eq!(after(&two, &Event::EventsDropped { count: 1 }), None);
    }

    #[test]
    fn transports_are_named() {
        assert_eq!(transport_text(AgentTransport::McpHttp), "MCP over HTTP");
        assert!(transport_text(AgentTransport::Websocket).contains("WebSocket"));
    }
}
