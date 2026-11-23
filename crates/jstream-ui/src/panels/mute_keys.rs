//! Keys for a mute (ADR-0207): the small key button beside a Mute button in the mixer.
//! It lists the chords bound to that source's or bus's `source.mute` or `bus.mute`, with
//! how each mutes, and binds a new one: a **toggle**, **push to talk** (unmuted while the
//! key is held) or **push to mute** (muted while it is held, for a cough). A key that ran
//! something else is taken from it, as every Set key does. Mapping a MIDI control or a
//! channel onto the mute is the control button's (`Learn`), beside it.

use crate::app::SessionContext;
use crate::icons::Icon;
use crate::panels::learn::KeyCapture;
use crate::store::{Collection, Studio};
use jstream_control::protocol::{Action, BusId, Chord, Edit, MuteTo, SourceId};
use leptos::prelude::*;

/// Whose mute the keys run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MuteOf {
    /// An audio source's.
    Source(SourceId),
    /// A bus's, the master's included.
    Bus(BusId),
}

impl MuteOf {
    /// The action that does `to` to this mute.
    pub fn action(&self, to: MuteTo) -> Action {
        match self {
            MuteOf::Source(source_id) => Action::SourceMute {
                source_id: source_id.clone(),
                to,
            },
            MuteOf::Bus(bus_id) => Action::BusMute {
                bus_id: bus_id.clone(),
                to,
            },
        }
    }

    /// What `action` does to this mute, if it is this mute's.
    fn of(&self, action: &Action) -> Option<MuteTo> {
        match (self, action) {
            (MuteOf::Source(id), Action::SourceMute { source_id, to }) if id == source_id => {
                Some(*to)
            }
            (MuteOf::Bus(id), Action::BusMute { bus_id, to }) if id == bus_id => Some(*to),
            _ => None,
        }
    }
}

/// How a key mutes, as the menu says it.
pub fn mode_name(to: MuteTo) -> &'static str {
    match to {
        MuteTo::Toggle => "Toggle",
        MuteTo::PushToTalk => "Push to talk",
        MuteTo::PushToMute => "Push to mute",
        MuteTo::Muted => "Mute",
        MuteTo::Unmuted => "Unmute",
        _ => "Mute key",
    }
}

/// The chords bound to `of`'s mute in `collection`, with how each mutes, in chord order.
pub fn keys(collection: &Collection, of: &MuteOf) -> Vec<(Chord, MuteTo)> {
    collection
        .hotkeys
        .iter()
        .filter_map(|b| Some((b.chord.clone(), of.of(&b.action()?)?)))
        .collect()
}

/// The modes the menu offers, in its order.
const MODES: [(MuteTo, &str); 3] = [
    (MuteTo::Toggle, "Each press mutes or unmutes"),
    (MuteTo::PushToTalk, "Unmuted while held"),
    (MuteTo::PushToMute, "Muted while held"),
];

