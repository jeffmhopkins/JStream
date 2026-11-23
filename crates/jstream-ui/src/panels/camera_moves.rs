//! The scene Properties dialog's camera moves (ADR-0113, ADR-0120): Follow pointer and
//! Camera motion. Follow pointer is a filter on a display or window capture (ADR-0120),
//! so the dialog only says which capture's filter moves the camera and opens it, or
//! opens Add filter on the scene's first capture; the filter's settings and its Zoom in
//! and Zoom out keys are in the Filters window. Camera motion, the camera's motion
//! modifiers (a wiggle on its position is a hand-held sway), is part of the dialog's
//! draft, sent on OK as `scene.set_camera_motion`, and offered on a scene's own camera
//! only: a keyframe scene draws its parent's.
//!
//! Docs: docs/UI-FEATURES.md#dialogs

use super::motion::{
    KINDS, Param, SHAPES, kind_name, param_value, sized_kind, with_input, with_param,
};
use super::data_read::{DataReads, MOD_SLOTS, mod_reader, mod_slot};
use super::number::NumberField;
use super::scene_properties::SceneDraft;
use crate::store::Collection;
use jstream_control::protocol::{
    CameraMotionModifier, CameraProperty, FilterOwner, ItemId, MAX_MOTION_MODIFIERS, MotionClock,
    MotionKind, SceneId, SourceKind,
};
use leptos::prelude::*;

/// What each camera property is called in the list.
pub fn property_name(property: CameraProperty) -> &'static str {
    match property {
        CameraProperty::PositionX => "Position X",
        CameraProperty::PositionY => "Position Y",
        CameraProperty::PositionZ => "Position Z",
        CameraProperty::TargetX => "Looks at X",
        CameraProperty::TargetY => "Looks at Y",
        CameraProperty::TargetZ => "Looks at Z",
        CameraProperty::Fov => "Field of view",
        CameraProperty::Roll => "Roll",
        CameraProperty::Focus => "Focus distance",
        CameraProperty::Blur => "Blur",
        CameraProperty::Pan => "Pan",
        CameraProperty::Tilt => "Tilt",
        CameraProperty::Autofocus => "Autofocus",
        // One from a newer engine: shown, but not offered.
        _ => "Other",
    }
}

fn unit(property: CameraProperty) -> &'static str {
    match property {
        CameraProperty::Fov | CameraProperty::Roll | CameraProperty::Pan | CameraProperty::Tilt => {
            "°"
        }
        // On at one half or more.
        CameraProperty::Autofocus => "",
        _ => "px",
    }
}

/// A gentle amount for `property`: a few pixels, or a fraction of a degree.
fn gentle(property: CameraProperty) -> f32 {
    match unit(property) {
        "°" => 0.5,
        _ => 6.0,
    }
}

/// The kinds the How select offers a camera: an item's, but audio (a camera hears
/// nothing), and Channel when the collection has a channel to follow.
pub fn camera_kinds(has_channels: bool) -> Vec<&'static str> {
    KINDS
        .into_iter()
        .filter(|k| *k != "Audio")
        .chain(has_channels.then_some("Channel"))
        .collect()
}

/// A new camera modifier of the kind named `name`, `size` its amount. A MIDI one
/// (ADR-0176) follows CC 1 on any input, a DAW data one the first of a JTunnel sender's
/// Mod params `mods` doesn't hold (ADR-0181), and a Channel one `channel`, each across
/// a range big enough to see, resting at the camera's stored view. The clocked kinds are
/// an item's. `None` for Channel with no channel.
pub fn camera_kind(
    name: &str,
    property: CameraProperty,
    size: f32,
    mods: &[u8],
    channel: Option<&str>,
) -> Option<MotionKind> {
    let to = match property {
        // A zoom in: the field of view narrows.
        CameraProperty::Fov => -20.0,
        // A button or a fader past its middle turns autofocus on.
        CameraProperty::Autofocus => 1.0,
        _ if unit(property) == "°" => 45.0,
        _ => 200.0,
    };
    if name == "DAW data" {
        let n = (1..=MOD_SLOTS).find(|n| !mods.contains(n)).unwrap_or(1);
        return Some(mod_reader(n, 0.0, to));
    }
    if name == "Channel" {
        return Some(MotionKind::Channel {
            channel_id: channel?.to_owned(),
            smoothing_ms: 50,
            curve: jstream_control::protocol::MotionCurve::Linear,
            from: jstream_control::protocol::Finite32::ZERO,
            to: jstream_control::protocol::Finite32::new(to)?,
        });
    }
    if name == "MIDI" {
        return Some(MotionKind::Midi {
            control: jstream_control::protocol::MotionMidiControl::Cc,
            controller: Some(1),
            channel: None,
            input: None,
            smoothing_ms: 50,
            curve: jstream_control::protocol::MotionCurve::Linear,
            from: jstream_control::protocol::Finite32::ZERO,
            to: jstream_control::protocol::Finite32::new(to)?,
        });
    }
    sized_kind(name, size)
}

