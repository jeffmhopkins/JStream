//! The scene Properties dialog's camera moves (ADR-0113): Follow pointer, a camera that
//! glides after the pointer over one of the scene's display or window captures, and
//! Camera motion, the camera's motion modifiers (a wiggle on its position is a hand-held
//! sway). Both are part of the dialog's draft, sent on OK as `scene.set_camera_follow`
//! and `scene.set_camera_motion`, and offered on a scene's own camera only: a keyframe
//! scene draws its parent's. Follow pointer's Zoom in and Zoom out rows bind hotkeys to
//! `scene.zoom_camera` at once, as the Inspector's hotkey rows do.
//!
//! Docs: docs/UI-FEATURES.md#dialogs

use super::inspector::HotkeyRow;
use super::motion::{KINDS, Param, SHAPES, kind_name, param_value, sized_kind, with_param};
use super::number::NumberField;
use super::scene_properties::SceneDraft;
use crate::store::Collection;
use jstream_control::protocol::{
    Action, CameraFollow, CameraMotionModifier, CameraProperty, Finite32, ItemId,
    MAX_MOTION_MODIFIERS, MotionClock, MotionKind, SceneId, SourceKind,
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

/// One number of a follow camera, to show and to set.
#[derive(Clone, Copy)]
enum FollowField {
    Zoom,
    Yaw,
    Pitch,
    Lean,
    Smooth,
}

impl FollowField {
    /// Its label, unit, step, decimals and range.
    fn field(self) -> (&'static str, &'static str, f64, usize, f64, f64) {
        match self {
            FollowField::Zoom => ("Zoom", "×", 0.25, 2, 1.0, 4.0),
            FollowField::Yaw => ("From the right", "°", 1.0, 1, -60.0, 60.0),
            FollowField::Pitch => ("From below", "°", 1.0, 1, -60.0, 60.0),
            FollowField::Lean => ("Lean", "°", 0.5, 1, 0.0, 30.0),
            FollowField::Smooth => ("Smoothing", "ms", 25.0, 0, 50.0, 5000.0),
        }
    }

    fn get(self, follow: &CameraFollow) -> f64 {
        match self {
            FollowField::Zoom => f64::from(follow.zoom.get()),
            FollowField::Yaw => f64::from(follow.yaw_deg.get()),
            FollowField::Pitch => f64::from(follow.pitch_deg.get()),
            FollowField::Lean => f64::from(follow.lean_deg.get()),
            FollowField::Smooth => f64::from(follow.smooth_ms),
        }
    }

    /// Sets it; whether that changed anything.
    fn set(self, follow: &mut CameraFollow, value: f64) -> bool {
        let before = follow.clone();
        let v = Finite32::new(value as f32);
        match (self, v) {
            (FollowField::Zoom, Some(v)) => follow.zoom = v,
            (FollowField::Yaw, Some(v)) => follow.yaw_deg = v,
            (FollowField::Pitch, Some(v)) => follow.pitch_deg = v,
            (FollowField::Lean, Some(v)) => follow.lean_deg = v,
            (FollowField::Smooth, _) => follow.smooth_ms = value.round().clamp(50.0, 5000.0) as u32,
            _ => {}
        }
        *follow != before
    }
}

/// The Follow pointer group and the Camera motion list, editing `draft` for scene
/// `scene_id`, shown while the draft has a camera.
#[component]
pub fn CameraMoves(
    /// The scene the dialog edits.
    scene_id: SceneId,
    /// The dialog's draft.
    draft: RwSignal<SceneDraft>,
) -> impl IntoView {
    let studio = expect_context::<crate::store::Studio>();
    let scene = StoredValue::new(scene_id);
    let items = Memo::new(move |_| {
        studio.collection.with(|c| {
            c.as_ref()
                .map(|c| scene.with_value(|s| followable(c, s)))
                .unwrap_or_default()
        })
    });
    let following = move || draft.with(|d| d.camera_follow.is_some());
    let zoom = move |steps: i32| Action::SceneZoomCamera {
        scene_id: scene.get_value(),
        steps,
    };
    let toggle = move |on: bool| {
        draft.update(|d| {
            d.camera_follow = if on {
                items
                    .with_untracked(|items| items.first().map(|(id, _)| id.clone()))
                    .map(CameraFollow::of)
            } else {
                None
            };
        });
    };
    let follow_number = move |field: FollowField| {
        let (label, unit, step, decimals, min, max) = field.field();
        let value =
            Signal::derive(move || draft.with(|d| d.camera_follow.as_ref().map(|f| field.get(f))));
        let send = move |v: f64| {
            let mut changed = false;
            draft.update(|d| {
                if let Some(follow) = d.camera_follow.as_mut() {
                    changed = field.set(follow, v);
                }
            });
            changed
        };
        view! { <NumberField label unit value step decimals min max send /> }
    };
    let follow_fields = move || {
        following().then(|| {
            view! {
                <div class="camera-row">
                    <label class="select-field">
                        <span class="field-label">"Follows"</span>
                        <select
                            aria-label="The capture it follows"
                            prop:value=move || draft.with(|d| {
                                d.camera_follow.as_ref().map(|f| f.item_id.to_string()).unwrap_or_default()
                            })
                            on:change=move |event| {
                                let id = ItemId::new(event_target_value(&event));
                                draft.update(|d| {
                                    if let Some(follow) = d.camera_follow.as_mut() {
                                        follow.item_id = id;
                                    }
                                });
                            }
                        >
                            {move || items.get().into_iter().map(|(id, name)| {
                                view! { <option value=id.to_string()>{name}</option> }
                            }).collect_view()}
                        </select>
                    </label>
                    {follow_number(FollowField::Zoom)}
                </div>
                <div class="camera-row">
                    {follow_number(FollowField::Yaw)}
                    {follow_number(FollowField::Pitch)}
                    {follow_number(FollowField::Lean)}
                    {follow_number(FollowField::Smooth)}
                </div>
                // Bound at once, as everywhere: the keys zoom the live camera (ADR-0112).
                <HotkeyRow label="Zoom in" action=zoom(1) />
                <HotkeyRow label="Zoom out" action=zoom(-1) />
            }
        })
    };
    let follow_note = move || {
        if items.with(Vec::is_empty) {
            "Add a display or window capture to the scene to follow the pointer over it."
        } else if following() {
            "Glides after the pointer over the capture, with depth of field focused where it looks. The zoom keys change the zoom live, and are saved at once."
        } else {
            "Glides after the pointer over a display or window capture."
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
        <label class="camera-row">
            <input
                type="checkbox"
                aria-label="Follow pointer"
                prop:checked=following
                prop:disabled=move || items.with(Vec::is_empty) && !following()
                on:change=move |event| toggle(event_target_checked(&event))
            />
            <span>"Follow pointer"</span>
        </label>
        {follow_fields}
        <span class="field-note">{follow_note}</span>
        <div class="camera-motion">
            <span class="label">"Camera motion"</span>
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

    #[test]
    fn follow_fields_read_and_write() {
        let mut follow = CameraFollow::of(ItemId::new("screen"));
        for field in [
            FollowField::Zoom,
            FollowField::Yaw,
            FollowField::Pitch,
            FollowField::Lean,
            FollowField::Smooth,
        ] {
            assert!(field.set(&mut follow, 2.0) || field.get(&follow) == 2.0);
        }
        assert_eq!(follow.smooth_ms, 50, "held to its range");
        assert!(!FollowField::Zoom.set(&mut follow, 2.0), "unchanged");
    }
}