/// The key button for `of`'s mute, named `label` in its menu.
#[component]
pub fn MuteKeys(
    /// Whose mute.
    of: MuteOf,
    /// What the menu calls it ("Mic").
    #[prop(into)]
    label: String,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let of = StoredValue::new(of);
    let label = StoredValue::new(label);
    // Where the menu is, and the mode a key is being pressed for.
    let menu = RwSignal::new(None::<(f64, f64)>);
    let capturing = RwSignal::new(None::<MuteTo>);
    let bound = move || {
        studio.collection.with(|c| {
            c.as_ref()
                .map(|c| of.with_value(|of| keys(c, of)))
                .unwrap_or_default()
        })
    };
    let close = move || {
        menu.set(None);
        capturing.set(None);
    };
    let bind = move |chord: Option<Chord>| {
        let to = capturing.get_untracked();
        close();
        let (Some(chord), Some(to), Some(s)) = (chord, to, session.0.get_value()) else {
            return;
        };
        let action = of.with_value(|of| of.action(to));
        let edits = studio.collection.with_untracked(|c| {
            c.as_ref()
                .map(|c| crate::hotkeys::bind_edits(c, &action, &chord))
                .unwrap_or_default()
        });
        if !edits.is_empty() {
            studio.apply(&s, edits);
        }
    };
    let unbind = move |chord: Chord| {
        close();
        if let Some(s) = session.0.get_value() {
            studio.apply(&s, vec![Edit::HotkeyClear { chord }]);
        }
    };
    let title = move || {
        let list = bound();
        if list.is_empty() {
            "Set a key that mutes, or that talks while held.".to_owned()
        } else {
            list.iter()
                .map(|(chord, to)| format!("{}: {}", crate::hotkeys::label(chord), mode_name(*to)))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    let open = move |e: leptos::ev::MouseEvent| {
        let (x, y) = (f64::from(e.client_x()), f64::from(e.client_y()));
        capturing.set(None);
        menu.set(Some((x, y)));
    };
    let shown = move || {
        let (x, y) = menu.get()?;
        let (left, top) = (
            format!("{}px", (x - 200.0).max(4.0)),
            format!("{}px", y + 12.0),
        );
        let name = label.get_value();
        Some(view! {
            <div class="ctx-catcher" on:pointerdown=move |_| close()></div>
            <div class="context-menu control-menu" role="menu" style:left=left style:top=top>
                <div class="ctx-head">{format!("Keys for “{name}” mute")}</div>
                {move || match capturing.get() {
                    Some(to) => view! {
                        <KeyCapture done=bind />
                        <p class="hint control-menu-note">{match to {
                            MuteTo::PushToTalk => "Held, the key unmutes; let go, it mutes again.",
                            MuteTo::PushToMute => "Held, the key mutes; let go, it unmutes again.",
                            _ => "Each press mutes or unmutes.",
                        }}</p>
                    }.into_any(),
                    None => view! {
                        {bound().into_iter().map(|(chord, to)| {
                            let gone = chord.clone();
                            view! {
                                <button type="button" class="menuitem" role="menuitem"
                                    title="Remove this key"
                                    on:click=move |_| unbind(gone.clone())>
                                    <span class="ctx-check"></span>
                                    <span class="ctx-label">{crate::hotkeys::label(&chord)}</span>
                                    <span class="hint">{format!("{} · remove", mode_name(to))}</span>
                                </button>
                            }
                        }).collect_view()}
                        {(!bound().is_empty()).then(|| view! { <div class="menu-sep"></div> })}
                        {MODES.into_iter().map(|(to, hint)| view! {
                            <button type="button" class="menuitem" role="menuitem"
                                on:click=move |_| capturing.set(Some(to))>
                                <span class="ctx-check"></span>
                                <span class="ctx-label">{format!("{} key…", mode_name(to))}</span>
                                <span class="hint">{hint}</span>
                            </button>
                        }).collect_view()}
                    }.into_any(),
                }}
            </div>
        })
    };
    view! {
        <button
            type="button"
            class="learn mute-keys"
            class:bound=move || !bound().is_empty()
            title=title
            aria-label=title
            aria-haspopup="menu"
            on:click=open
        >
            <Icon name="key" size=13 />
        </button>
        {shown}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::HotkeyBinding;

    #[test]
    fn a_mutes_keys_are_the_chords_bound_to_it() {
        let mic = MuteOf::Source(SourceId::new("mic"));
        let master = MuteOf::Bus(BusId::master());
        let collection = Collection {
            hotkeys: vec![
                HotkeyBinding::new(Chord::new("KeyM"), &mic.action(MuteTo::Toggle)),
                HotkeyBinding::new(Chord::new("KeyT"), &mic.action(MuteTo::PushToTalk)),
                HotkeyBinding::new(Chord::new("KeyB"), &master.action(MuteTo::PushToMute)),
                HotkeyBinding::new(
                    Chord::new("KeyO"),
                    &MuteOf::Source(SourceId::new("other")).action(MuteTo::Toggle),
                ),
            ],
            ..Collection::default()
        };
        assert_eq!(
            keys(&collection, &mic),
            vec![
                (Chord::new("KeyM"), MuteTo::Toggle),
                (Chord::new("KeyT"), MuteTo::PushToTalk),
            ]
        );
        assert_eq!(
            keys(&collection, &master),
            vec![(Chord::new("KeyB"), MuteTo::PushToMute)]
        );
        assert_eq!(mode_name(MuteTo::PushToTalk), "Push to talk");
    }
}
