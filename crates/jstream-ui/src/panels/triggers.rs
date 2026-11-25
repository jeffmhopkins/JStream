//! Keys and MIDI triggers in the GUI (ADR-0024 §5, ADR-0122, ADR-0208): the trigger list,
//! the action picker and the MIDI trigger form, and Settings › Keys & MIDI, which shows
//! them together.
//!
//! Each key or MIDI trigger runs every action listed beside it, in order. The list shows
//! every trigger in the collection; "+ Action" on a row opens the picker, which names
//! every action a person binds (go live, record, outputs, scenes, items, channels,
//! sequences, sources) and adds it after what the trigger runs already. A chip's ✕
//! unbinds that one action, and Remove unbinds the trigger. The model is
//! [`crate::bindings`]; these components are what other panels reuse (the Mappings
//! panel's Triggers tab lists its keys and MIDI with [`TriggerList`]).
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#keyboard

use crate::app::SessionContext;
use crate::bindings::{self, Needs, Pick, When};
use crate::panels::learn::{KeyCapture, controls, moved};
use crate::store::Studio;
use jstream_control::protocol::{Action, MidiControlInfo, Trigger};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::time::Duration;

/// How often MIDI learn reads the inputs' latest controls.
const POLL: Duration = Duration::from_millis(150);
/// How many times MIDI learn reads before it gives up: 15 s.
const TRIES: u32 = 100;

/// The note under the MIDI form about JTunnel senders, whose MIDI runs triggers only
/// where the person allowed it (ADR-0123 §3).
pub const JTUNNEL_MIDI_NOTE: &str = "A JTunnel sender's MIDI runs triggers only when \"Use this sender's MIDI\" is on for it in Settings › Devices.";

/// Settings › Keys & MIDI: every key and MIDI trigger in the project, with what each
/// runs, a filter, and "+ Key" and "+ MIDI trigger" to add one.
#[component]
pub fn KeysAndMidi() -> impl IntoView {
    let filter = RwSignal::new(String::new());
    // A new trigger being captured, then waiting for its first action.
    let adding = RwSignal::new(None::<Adding>);
    let fresh = move || match adding.get() {
        None => ().into_any(),
        Some(Adding::Key) => view! {
            <div class="trigger-new" role="group" aria-label="New key">
                <KeyCapture done=move |chord| {
                    adding.set(chord.map(|chord| Adding::Pick(When::Key(chord))));
                } />
            </div>
        }
        .into_any(),
        Some(Adding::Midi) => view! {
            <MidiTriggerForm
                done=move |trigger| adding.set(trigger.map(|t| Adding::Pick(When::Midi(t))))
            />
        }
        .into_any(),
        Some(Adding::Pick(when)) => {
            view! { <NewTrigger when=when done=move || adding.set(None) /> }.into_any()
        }
    };
    view! {
        <section class="prop-section keys-and-midi">
            <p class="settings-check-note">
                "Each key or MIDI trigger runs every action beside it, in order: one key can go live and switch the scene. Keys work while JStream has the focus, and in the desktop app anywhere when global hotkeys are on."
            </p>
            <div class="trigger-toolbar">
                <input
                    type="search"
                    class="trigger-filter"
                    aria-label="Find a key, pad or action"
                    placeholder="Find a key, pad or action"
                    prop:value=move || filter.get()
                    on:input=move |event| filter.set(event_target_value(&event))
                />
                <button type="button" on:click=move |_| adding.set(Some(Adding::Key))>"+ Key"</button>
                <button type="button" on:click=move |_| adding.set(Some(Adding::Midi))>"+ MIDI trigger"</button>
            </div>
            {fresh}
            <TriggerList filter=filter />
        </section>
    }
}

/// What "+ Key" or "+ MIDI trigger" is doing.
#[derive(Clone, Debug, PartialEq)]
enum Adding {
    /// Waiting for the key.
    Key,
    /// Filling in, or learning, the MIDI trigger.
    Midi,
    /// The trigger is chosen; its first action is being picked.
    Pick(When),
}

