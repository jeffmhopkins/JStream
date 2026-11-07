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

use super::motion::{KINDS, Param, SHAPES, kind_name, param_value, sized_kind, with_param};
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
        // One from a newer engine: shown, but not offered.
        _ => "Other",
    }
}

fn unit(property: CameraProperty) -> &'static str {
    match property {
        CameraProperty::Fov | CameraProperty::Roll => "°",
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
                        let (label, step, decimals, min, max) = param.field();
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
                                    match sized_kind(&name, size) {
                                        Some(kind) => {
                                            m.kind = kind;
                                            true
                                        }
                                        None => false,
                                    }
                                });
                            }
                        >
                            {KINDS
                                .into_iter()
                                // A camera hears nothing: an audio move is an item's.
                                .filter(|k| *k != "Audio")
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