/// `list` with a new hand-held sway (a slow wiggle on the position's x) at its end,
/// under an ID it doesn't use yet; `None` when it is full.
pub fn with_added(list: &[CameraMotionModifier]) -> Option<Vec<CameraMotionModifier>> {
    if list.len() >= MAX_MOTION_MODIFIERS {
        return None;
    }
    let id = (1..)
        .map(|n| format!("camera-{n}"))
        .find(|id| list.iter().all(|m| m.modifier_id != *id))?;
    let property = CameraProperty::PositionX;
    let mut next = list.to_vec();
    next.push(CameraMotionModifier {
        modifier_id: id,
        property,
        kind: sized_kind("Wiggle", gentle(property))?,
        clock: MotionClock::Engine,
        enabled: true,
        amount: None,
    });
    Some(next)
}

/// The items of `scene_id` a follow camera can follow: those showing a display or a
/// window capture, with their source's name.
pub fn followable(collection: &Collection, scene_id: &SceneId) -> Vec<(ItemId, String)> {
    let Some(scene) = collection.scene(scene_id) else {
        return Vec::new();
    };
    scene
        .items
        .iter()
        .filter_map(|item| {
            let source = collection.source(&item.source_id)?;
            match &source.kind {
                SourceKind::Builtin { kind } if kind == "display" || kind == "window" => {
                    Some((item.item_id.clone(), source.name.clone()))
                }
                _ => None,
            }
        })
        .collect()
}

/// What the Follow pointer note says for scene `scene_id` (ADR-0120): the capture whose
/// filter moves its camera, by its source's name, or how to add one.
pub fn follow_note(collection: &Collection, scene_id: &SceneId) -> String {
    let name = |item_id: &ItemId| {
        collection
            .item(item_id)
            .and_then(|(_, item)| collection.source(&item.source_id))
            .map_or_else(|| item_id.to_string(), |source| source.name.clone())
    };
    let following = collection
        .scene(scene_id)
        .and_then(|scene| scene.camera_follow.as_ref());
    match following {
        Some(follow) => format!(
            "Follow pointer is set by the filter on “{}”: the camera glides after the pointer over it.",
            name(&follow.item_id)
        ),
        None if followable(collection, scene_id).is_empty() => {
            "Add a display or window capture to the scene, then add the Follow pointer filter to it to have the camera glide after the pointer.".into()
        }
        None => "Add the Follow pointer filter to a display or window capture to have the camera glide after the pointer.".into(),
    }
}