/// A trigger just chosen, with what it runs already (it may be bound) and the picker for
/// the action to add.
#[component]
fn NewTrigger(when: When, done: impl Fn() + Copy + Send + Sync + 'static) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let label = when.label();
    let note = {
        let when = when.clone();
        move || {
            studio
                .collection
                .with(|c| c.as_ref().and_then(|c| bindings::in_use(c, &when)))
        }
    };
    view! {
        <div class="trigger-new" role="group" aria-label=format!("Add an action to {label}")>
            <span class="trigger-when">{label.clone()}</span>
            {move || note().map(|note| view! { <span class="settings-check-note">{note}" · the action you pick runs after these."</span> })}
            <ActionPicker when=when done=done />
        </div>
    }
}

/// Every key and MIDI trigger in the collection whose label or actions hold `filter`,
/// each with the actions it runs as chips (✕ unbinds one), "+ Action" and Remove.
#[component]
pub fn TriggerList(
    /// Text a row's trigger or actions must hold; empty for every row.
    #[prop(into)]
    filter: Signal<String>,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    // The row whose picker is open.
    let open = RwSignal::new(None::<When>);
    let rows = move || {
        studio.collection.with(|c| {
            let Some(c) = c.as_ref() else {
                return Vec::new();
            };
            bindings::filtered(bindings::rows(c), c, &filter.get())
                .into_iter()
                .map(|row| {
                    let does: Vec<(Action, String)> = row
                        .does
                        .into_iter()
                        .map(|a| {
                            let text = bindings::describe(&a, c);
                            (a, text)
                        })
                        .collect();
                    (row.when, does)
                })
                .collect::<Vec<_>>()
        })
    };
    let apply = move |edits: Vec<jstream_control::protocol::Edit>| {
        if let Some(s) = session.get() {
            studio.apply(&s, edits);
        }
    };
    move || {
        let rows = rows();
        if rows.is_empty() {
            let text = if filter.with(|f| f.trim().is_empty()) {
                "No key or MIDI trigger runs anything yet."
            } else {
                "Nothing matches."
            };
            return view! { <p class="placeholder">{text}</p> }.into_any();
        }
        view! {
            <ul class="trigger-list">
                {rows.into_iter().map(|(when, does)| {
                    let label = when.label();
                    let kind = match when { When::Key(_) => "key", When::Midi(_) => "midi" };
                    let chips = does.into_iter().map(|(action, text)| {
                        let unbind = when.unbind(Some(action));
                        let aria = format!("Stop {label} running {text}");
                        view! {
                            <li class="trigger-action">
                                <span>{text}</span>
                                <button
                                    type="button"
                                    class="icon-button"
                                    aria-label=aria
                                    title="Unbind this action"
                                    on:click=move |_| apply(vec![unbind.clone()])
                                >"✕"</button>
                            </li>
                        }
                    }).collect_view();
                    let (w_open, w_toggle, w_picker) = (when.clone(), when.clone(), when.clone());
                    let clear = when.unbind(None);
                    view! {
                        <li class="trigger-row" data-kind=kind>
                            <span class="trigger-when">{label.clone()}</span>
                            <ul class="trigger-does">{chips}</ul>
                            <span class="trigger-buttons">
                                <button
                                    type="button"
                                    aria-expanded=move || open.with(|o| o.as_ref() == Some(&w_open)).to_string()
                                    on:click=move |_| open.update(|o| {
                                        *o = if o.as_ref() == Some(&w_toggle) { None } else { Some(w_toggle.clone()) };
                                    })
                                >"+ Action"</button>
                                <button
                                    type="button"
                                    aria-label=format!("Remove {label}")
                                    on:click=move |_| apply(vec![clear.clone()])
                                >"Remove"</button>
                            </span>
                            {move || open.with(|o| o.as_ref() == Some(&w_picker)).then(|| {
                                view! { <ActionPicker when=w_picker.clone() done=move || open.set(None) /> }
                            })}
                        </li>
                    }
                }).collect_view()}
            </ul>
        }
        .into_any()
    }
}

