//! "Receive from JTunnel senders" ([ADR-0123](../../../docs/adr/0123-lan-inputs-jtunnel-and-visca.md)
//! decisions 1 and 3), on the Settings dialog's Devices page (`settings.rs`), in the
//! desktop app only.
//!
//! - **The switch** turns the JTunnel receiver on for this machine. Off until the person
//!   turns it on.
//! - **One live sender.** Any number of senders may be accepted, but JStream receives
//!   from one at a time, so what reads it names no sender ([`bound_input`]). The live one is marked **Live**; each other one stands by with
//!   **Make live**, which switches to it. A sender added or accepted becomes the live
//!   one, replacing the one before, which stays accepted.
//! - **The accepted senders** are listed by tunnel name and the address and port they
//!   send from, each with **Use this sender's state** (ADR-0171 decision 1, on once
//!   the sender is accepted, until the person unticks it: the DAW's transport, tracks and clips become live data that
//!   bindings and sources may read) and Remove. The add row takes a name and an `address:port`; the
//!   app checks the sender as the engine would (a private IPv4 address, a name no other
//!   sender has) and the page shows why it refused one.
//! - **Senders on this network** (protocol 0.114): while the app's receiver runs, the
//!   senders heard on the LAN over mDNS (`jtunnel.senders`), read again on each
//!   `jtunnel.senders_changed`. Each one not on the list has Accept…, which asks first
//!   (ADR-0123 §3: the first use of a sender is explicit), naming the sender and its
//!   address and what accepting lets it do; confirming adds it and makes it live. A
//!   sender already accepted by its name that is now heard at another address (its
//!   DAW restarted on another port) asks the same way, and confirming moves it there.
//!   An accepted one standing by has Make live. A name is the sender's own text, shown
//!   as text. Listing a sender trusts it with nothing.
//! - **Saving.** Each change asks the app to save the whole setting
//!   (`host::set_jtunnel`, the app's own command, which no engine client reaches). A
//!   refused change is undone on the page. The app applies a saved change at once,
//!   with no restart, and the senders heard are read again when receiving turns on.
//!
//! Docs: crates/jstream-ui/README.md#design

use crate::app::SessionContext;
use crate::host::{JtunnelSender, JtunnelSetting};
use crate::store::Studio;
use jstream_control::Control;
use jstream_control::protocol::{Call, CallResult, JtunnelSenderInfo, Query, QueryResult};
use leptos::prelude::*;
use leptos::task::spawn_local;

/// The input a reader learned from `input` names: none when `input` is a JTunnel sender
/// this app accepts, since the app receives from one sender at a time and a reader
/// naming no input reads the live one, whatever its tunnel name (ADR-0218); `input`
/// itself for anything else (a MIDI port), or outside the desktop app.
pub fn bound_input(input: &str) -> Option<String> {
    let jtunnel = crate::host::desktop_jtunnel()
        .is_some_and(|setting| setting.accepted.iter().any(|s| s.name == input));
    bound_input_of(input, jtunnel)
}

/// [`bound_input`] once it is known whether `input` is an accepted JTunnel sender.
pub fn bound_input_of(input: &str, jtunnel: bool) -> Option<String> {
    (!jtunnel && !input.is_empty()).then(|| input.to_owned())
}

/// `jtunnel.senders`: whether the receiver runs, and the senders it heard.
///
/// # Errors
/// The call failed, or the engine answered with something else.
pub async fn heard(control: &dyn Control) -> Result<(bool, Vec<JtunnelSenderInfo>), String> {
    let call = Call::Query {
        query: Query::JtunnelSenders,
    };
    match control.call(call).await {
        Ok(response) => match response.result {
            CallResult::Query {
                result: QueryResult::JtunnelSenders { on, senders },
            } => Ok((on, senders)),
            _ => Err("the engine answered `jtunnel.senders` with something else".into()),
        },
        Err(error) => Err(error.to_string()),
    }
}

/// Whether `setting` accepts the sender `name` at `address`.
pub fn accepts(setting: &JtunnelSetting, name: &str, address: &str) -> bool {
    setting
        .accepted
        .iter()
        .any(|sender| sender.name == name && sender.address == address)
}

/// The setting with one more sender, or why the page won't ask the app to add it. The
/// app checks the address itself; this only catches what is plainly missing.
pub fn with_sender(
    setting: &JtunnelSetting,
    name: &str,
    address: &str,
) -> Result<JtunnelSetting, &'static str> {
    let name = name.trim();
    let address = address.trim();
    if name.is_empty() {
        return Err("Type the sender's tunnel name.");
    }
    if address.is_empty() {
        return Err("Type the address and port it sends from, such as 192.168.1.5:9000.");
    }
    if setting.accepted.iter().any(|sender| sender.name == name) {
        return Err("A sender with that name is already accepted.");
    }
    let mut next = setting.clone();
    next.accepted.push(JtunnelSender {
        name: name.to_owned(),
        address: address.to_owned(),
        state: true,
    });
    next.live = Some(name.to_owned());
    Ok(next)
}

