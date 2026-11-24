//! Asking the person before a device pairs without a code
//! ([ADR-0235](../../../docs/adr/0235-pair-a-device-by-approving-it-on-the-host.md)).
//!
//! [`DevicePrompt`], in the shell: while a device on the network waits for an answer
//! (`pairing.asked`), a dialog says "\<name\> wants to connect" and the address it asked
//! from, with Allow and Deny (`pairing.answer`). Several wait in turn, oldest first.
//! `pairing.ask_ended` closes one answered anywhere, timed out, or whose device went
//! away. Escape and the × leave it waiting; it times out on its own after 2 minutes.
//! A page opened while an ask waits doesn't show it: the device asks again.
//!
//! The name is the device's own text, shown as data. A session with no server in front
//! hears no ask, and shows nothing.
//!
//! Docs: crates/jstream-ui/README.md#design

use crate::app::SessionContext;
use crate::dialog::Modal;
use futures_util::StreamExt;
use jstream_control::Control;
use jstream_control::protocol::{Action, Call, Event, PairingAsk};
use leptos::prelude::*;
use leptos::task::spawn_local;

/// The open asks after `event`: a new one added at the end, an ended one removed. `None`
/// when the event says nothing about them.
pub fn after(open: &[PairingAsk], event: &Event) -> Option<Vec<PairingAsk>> {
    match event {
        Event::PairingAsked { ask } => {
            let mut next = open.to_vec();
            if !next.iter().any(|a| a.ask_id == ask.ask_id) {
                next.push(ask.clone());
            }
            Some(next)
        }
        Event::PairingAskEnded { ask_id, .. } => Some(
            open.iter()
                .filter(|a| a.ask_id != *ask_id)
                .cloned()
                .collect(),
        ),
        _ => None,
    }
}

/// Answers ask `ask_id` (`pairing.answer`).
pub async fn answer(control: &dyn Control, ask_id: String, allow: bool) -> Result<(), String> {
    let call = Call::Action {
        action: Action::PairingAnswer { ask_id, allow },
    };
    control
        .call(call)
        .await
        .map(drop)
        .map_err(|error| error.to_string())
}

/// The prompt for the devices waiting for an answer.
#[component]
pub fn DevicePrompt() -> impl IntoView {
    let session = use_context::<SessionContext>();
    let current = move || session.and_then(|s| s.get()).map(|s| s.control);
    let Some(control) = current() else {
        return ().into_any();
    };
    let open = RwSignal::new(Vec::<PairingAsk>::new());
    let problem = RwSignal::new(None::<String>);
    // The event task holds only the stream, so nothing here keeps the connection open
    // after Disconnect or a reconnect (as `agents.rs` does).
    let mut events = control.events();
    drop(control);
    spawn_local(async move {
        while let Some(event) = events.next().await {
            if let Some(next) = open.try_with_untracked(|o| after(o, &event)).flatten() {
                if open.try_set(next).is_some() {
                    return;
                }
                problem.try_set(None);
            }
        }
    });
    let decide = move |ask_id: String, allow: bool| {
        let Some(control) = current() else {
            return;
        };
        spawn_local(async move {
            if let Err(error) = answer(&*control, ask_id, allow).await {
                problem.try_set(Some(error));
            }
        });
    };
    let hidden = RwSignal::new(Vec::<String>::new());
    let first = Memo::new(move |_| {
        open.with(|o| hidden.with(|h| o.iter().find(|a| !h.contains(&a.ask_id)).cloned()))
    });
    view! {
        {move || first.get().map(|ask| {
            let shown = StoredValue::new(ask.ask_id.clone());
            let hide = move || hidden.update(|h| h.push(shown.get_value()));
            let (allow, deny) = (ask.ask_id.clone(), ask.ask_id.clone());
            view! {
                <Modal label="Device wants to connect" class="agent-prompt" cancel=hide>
                    <div class="modal-header">
                        <div class="modal-titles">
                            <h2 class="modal-title">
                                <span class="agent-name">{ask.name.clone()}</span>
                                " wants to connect"
                            </h2>
                            <span class="modal-sub">"From " {ask.address.clone()} " on your network"</span>
                        </div>
                        <button type="button" class="modal-close" aria-label="Close" on:click=move |_| hide()>"×"</button>
                    </div>
                    <div class="agent-prompt-body">
                        <p>
                            "A paired device can control JStream: change scenes and go live. Allow it only if you just asked it to connect, from that address. You can revoke it under Settings, Devices."
                        </p>
                        {move || problem.get().map(|p| view! { <p class="problem" role="alert">{p}</p> })}
                    </div>
                    <div class="modal-footer">
                        <button type="button" on:click=move |_| decide(deny.clone(), false)>"Deny"</button>
                        <button type="button" class="primary" on:click=move |_| decide(allow.clone(), true)>"Allow"</button>
                    </div>
                </Modal>
            }
        })}
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::after;
    use jstream_control::protocol::{Event, PairingAsk, PairingAskOutcome};

    fn ask(id: &str) -> PairingAsk {
        PairingAsk::new(id, "Stream Deck", "192.168.1.20")
    }

    #[test]
    fn asks_queue_in_order_and_leave_when_ended() {
        let one = after(&[], &Event::PairingAsked { ask: ask("ask_1") }).unwrap_or_default();
        let two = after(&one, &Event::PairingAsked { ask: ask("ask_2") }).unwrap_or_default();
        assert_eq!(two, vec![ask("ask_1"), ask("ask_2")]);
        let again = after(&two, &Event::PairingAsked { ask: ask("ask_2") });
        assert_eq!(again.as_ref().map(Vec::len), Some(2));
        let left = after(
            &two,
            &Event::PairingAskEnded {
                ask_id: "ask_1".into(),
                outcome: PairingAskOutcome::TimedOut,
            },
        );
        assert_eq!(left, Some(vec![ask("ask_2")]));
        assert_eq!(after(&two, &Event::EventsDropped { count: 1 }), None);
    }
}