/// Picks an action to bind to `when`, from every kind a person binds ([`Pick`]), with
/// its target and amount, and shows what it will run before Add. `done` runs after Add
/// or Cancel.
#[component]
pub fn ActionPicker(
    /// The trigger the picked action is added to.
    when: When,
    /// Runs after Add or Cancel.
    done: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let pick = RwSignal::new(Pick::GoLive);
    let chosen = RwSignal::new(String::new());
    let amount = RwSignal::new(10.0_f64);
    let when = StoredValue::new(when);
    let choices = move || {
        let needs = pick.get().needs();
        studio.collection.with(|c| {
            c.as_ref()
                .map(|c| bindings::targets(c, needs))
                .unwrap_or_default()
        })
    };
    // The target: the one chosen while it is still offered, else the first offered.
    let target = move || {
        let choices = choices();
        let chosen = chosen.get();
        if choices.iter().any(|(id, _)| *id == chosen) {
            chosen
        } else {
            choices
                .into_iter()
                .next()
                .map(|(id, _)| id)
                .unwrap_or_default()
        }
    };
    let actions = move || {
        let (pick, target, amount) = (pick.get(), target(), amount.get());
        studio.collection.with(|c| {
            c.as_ref()
                .map(|c| pick.actions(c, &target, amount))
                .unwrap_or_default()
        })
    };
    let preview = move || {
        let actions = actions();
        if actions.is_empty() {
            return match pick.get() {
                Pick::GoLive => "Add a stream output first.".to_owned(),
                Pick::Record => "Add a recording output first.".to_owned(),
                Pick::ChannelNudge => "Nudge by an amount other than 0.".to_owned(),
                _ => "Nothing to pick in this project.".to_owned(),
            };
        }
        studio.collection.with(|c| {
            c.as_ref()
                .map(|c| {
                    actions
                        .iter()
                        .map(|a| bindings::describe(a, c))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default()
        })
    };
    let add = move || {
        let actions = actions();
        let edits = studio.collection.with_untracked(|c| {
            c.as_ref()
                .map(|c| when.with_value(|w| bindings::add_edits(c, w, actions)))
                .unwrap_or_default()
        });
        if !edits.is_empty()
            && let Some(s) = session.get()
        {
            studio.apply(&s, edits);
        }
        done();
    };
    let mut groups: Vec<&'static str> = Vec::new();
    for p in Pick::ALL {
        if !groups.contains(&p.group()) {
            groups.push(p.group());
        }
    }
    view! {
        <div class="action-picker" role="group" aria-label="Pick an action">
            <select
                aria-label="Action"
                on:change=move |event| {
                    if let Some(p) = Pick::from_key(&event_target_value(&event)) {
                        pick.set(p);
                        chosen.set(String::new());
                        amount.set(if p == Pick::ChannelSet { 100.0 } else { 10.0 });
                    }
                }
            >
                {groups.into_iter().map(|group| view! {
                    <optgroup label=group>
                        {Pick::ALL.into_iter().filter(|p| p.group() == group).map(|p| view! {
                            <option value=p.key() selected=move || pick.get() == p>{p.label()}</option>
                        }).collect_view()}
                    </optgroup>
                }).collect_view()}
            </select>
            {move || (pick.get().needs() != Needs::Nothing).then(|| view! {
                <select
                    aria-label="On"
                    on:change=move |event| chosen.set(event_target_value(&event))
                >
                    {move || {
                        let current = target();
                        choices().into_iter().map(|(id, name)| {
                            let selected = id == current;
                            view! { <option value=id selected=selected>{name}</option> }
                        }).collect_view()
                    }}
                </select>
            })}
            {move || (pick.get().needs() == Needs::ChannelAmount).then(|| view! {
                <label class="action-amount">
                    <input
                        type="number"
                        aria-label=move || if pick.get() == Pick::ChannelSet { "Value, in percent" } else { "Amount, in percent" }
                        step="1"
                        min=move || if pick.get() == Pick::ChannelSet { "0" } else { "-100" }
                        max="100"
                        prop:value=move || amount.get().to_string()
                        on:input=move |event| {
                            if let Ok(v) = event_target_value(&event).parse::<f64>() {
                                amount.set(v);
                            }
                        }
                    />
                    " %"
                </label>
            })}
            <span class="action-preview" aria-live="polite">{preview}</span>
            <button
                type="button"
                class="primary"
                prop:disabled=move || actions().is_empty()
                on:click=move |_| add()
            >"Add"</button>
            <button type="button" on:click=move |_| done()>"Cancel"</button>
        </div>
    }
}

/// A MIDI trigger, typed or learned: its kind (note, control change or program
/// change), number, channel and input. Learn takes the next control moved on any input.
/// `done` gets the trigger on Next, or `None` on Cancel.
#[component]
pub fn MidiTriggerForm(
    /// Gets the trigger on Next, or `None` on Cancel.
    done: impl Fn(Option<Trigger>) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let session = expect_context::<SessionContext>();
    let kind = RwSignal::new("note".to_owned());
    let number = RwSignal::new(36_u32);
    let channel = RwSignal::new(0_u32);
    let input = RwSignal::new(String::new());
    let learning = RwSignal::new(false);
    let problem = RwSignal::new(None::<String>);
    let trigger =
        move || bindings::midi_trigger(&kind.get(), number.get(), channel.get(), &input.get());
    let take = move |control: MidiControlInfo| {
        kind.set("cc".into());
        number.set(u32::from(control.controller));
        channel.set(u32::from(control.channel));
        input.set(control.input);
    };
    let learn = move || {
        let Some(s) = session.get() else {
            return;
        };
        problem.set(None);
        learning.set(true);
        spawn_local(async move {
            match controls(&s).await {
                Some(baseline) => watch(s, learning, problem, baseline, take, TRIES),
                None => {
                    learning.try_set(false);
                    problem.try_set(Some("This engine reads no MIDI.".into()));
                }
            }
        });
    };
    let number_input =
        |signal: RwSignal<u32>, label: &'static str, min: &'static str, max: &'static str| {
            view! {
                <input
                    type="number"
                    aria-label=label
                    title=label
                    min=min
                    max=max
                    step="1"
                    prop:value=move || signal.get().to_string()
                    on:input=move |event| {
                        if let Ok(v) = event_target_value(&event).parse::<u32>() {
                            signal.set(v);
                        }
                    }
                />
            }
        };
    view! {
        <div class="trigger-new midi-trigger-form" role="group" aria-label="New MIDI trigger">
            <select aria-label="MIDI message" on:change=move |event| kind.set(event_target_value(&event))>
                <option value="note" selected=move || kind.get() == "note">"Note"</option>
                <option value="cc" selected=move || kind.get() == "cc">"Control change"</option>
                <option value="program" selected=move || kind.get() == "program">"Program change"</option>
            </select>
            {number_input(number, "Number, 0 to 127", "0", "127")}
            <label>"Channel " {number_input(channel, "Channel, 1 to 16, 0 for any", "0", "16")}</label>
            <input
                type="text"
                aria-label="Input, blank for any"
                placeholder="Any input"
                prop:value=move || input.get()
                on:input=move |event| input.set(event_target_value(&event))
            />
            <button
                type="button"
                aria-pressed=move || learning.get().to_string()
                on:click=move |_| if learning.get_untracked() { learning.set(false) } else { learn() }
            >{move || if learning.get() { "Move a control…" } else { "Learn" }}</button>
            <button
                type="button"
                class="primary"
                prop:disabled=move || trigger().is_none()
                on:click=move |_| {
                    learning.set(false);
                    if let Some(t) = trigger() {
                        done(Some(t));
                    }
                }
            >"Next"</button>
            <button type="button" on:click=move |_| { learning.set(false); done(None); }>"Cancel"</button>
            <p class="settings-check-note">
                "A control change fires when it crosses the middle upward, so a button fires once per press. Learn hears a knob, fader or button that sends control changes; type a pad's note number. "
                {JTUNNEL_MIDI_NOTE}
            </p>
            {move || problem.get().map(|why| view! { <p class="problem" role="alert">{why}</p> })}
        </div>
    }
}

/// Reads the controls after [`POLL`] and hands `take` the first that moved since
/// `baseline`, or reads again, `tries` more times, while `learning` holds.
fn watch(
    session: crate::session::Session,
    learning: RwSignal<bool>,
    problem: RwSignal<Option<String>>,
    baseline: Vec<MidiControlInfo>,
    take: impl Fn(MidiControlInfo) + Copy + 'static,
    tries: u32,
) {
    set_timeout(
        move || {
            spawn_local(async move {
                if learning.try_get_untracked() != Some(true) {
                    return;
                }
                let now = controls(&session).await.unwrap_or_default();
                if learning.try_get_untracked() != Some(true) {
                    return;
                }
                if let Some(control) = moved(&baseline, &now) {
                    take(control);
                    learning.try_set(false);
                } else if tries == 0 {
                    learning.try_set(false);
                    problem.try_set(Some("No control moved. Type the trigger instead.".into()));
                } else {
                    watch(session, learning, problem, baseline, take, tries - 1);
                }
            });
        },
        POLL,
    );
}