/// The setting with a sender heard on the LAN accepted and live: added, or, when a
/// sender of that name is accepted at another address, moved there with its "Use this
/// sender's state" kept.
pub fn with_heard(
    setting: &JtunnelSetting,
    name: &str,
    address: &str,
) -> Result<JtunnelSetting, &'static str> {
    let Some(at) = setting
        .accepted
        .iter()
        .position(|sender| sender.name == name)
    else {
        return with_sender(setting, name, address);
    };
    let mut next = setting.clone();
    address.trim().clone_into(&mut next.accepted[at].address);
    next.live = Some(name.to_owned());
    Ok(next)
}

/// The setting with `name` the live sender.
pub fn with_live(setting: &JtunnelSetting, name: &str) -> JtunnelSetting {
    let mut next = setting.clone();
    next.live = Some(name.to_owned());
    next
}

/// The switch and the accepted senders. Shows nothing outside the desktop app.
#[component]
pub fn JtunnelSettingView() -> impl IntoView {
    let Some(started) = crate::host::desktop_jtunnel() else {
        return ().into_any();
    };
    let setting = RwSignal::new(started);
    let problem = RwSignal::new(None::<String>);
    let name = RwSignal::new(String::new());
    let address = RwSignal::new(String::new());
    let save = move |next: JtunnelSetting, added: bool| {
        let before = setting.get_untracked();
        setting.set(next.clone());
        spawn_local(async move {
            match crate::host::set_jtunnel(&next).await {
                None => {
                    problem.try_set(None);
                    if added {
                        name.try_set(String::new());
                        address.try_set(String::new());
                    }
                }
                Some(why) => {
                    setting.try_set(before);
                    problem.try_set(Some(why));
                }
            }
        });
    };
    let toggle = move |on: bool| {
        let mut next = setting.get_untracked();
        next.on = on;
        save(next, false);
    };
    let remove = move |gone: String| {
        let mut next = setting.get_untracked();
        next.accepted.retain(|sender| sender.name != gone);
        if next.live.as_deref() == Some(gone.as_str()) {
            // The first one left takes over.
            next.live = None;
        }
        save(next, false);
    };
    let make_live = move |name: String| save(with_live(&setting.get_untracked(), &name), false);
    let use_state = move |name: String, on: bool| {
        let mut next = setting.get_untracked();
        for sender in &mut next.accepted {
            if sender.name == name {
                sender.state = on;
            }
        }
        save(next, false);
    };
    let add = move || match with_sender(
        &setting.get_untracked(),
        &name.get_untracked(),
        &address.get_untracked(),
    ) {
        Ok(next) => save(next, true),
        Err(why) => {
            problem.set(Some(why.to_owned()));
        }
    };
    // The senders heard on the LAN, read when the page opens and on each change.
    let listed = RwSignal::new(None::<(bool, Vec<JtunnelSenderInfo>)>);
    let pending = RwSignal::new(None::<(String, String)>);
    if let (Some(session), Some(studio)) =
        (use_context::<SessionContext>(), use_context::<Studio>())
    {
        Effect::new(move |_| {
            studio.jtunnel_changed.track();
            let Some(current) = session.get() else {
                return;
            };
            spawn_local(async move {
                match heard(&*current.control).await {
                    Ok(now) => {
                        listed.try_set(Some(now));
                    }
                    Err(why) => {
                        problem.try_set(Some(why));
                    }
                }
            });
        });
    }
    let accept = move |name: String, address: String| {
        pending.set(None);
        match with_heard(&setting.get_untracked(), &name, &address) {
            Ok(next) => save(next, false),
            Err(why) => problem.set(Some(why.to_owned())),
        }
    };
    let heard_view = move || {
        let (on, senders) = listed.get()?;
        if !on {
            return Some(
                view! {
                    <p class="settings-check-note">
                        "Senders on this network show here while receiving is on."
                    </p>
                }
                .into_any(),
            );
        }
        if senders.is_empty() {
            return Some(
                view! { <p class="placeholder">"No JTunnel sender has answered on this network."</p> }
                    .into_any(),
            );
        }
        Some(
            view! {
                <h4 class="jtunnel-heard-title">"Senders on this network"</h4>
                <ul class="device-list">
                    {senders.into_iter().map(|sender| {
                        let name = sender.remote_name.clone();
                        let address = sender.address.clone();
                        let ask = (name.clone(), address.clone());
                        let shown = (name.clone(), address.clone());
                        view! {
                            <li class="device">
                                <span class="device-name">{name}</span>
                                <span class="device-dates">{address}</span>
                                {move || {
                                    let (name, address) = shown.clone();
                                    let (known, live) = setting.with(|now| {
                                        (accepts(now, &name, &address), now.live_name() == Some(name.as_str()))
                                    });
                                    if known && live {
                                        view! { <span class="jtunnel-live">"Live"</span> }.into_any()
                                    } else if known {
                                        view! {
                                            <button type="button" on:click=move |_| make_live(name.clone())>
                                                "Make live"
                                            </button>
                                        }
                                        .into_any()
                                    } else {
                                        let ask = ask.clone();
                                        view! {
                                            <button type="button" on:click=move |_| pending.set(Some(ask.clone()))>
                                                "Accept…"
                                            </button>
                                        }
                                        .into_any()
                                    }
                                }}
                            </li>
                        }
                    }).collect_view()}
                </ul>
            }
            .into_any(),
        )
    };
    let confirm_view = move || {
        let (name, address) = pending.get()?;
        let (n, a) = (name.clone(), address.clone());
        let replaces = setting.with(|now| {
            let moved = now
                .accepted
                .iter()
                .find(|sender| sender.name == name)
                .map(|sender| sender.address.clone());
            let live = now
                .live_name()
                .filter(|live| *live != name)
                .map(str::to_owned);
            match (moved, live) {
                (Some(before), _) => {
                    format!(" It moves from {before}, and becomes the live sender.")
                }
                (None, Some(live)) => {
                    format!(" It becomes the live sender in place of {live}, which stays accepted.")
                }
                (None, None) => " It becomes the live sender.".to_owned(),
            }
        });
        Some(view! {
            <div class="jtunnel-confirm" role="alertdialog" aria-label="Accept this sender?">
                <p>
                    "Accept the JTunnel sender "<strong>{name}</strong>" at "<code>{address}</code>
                    "? Its audio can then feed a network audio source that names it, and its MIDI the MIDI sources. Only accept a sender you know is yours: anyone on this network can announce a name."
                    {replaces}
                </p>
                <button type="button" class="primary" on:click=move |_| accept(n.clone(), a.clone())>"Accept"</button>
                <button type="button" on:click=move |_| pending.set(None)>"Cancel"</button>
            </div>
        })
    };
    view! {
        <section class="jtunnel-setting">
            <label class="settings-check">
                <input
                    type="checkbox"
                    aria-label="Receive from JTunnel senders"
                    prop:checked=move || setting.with(|now| now.on)
                    on:change=move |event| toggle(event_target_checked(&event))
                />
                <span>
                    <span class="settings-check-label">"Receive from JTunnel senders"</span>
                    <span class="settings-check-note">
                        "When on, JStream takes audio and MIDI from the live JTunnel sender below, over UDP on this network, and the DAW's state from it unless you untick it. You can accept several senders, but only one is live at a time; the others stand by. Only the senders you accept here get in, each from the address and port it sends from. Off until you turn it on."
                    </span>
                </span>
            </label>
            {move || {
                let list = setting.with(|now| now.accepted.clone());
                if list.is_empty() {
                    return view! { <p class="placeholder">"No sender is accepted."</p> }.into_any();
                }
                view! {
                    <ul class="device-list">
                        {list.into_iter().map(|sender| {
                            let gone = sender.name.clone();
                            let named = sender.name.clone();
                            let picked = sender.name.clone();
                            let label = format!("Use the state of {}", sender.name);
                            let live = setting.with(|now| now.live_name() == Some(sender.name.as_str()));
                            view! {
                                <li class="device" class:jtunnel-is-live=live>
                                    <span class="device-name">{sender.name}</span>
                                    <span class="device-dates">{sender.address}</span>
                                    {if live {
                                        view! { <span class="jtunnel-live">"Live"</span> }.into_any()
                                    } else {
                                        view! {
                                            <button type="button" title="Receive from this sender instead of the live one" on:click=move |_| make_live(picked.clone())>
                                                "Make live"
                                            </button>
                                        }
                                        .into_any()
                                    }}
                                    <label class="jtunnel-state" title="Take the DAW's state from this sender (its transport, tracks and clips), for bindings and sources to read. On once the sender is accepted.">
                                        <input
                                            type="checkbox"
                                            aria-label=label
                                            prop:checked=sender.state
                                            on:change=move |event| use_state(named.clone(), event_target_checked(&event))
                                        />
                                        "Use this sender's state"
                                    </label>
                                    <button type="button" on:click=move |_| remove(gone.clone())>"Remove"</button>
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                }
                .into_any()
            }}
            {heard_view}
            {confirm_view}
            <form class="jtunnel-add" on:submit=move |event| { event.prevent_default(); add(); }>
                <input
                    type="text"
                    aria-label="Sender name"
                    placeholder="Name"
                    prop:value=move || name.get()
                    on:input=move |event| name.set(event_target_value(&event))
                />
                <input
                    type="text"
                    aria-label="Sender address and port"
                    placeholder="192.168.1.5:9000"
                    prop:value=move || address.get()
                    on:input=move |event| address.set(event_target_value(&event))
                />
                <button type="submit">"Add sender"</button>
            </form>
            {move || problem.get().map(|why| view! { <p class="problem" role="alert">{why}</p> })}
        </section>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::with_sender;
    use crate::host::{JtunnelSender, JtunnelSetting};

    fn sender(name: &str, address: &str, state: bool) -> JtunnelSender {
        JtunnelSender {
            name: name.to_owned(),
            address: address.to_owned(),
            state,
        }
    }

    #[test]
    fn the_setting_reads_and_writes_the_apps_json() {
        let text = r#"{"on":true,"accepted":[{"name":"Bitwig","address":"192.168.1.5:9000"},{"name":"Live","address":"10.0.0.2:9000","state":false},{"name":7}]}"#;
        let setting = JtunnelSetting::parse(text);
        assert!(setting.on);
        assert_eq!(
            setting.accepted,
            vec![
                sender("Bitwig", "192.168.1.5:9000", true),
                sender("Live", "10.0.0.2:9000", false),
            ],
            "an entry it can't read is left out, and state is on unless it says off"
        );
        assert_eq!(JtunnelSetting::parse(&setting.to_json()), setting);
        assert_eq!(JtunnelSetting::parse("not json"), JtunnelSetting::default());
        assert!(!JtunnelSetting::parse(r#"{"on":"yes"}"#).on);
    }

    #[test]
    fn a_heard_sender_is_accepted_only_by_its_name_at_its_address() {
        let one =
            with_sender(&JtunnelSetting::default(), "Bitwig", "192.168.1.5:9000").expect("added");
        assert!(super::accepts(&one, "Bitwig", "192.168.1.5:9000"));
        assert!(!super::accepts(&one, "Bitwig", "192.168.1.6:9000"));
        assert!(!super::accepts(&one, "bitwig", "192.168.1.5:9000"));
    }

    #[test]
    fn a_sender_needs_a_name_an_address_and_a_name_of_its_own() {
        let none = JtunnelSetting::default();
        let one = with_sender(&none, " Bitwig ", " 192.168.1.5:9000 ").expect("added");
        assert_eq!(
            one.accepted,
            vec![sender("Bitwig", "192.168.1.5:9000", true)],
            "a new sender's state is on"
        );
        assert!(with_sender(&none, "  ", "192.168.1.5:9000").is_err());
        assert!(with_sender(&none, "Bitwig", "").is_err());
        assert!(with_sender(&one, "Bitwig", "192.168.1.6:9000").is_err());
    }

    /// One live sender: an added or accepted one becomes live, a sender heard at a new
    /// address moves there, and with none named the first is live.
    #[test]
    fn one_sender_is_live_and_a_moved_one_follows_its_new_address() {
        let one =
            with_sender(&JtunnelSetting::default(), "Bitwig", "192.168.1.57:47000").expect("added");
        assert_eq!(one.live_name(), Some("Bitwig"));
        let two = super::with_heard(&one, "Live", "192.168.1.58:47000").expect("accepted");
        assert_eq!(two.live_name(), Some("Live"), "the accepted one goes live");
        assert_eq!(two.accepted.len(), 2, "the one before stays accepted");
        let back = super::with_live(&two, "Bitwig");
        assert_eq!(back.live_name(), Some("Bitwig"));
        let mut off = two.clone();
        off.accepted[1].state = false;
        let moved = super::with_heard(&off, "Live", "192.168.1.58:47001").expect("moved");
        assert_eq!(moved.accepted.len(), 2);
        assert_eq!(moved.accepted[1].address, "192.168.1.58:47001");
        assert!(!moved.accepted[1].state, "its state switch is kept");
        let unnamed = JtunnelSetting {
            live: None,
            ..two.clone()
        };
        assert_eq!(unnamed.live_name(), Some("Bitwig"));
        let gone = JtunnelSetting {
            live: Some("Removed".into()),
            ..two
        };
        assert_eq!(gone.live_name(), Some("Bitwig"));
        assert_eq!(JtunnelSetting::parse(&moved.to_json()), moved);
    }

    /// ADR-0218: a reader learned from an accepted JTunnel sender names no input, so it
    /// follows whichever sender is live; a MIDI port keeps its name.
    #[test]
    fn a_reader_learned_from_a_jtunnel_sender_names_no_input() {
        assert_eq!(super::bound_input_of("Bitwig", true), None);
        assert_eq!(
            super::bound_input_of("Pads", false),
            Some("Pads".to_owned())
        );
        assert_eq!(super::bound_input_of("", false), None);
    }
}