/// The Follow pointer note and the Camera motion list, editing `draft` for scene
/// `scene_id`, shown while the draft has a camera.
#[component]
pub fn CameraMoves(
    /// The scene the dialog edits.
    scene_id: SceneId,
    /// The dialog's draft.
    draft: RwSignal<SceneDraft>,
) -> impl IntoView {
    let studio = expect_context::<crate::store::Studio>();
    let dialogs = use_context::<crate::panels::filters::FilterDialogs>();
    let scene = StoredValue::new(scene_id);
    let note = Memo::new(move |_| {
        studio.collection.with(|c| {
            c.as_ref()
                .map(|c| scene.with_value(|s| follow_note(c, s)))
                .unwrap_or_default()
        })
    });
    // The item whose filter moves the camera, or the first capture to add one to.
    let target = Memo::new(move |_| {
        studio.collection.with(|c| {
            let c = c.as_ref()?;
            scene.with_value(|s| {
                let following = c
                    .scene(s)
                    .and_then(|scene| scene.camera_follow.as_ref())
                    .map(|f| (f.item_id.clone(), true));
                following.or_else(|| {
                    followable(c, s)
                        .into_iter()
                        .next()
                        .map(|(id, _)| (id, false))
                })
            })
        })
    });
    let open = move |_| {
        let (Some(dialogs), Some((item_id, following))) = (dialogs, target.get_untracked()) else {
            return;
        };
        let owner = FilterOwner::Item { item_id };
        if following {
            dialogs.open(owner);
        } else {
            dialogs.add(owner);
        }
    };

    // The channels a Channel move can follow, this scene's first, by name.
    let channel_options = move |scene_id: SceneId| {
        studio.collection.with(|c| {
            let Some(c) = c.as_ref() else {
                return Vec::new();
            };
            let (here, rest) = crate::panels::channels::search(&c.channels, "", Some(&scene_id));
            here.into_iter()
                .chain(rest)
                .map(|ch| {
                    let name = if ch.name.trim().is_empty() {
                        ch.channel_id.clone()
                    } else {
                        ch.name.clone()
                    };
                    (ch.channel_id.clone(), name)
                })
                .collect::<Vec<_>>()
        })
    };
    let has_channels =
        move || studio.collection.with(|c| c.as_ref().is_some_and(|c| !c.channels.is_empty()));
    // The Mods the draft's camera and the channels already read, and the channel a new
    // Channel move follows.
    let taken = move || {
        let mut mods: Vec<u8> = draft.with_untracked(|d| {
            d.camera_motion.iter().filter_map(|m| mod_slot(&m.kind)).collect()
        });
        studio.collection.with_untracked(|c| {
            if let Some(c) = c.as_ref() {
                mods.extend(c.channels.iter().flat_map(|ch| ch.inputs.iter().filter_map(mod_slot)));
            }
        });
        let channel = untrack(move || channel_options(scene.get_value()))
            .into_iter()
            .next()
            .map(|(id, _)| id);
        (mods, channel)
    };

    // Camera motion: row `n` changed by `change`.
    let change = move |n: usize, change: &dyn Fn(&mut CameraMotionModifier) -> bool| {
        draft.update(|d| {
            if let Some(m) = d.camera_motion.get_mut(n) {
                change(m);
            }
        });
        true
    };
    let row = move |n: usize| {
        let at = move || draft.with(|d| d.camera_motion.get(n).cloned());
        let fields = move || {
            let kind = at()?.kind;
            Some(
                Param::of(&kind)
                    .iter()
                    .map(|param| {
                        let param = *param;
                        let (_, step, decimals, min, max) = param.field();
                        let label = param.label_for(&kind);
                        let unit = match param {
                            Param::Amplitude | Param::Rate | Param::Wrap => {
                                at().map_or("", |m| unit(m.property))
                            }
                            Param::Frequency => "Hz",
                            Param::Period => "ms",
                            _ => "",
                        };
                        let value =
                            Signal::derive(move || at().and_then(|m| param_value(&m.kind, param)));
                        let send = move |v: f64| {
                            change(n, &|m| match with_param(&m.kind, param, v) {
                                Some(kind) if kind != m.kind => {
                                    m.kind = kind;
                                    true
                                }
                                _ => false,
                            })
                        };
                        view! { <NumberField label unit value step decimals min max send /> }
                    })
                    .collect_view(),
            )
        };
        view! {
            <div class="motion-row">
                <div class="field-grid">
                    <label class="select-field">
                        <span class="field-label">"Moves"</span>
                        <select
                            aria-label="What it moves"
                            prop:value=move || at().map_or("", |m| property_name(m.property))
                            on:change=move |event| {
                                let name = event_target_value(&event);
                                change(n, &|m| {
                                    let Some(p) = CameraProperty::ALL
                                        .into_iter()
                                        .find(|p| property_name(*p) == name)
                                    else {
                                        return false;
                                    };
                                    m.property = p;
                                    true
                                });
                            }
                        >
                            {CameraProperty::ALL
                                .into_iter()
                                .map(|p| view! { <option value=property_name(p)>{property_name(p)}</option> })
                                .collect_view()}
                        </select>
                    </label>
                    <label class="select-field">
                        <span class="field-label">"How"</span>
                        <select
                            aria-label="How it moves"
                            prop:value=move || at().map_or("", |m| kind_name(&m.kind))
                            on:change=move |event| {
                                let name = event_target_value(&event);
                                change(n, &|m| {
                                    if kind_name(&m.kind) == name {
                                        return false;
                                    }
                                    let size = param_value(&m.kind, Param::Amplitude)
                                        .map_or_else(|| gentle(m.property), |v| v as f32);
                                    let (mods, channel) = taken();
                                    match camera_kind(&name, m.property, size, &mods, channel.as_deref()) {
                                        Some(kind) => {
                                            m.kind = kind;
                                            true
                                        }
                                        None => false,
                                    }
                                });
                            }
                        >
                            {move || camera_kinds(has_channels())
                                .into_iter()
                                .map(|k| view! { <option value=k>{k}</option> })
                                .collect_view()}
                        </select>
                    </label>
                    <Show when=move || at().is_some_and(|m| matches!(m.kind, MotionKind::Oscillate { .. }))>
                        <label class="select-field">
                            <span class="field-label">"Shape"</span>
                            <select
                                aria-label="Shape"
                                prop:value=move || at().and_then(|m| match m.kind {
                                    MotionKind::Oscillate { shape, .. } => SHAPES
                                        .iter()
                                        .find(|(s, _)| *s == shape)
                                        .map(|(_, name)| *name),
                                    _ => None,
                                }).unwrap_or("")
                                on:change=move |event| {
                                    let name = event_target_value(&event);
                                    let Some(next) = SHAPES
                                        .iter()
                                        .find(|(_, n)| *n == name)
                                        .map(|(s, _)| *s)
                                    else {
                                        return;
                                    };
                                    change(n, &|m| match &mut m.kind {
                                        MotionKind::Oscillate { shape, .. } => {
                                            *shape = next;
                                            true
                                        }
                                        _ => false,
                                    });
                                }
                            >
                                {SHAPES
                                    .iter()
                                    .map(|(_, name)| view! { <option value=*name>{*name}</option> })
                                    .collect_view()}
                            </select>
                        </label>
                    </Show>
                    {fields}
                    <Show when=move || at().is_some_and(|m| matches!(m.kind, MotionKind::Midi { .. }))>
                        <label class="select-field">
                            <span class="field-label">"Input"</span>
                            <input
                                type="text"
                                aria-label="Which MIDI input"
                                placeholder="Any input"
                                title="A MIDI port or JTunnel sender by name; blank for any"
                                maxlength="64"
                                prop:value=move || at().and_then(|m| match m.kind {
                                    MotionKind::Midi { input, .. } => input,
                                    _ => None,
                                }).unwrap_or_default()
                                on:change=move |event| {
                                    let name = event_target_value(&event);
                                    change(n, &|m| match with_input(&m.kind, &name) {
                                        Some(kind) => {
                                            m.kind = kind;
                                            true
                                        }
                                        None => false,
                                    });
                                }
                            />
                        </label>
                    </Show>
                    <Show when=move || at().is_some_and(|m| matches!(m.kind, MotionKind::Data { .. }))>
                        <DataReads
                            kind=Signal::derive(move || at().map(|m| m.kind))
                            send=move |kind| {
                                change(n, &|m| {
                                    m.kind = kind.clone();
                                    true
                                });
                            }
                        />
                    </Show>
                    <Show when=move || at().is_some_and(|m| matches!(m.kind, MotionKind::Channel { .. }))>
                        <label class="select-field">
                            <span class="field-label">"Channel"</span>
                            <select
                                aria-label="Which channel it follows"
                                title="The channel whose value, 0 to 100 %, moves the camera from At 0 % to At 100 %"
                                prop:value=move || at().and_then(|m| match m.kind {
                                    MotionKind::Channel { channel_id, .. } => Some(channel_id),
                                    _ => None,
                                }).unwrap_or_default()
                                on:change=move |event| {
                                    let next = event_target_value(&event);
                                    change(n, &|m| match &mut m.kind {
                                        MotionKind::Channel { channel_id, .. } if *channel_id != next => {
                                            channel_id.clone_from(&next);
                                            true
                                        }
                                        _ => false,
                                    });
                                }
                            >
                                {move || channel_options(scene.get_value())
                                    .into_iter()
                                    .map(|(id, name)| view! { <option value=id>{name}</option> })
                                    .collect_view()}
                            </select>
                        </label>
                    </Show>
                    <label class="select-field">
                        <span class="field-label">"Starts"</span>
                        <select
                            aria-label="When its time starts"
                            title="With the studio clock, or each time the scene goes on program"
                            prop:value=move || at().map_or("", |m| match m.clock {
                                MotionClock::Shown => "shown",
                                _ => "engine",
                            })
                            on:change=move |event| {
                                let clock = match event_target_value(&event).as_str() {
                                    "shown" => MotionClock::Shown,
                                    _ => MotionClock::Engine,
                                };
                                change(n, &|m| {
                                    m.clock = clock;
                                    true
                                });
                            }
                        >
                            <option value="engine">"In step"</option>
                            <option value="shown">"When shown"</option>
                        </select>
                    </label>
                </div>
                <div class="motion-row-actions">
                    <label class="check">
                        <input
                            type="checkbox"
                            prop:checked=move || at().is_some_and(|m| m.enabled)
                            on:change=move |event| {
                                let on = event_target_checked(&event);
                                change(n, &|m| {
                                    m.enabled = on;
                                    true
                                });
                            }
                        />
                        "On"
                    </label>
                    <button
                        type="button"
                        class="motion-button"
                        title="Remove this modifier"
                        on:click=move |_| draft.update(|d| {
                            if n < d.camera_motion.len() {
                                d.camera_motion.remove(n);
                            }
                        })
                    >
                        "Remove"
                    </button>
                </div>
            </div>
        }
    };

    view! {
        <h3 class="camera-head">"Follow pointer"</h3>
        <span class="field-note">{move || note.get()}</span>
        {move || target.get().map(|(_, following)| view! {
            <button type="button" class="motion-button" on:click=open>
                {if following { "Open its filter" } else { "Add Follow pointer…" }}
            </button>
        })}
        <div class="camera-motion">
            <h3 class="camera-head">"Camera motion"</h3>
            {move || (0..draft.with(|d| d.camera_motion.len())).map(row).collect_view()}
            <button
                type="button"
                class="motion-button"
                disabled=move || draft.with(|d| d.camera_motion.len() >= MAX_MOTION_MODIFIERS)
                on:click=move |_| {
                    if let Some(next) = draft.with_untracked(|d| with_added(&d.camera_motion)) {
                        draft.update(|d| d.camera_motion = next);
                    }
                }
            >
                "+ Add camera motion"
            </button>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0176: MIDI is offered on a camera, resting at the stored view, and the list
    /// it makes is one the engine takes.
    #[test]
    fn a_camera_takes_a_midi_move() {
        let fov = camera_kind("MIDI", CameraProperty::Fov, 0.5, &[], None).expect("midi");
        assert!(matches!(
            fov,
            MotionKind::Midi { from, to, controller: Some(1), .. }
                if from.get() == 0.0 && to.get() == -20.0
        ));
        let pan = camera_kind("MIDI", CameraProperty::Pan, 0.5, &[], None).expect("midi");
        assert!(matches!(pan, MotionKind::Midi { to, .. } if to.get() == 45.0));
        let named = with_input(&pan, "Bitwig").expect("named");
        let list = [CameraMotionModifier {
            modifier_id: "pan".into(),
            property: CameraProperty::Pan,
            kind: named,
            clock: MotionClock::Engine,
            enabled: true,
            amount: None,
        }];
        assert_eq!(
            jstream_control::protocol::check_camera_motion(&list),
            Ok(())
        );
        assert!(camera_kind("Audio", CameraProperty::Fov, 0.5, &[], None).is_none());
        assert_eq!(property_name(CameraProperty::Tilt), "Tilt");
        assert_eq!(unit(CameraProperty::Pan), "°");
    }

    /// A camera follows a JTunnel Mod or a channel from the dialog, as the engine takes
    /// them (ADR-0176, ADR-0183), and is never offered audio.
    #[test]
    fn a_camera_takes_a_mod_or_a_channel() {
        assert_eq!(
            camera_kinds(false),
            ["Wiggle", "Oscillate", "Drift", "Shake", "MIDI", "DAW data"]
        );
        assert_eq!(camera_kinds(true).last(), Some(&"Channel"));
        let fov = camera_kind("DAW data", CameraProperty::Fov, 0.5, &[1, 2, 4], None)
            .expect("data");
        assert_eq!(mod_slot(&fov), Some(3));
        assert!(matches!(fov, MotionKind::Data { from, to, .. } if from.get() == 0.0 && to.get() == -20.0));
        let roll = camera_kind("Channel", CameraProperty::Roll, 0.5, &[], Some("cam_roll"))
            .expect("channel");
        assert!(matches!(
            &roll,
            MotionKind::Channel { channel_id, to, .. } if channel_id == "cam_roll" && to.get() == 45.0
        ));
        assert!(camera_kind("Channel", CameraProperty::Roll, 0.5, &[], None).is_none());
        let list: Vec<_> = [(CameraProperty::Fov, fov), (CameraProperty::Roll, roll)]
            .into_iter()
            .map(|(property, kind)| CameraMotionModifier {
                modifier_id: format!("{property:?}"),
                property,
                kind,
                clock: MotionClock::Engine,
                enabled: true,
                amount: None,
            })
            .collect();
        assert_eq!(
            jstream_control::protocol::check_camera_motion(&list),
            Ok(())
        );
        assert_eq!(Param::From.label_for(&list[1].kind), "At 0 %");
        assert_eq!(
            param_value(&list[1].kind, Param::Smoothing),
            Some(50.0)
        );
        let slower = with_param(&list[1].kind, Param::Smoothing, 300.0).expect("smoothing");
        assert_eq!(param_value(&slower, Param::Smoothing), Some(300.0));
    }

    #[test]
    fn adding_starts_a_hand_held_sway_and_stops_when_full() {
        let one = with_added(&[]).expect("room");
        assert_eq!(one[0].modifier_id, "camera-1");
        assert_eq!(one[0].property, CameraProperty::PositionX);
        assert!(matches!(one[0].kind, MotionKind::Wiggle { .. }));
        let full: Vec<_> =
            (0..MAX_MOTION_MODIFIERS).fold(Vec::new(), |list, _| with_added(&list).expect("room"));
        assert_eq!(with_added(&full), None);
        assert_eq!(
            jstream_control::protocol::check_camera_motion(&full),
            Ok(())
        );
    }

    /// ADR-0120: the note names the capture whose filter moves the camera, or says how
    /// to add one.
    #[test]
    fn the_follow_note_names_the_filters_capture() {
        let collection = |follow: serde_json::Value| -> Collection {
            Collection {
                sources: vec![
                    serde_json::from_value(serde_json::json!({
                        "source_id": "screen", "name": "Main screen",
                        "kind": {"type": "builtin", "kind": "display"}
                    }))
                    .expect("a source"),
                ],
                scenes: vec![
                    serde_json::from_value(serde_json::json!({
                        "scene_id": "a", "name": "A",
                        "camera_follow": follow,
                        "items": [{
                            "item_id": "s1", "source_id": "screen",
                            "transform": {"position": {"x": 0.0, "y": 0.0}, "scale": {"x": 1.0, "y": 1.0}, "rotation_deg": 0.0},
                            "crop": {"left": 0, "top": 0, "right": 0, "bottom": 0},
                            "visible": true
                        }],
                    }))
                    .expect("a scene"),
                    serde_json::from_value(serde_json::json!({
                        "scene_id": "b", "name": "B", "items": [],
                    }))
                    .expect("a scene"),
                ],
                ..Collection::default()
            }
        };
        let a = SceneId::new("a");
        let on = collection(serde_json::json!({"item_id": "s1"}));
        assert!(
            follow_note(&on, &a).contains("“Main screen”"),
            "{}",
            follow_note(&on, &a)
        );
        let off = collection(serde_json::Value::Null);
        assert!(follow_note(&off, &a).starts_with("Add the Follow pointer filter"));
        assert!(follow_note(&off, &SceneId::new("b")).starts_with("Add a display"));
    }
}
