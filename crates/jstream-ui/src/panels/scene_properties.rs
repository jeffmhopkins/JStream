//! The scene Properties dialog (spec-workspace.md 6.2.5): a scene's name, its
//! Transition in and out, the hotkey that switches to it, whether a multiview shows it,
//! its background colour, whether its audio reaches the Mixer when it is nested, and its
//! 3D camera (ADR-0094), with the camera's Follow pointer note and Camera motion
//! (ADR-0113, ADR-0120, [`super::camera_moves`]). Everything is a draft until OK, which
//! sends what changed as one batch (`scene.rename`, `scene.set_transitions`,
//! `scene.set_meta`, `scene.set_options`, `scene.set_camera`,
//! `scene.set_camera_motion`, `hotkey.clear` and `hotkey.set`); Cancel, Escape and the backdrop
//! drop it, and Defaults clears the overrides, the hotkey and the background and checks
//! both switches again, keeping the name and the camera. A keyframe scene's camera is
//! its override of its parent's, starting from the framing camera when the parent is
//! 2D (ADR-0128). A keyframe scene shows its parent's background, which its dialog
//! shows and doesn't edit. The scene's filters show on the right, each change sent at
//! once, as the source Properties dialog's are.
//!
//! Docs: docs/UI-FEATURES.md#dialogs

use crate::app::SessionContext;
use crate::dialog::Modal;
use crate::hotkeys;
use crate::icons::Icon;
use crate::panels::camera_moves::CameraMoves;
use crate::panels::filters::{Chains, FilterDialogs, scene_owner};
use crate::panels::inspector::transition_options;
use crate::store::{Collection, Studio, apply};
use jstream_control::protocol::{
    Action, CameraMotionModifier, Chord, Edit, FilterOwner, Finite32, SceneCamera, SceneId,
    TransitionPresetId, check_camera_motion, parse_rgba,
};
use leptos::prelude::*;
use leptos::task::spawn_local;

/// The dialog's state as context: the scene it shows while open.
#[derive(Clone, Copy)]
pub struct SceneDialog(RwSignal<Option<SceneId>>);

impl SceneDialog {
    /// A closed dialog.
    pub fn new() -> SceneDialog {
        SceneDialog(RwSignal::new(None))
    }

    /// Opens the dialog on `scene_id`.
    pub fn open(&self, scene_id: SceneId) {
        self.0.set(Some(scene_id));
    }
}

impl Default for SceneDialog {
    fn default() -> SceneDialog {
        SceneDialog::new()
    }
}

/// What the dialog edits.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SceneDraft {
    /// The display name, as typed.
    pub name: String,
    /// The preset for switching into the scene, or none.
    pub transition_in: Option<TransitionPresetId>,
    /// The preset for switching out of it, or none.
    pub transition_out: Option<TransitionPresetId>,
    /// The chord that switches to it, or none.
    pub hotkey: Option<Chord>,
    /// Whether a multiview shows it.
    pub show_in_multiview: bool,
    /// Whether its audio reaches the Mixer when it is nested (ADR-0089).
    pub include_audio: bool,
    /// Its background colour as typed (`#rrggbb` or `#rrggbbaa`), or none.
    pub background: Option<String>,
    /// Its camera, which makes it a 3D scene (ADR-0094); on a keyframe scene its
    /// override of its parent's.
    pub camera: Option<SceneCamera>,
    /// Its camera's motion modifiers (ADR-0113); a keyframe scene draws its parent's.
    pub camera_motion: Vec<CameraMotionModifier>,
}

impl SceneDraft {
    /// The scene as the collection has it, or `None` when it has no such scene.
    pub fn of(collection: &Collection, scene_id: &SceneId) -> Option<SceneDraft> {
        let scene = collection.scene(scene_id)?;
        Some(SceneDraft {
            name: scene.name.clone(),
            transition_in: scene.transition_in.clone(),
            transition_out: scene.transition_out.clone(),
            hotkey: collection.scene_hotkey(scene_id).map(|b| b.chord.clone()),
            show_in_multiview: scene.meta.show_in_multiview,
            include_audio: scene.options.include_audio,
            background: scene.options.background.clone(),
            camera: scene.camera,
            camera_motion: scene.camera_motion.clone(),
        })
    }
}

/// The camera the dialog's switch starts `scene_id` from when it is turned on (ADR-0094
/// §6): a keyframe scene's parent's, which its own then overrides; otherwise, and for a
/// keyframe of a 2D scene (ADR-0128), one that shows it as it is now.
pub fn camera_start(collection: &Collection, scene_id: &SceneId) -> Option<SceneCamera> {
    let framing = SceneCamera::framing(collection.canvas);
    match collection.scene(scene_id)?.variant_of {
        Some(_) => Some(collection.parent_of(scene_id)?.camera.unwrap_or(framing)),
        None => Some(framing),
    }
}

/// The edits that turn `before` into `draft` for `scene_id`, in one batch: a rename
/// (a blank name is ignored), the overrides, the multiview flag (the scene's other
/// metadata kept), the options, the camera, then the hotkey (the old chord cleared
/// before the new one is bound, keeping the preset the old binding named). Refused when
/// the background isn't a colour, the camera is one the engine can't see through, or
/// the new chord is bound already, which `scene.set_options`, `scene.set_camera` and
/// `hotkey.set` would refuse too.
pub fn scene_edits(
    collection: &Collection,
    scene_id: &SceneId,
    before: &SceneDraft,
    draft: &SceneDraft,
) -> Result<Vec<Edit>, String> {
    let mut edits = Vec::new();
    let name = draft.name.trim();
    if !name.is_empty() && name != before.name {
        edits.push(Edit::SceneRename {
            scene_id: scene_id.clone(),
            name: name.to_owned(),
        });
    }
    if (&draft.transition_in, &draft.transition_out)
        != (&before.transition_in, &before.transition_out)
    {
        edits.push(Edit::SceneSetTransitions {
            scene_id: scene_id.clone(),
            transition_in: draft.transition_in.clone(),
            transition_out: draft.transition_out.clone(),
        });
    }
    if draft.show_in_multiview != before.show_in_multiview {
        let mut meta = collection
            .scene(scene_id)
            .map(|s| s.meta.clone())
            .unwrap_or_default();
        meta.show_in_multiview = draft.show_in_multiview;
        edits.push(Edit::SceneSetMeta {
            scene_id: scene_id.clone(),
            meta,
        });
    }
    let background = draft
        .background
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty());
    if background.is_some_and(|b| parse_rgba(b).is_none()) {
        return Err("The background color must be #rrggbb or #rrggbbaa.".into());
    }
    if (draft.include_audio, background) != (before.include_audio, before.background.as_deref()) {
        let mut options = collection
            .scene(scene_id)
            .map(|s| s.options.clone())
            .unwrap_or_default();
        options.include_audio = draft.include_audio;
        options.background = background.map(str::to_owned);
        edits.push(Edit::SceneSetOptions {
            scene_id: scene_id.clone(),
            options,
        });
    }
    if draft.camera != before.camera {
        if let Some(camera) = draft.camera
            && let Err(error) = camera.validate()
        {
            return Err(format!("The camera can't be used: {error}."));
        }
        edits.push(Edit::SceneSetCamera {
            scene_id: scene_id.clone(),
            camera: draft.camera,
        });
    }
    if draft.camera_motion != before.camera_motion {
        if let Err(problem) = check_camera_motion(&draft.camera_motion) {
            return Err(format!(
                "Camera motion: {} {}.",
                problem.field, problem.reason
            ));
        }
        edits.push(Edit::SceneSetCameraMotion {
            scene_id: scene_id.clone(),
            motion: draft.camera_motion.clone(),
        });
    }
    if draft.hotkey != before.hotkey {
        if let Some(chord) = &draft.hotkey
            && let Some(taken) = collection.hotkeys.iter().find(|b| &b.chord == chord)
        {
            let runs = match taken.action() {
                Some(Action::SceneSwitch { scene_id: id, .. }) if &id == scene_id => {
                    "already switches to this scene".to_owned()
                }
                Some(Action::SceneSwitch { scene_id: id, .. }) => format!(
                    "already switches to “{}”",
                    collection
                        .scene(&id)
                        .map_or_else(|| id.to_string(), |s| s.name.clone())
                ),
                _ => "already runs something else".to_owned(),
            };
            return Err(format!(
                "{} {runs}. Clear that hotkey first.",
                hotkeys::label(chord)
            ));
        }
        // A rebind keeps the preset the old binding switched with.
        let preset_id = collection
            .scene_hotkey(scene_id)
            .and_then(|b| match b.action() {
                Some(Action::SceneSwitch { preset_id, .. }) => preset_id,
                _ => None,
            });
        if let Some(chord) = before.hotkey.clone() {
            edits.push(Edit::HotkeyClear { chord });
        }
        if let Some(chord) = draft.hotkey.clone() {
            edits.push(Edit::HotkeySet {
                chord,
                action: Action::SceneSwitch {
                    scene_id: scene_id.clone(),
                    preset_id,
                },
            });
        }
    }
    Ok(edits)
}

/// The dialog, drawn while it is open. A scene that goes away closes it.
#[component]
pub fn SceneProperties() -> impl IntoView {
    let dialog = expect_context::<SceneDialog>();
    let studio = expect_context::<Studio>();
    let close = move || dialog.0.set(None);
    move || {
        dialog.0.get().map(|scene_id| {
            let present = {
                let scene_id = scene_id.clone();
                Memo::new(move |_| {
                    studio
                        .collection
                        .with(|c| c.as_ref().is_some_and(|c| c.scene(&scene_id).is_some()))
                })
            };
            move || {
                if present.get() {
                    Some(view! { <SceneBody scene_id=scene_id.clone() close=close /> })
                } else {
                    request_animation_frame(close);
                    None
                }
            }
        })
    }
}

#[component]
fn SceneBody(scene_id: SceneId, close: impl Fn() + Copy + Send + Sync + 'static) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let Some(opened) = studio
        .collection
        .with_untracked(|c| c.as_ref().and_then(|c| SceneDraft::of(c, &scene_id)))
    else {
        request_animation_frame(close);
        return ().into_any();
    };
    let subtitle = opened.name.clone();
    let (owner, parent) = studio.collection.with_untracked(|c| {
        c.as_ref().map_or_else(
            || {
                (
                    FilterOwner::Scene {
                        scene_id: scene_id.clone(),
                    },
                    None,
                )
            },
            |c| {
                (
                    scene_owner(c, &scene_id),
                    c.parent_of(&scene_id).map(|p| p.name.clone()),
                )
            },
        )
    });
    let before = StoredValue::new(opened.clone());
    let draft = RwSignal::new(opened);
    // A keyframe scene shows its parent's background (ADR-0089 §4).
    let keyframe_of = studio.collection.with_untracked(|c| {
        c.as_ref()
            .and_then(|c| c.parent_of(&scene_id))
            .map(|p| (p.name.clone(), p.options.background.clone()))
    });
    let background_note = match &keyframe_of {
        Some((parent, _)) => format!("A keyframe scene shows the background of “{parent}”."),
        None => "Drawn under its items, nested or not. None is transparent.".to_owned(),
    };
    let keyframe = keyframe_of.is_some();
    let shown_background = {
        let parents = keyframe_of.as_ref().map(|(_, b)| b.clone());
        move || match &parents {
            Some(parents) => parents.clone().unwrap_or_default(),
            None => draft.with(|d| d.background.clone().unwrap_or_default()),
        }
    };
    let swatch = {
        let shown = shown_background.clone();
        move || background_swatch(&shown())
    };
    // The camera (ADR-0094). A keyframe scene's is an override of its parent's, which
    // it follows while it has none of its own; with no parent camera there is none to
    // override.
    let (canvas, parent_camera, items_3d) = studio.collection.with_untracked(|c| {
        c.as_ref().map_or((Default::default(), None, false), |c| {
            (
                c.canvas,
                c.parent_of(&scene_id).and_then(|p| p.camera),
                has_3d_items(c, &scene_id),
            )
        })
    });
    let camera_label = if keyframe { "Own camera" } else { "3D camera" };
    let start = studio
        .collection
        .with_untracked(|c| c.as_ref().map(|c| camera_start(c, &scene_id)));
    let camera_locked = start.is_some_and(|s| s.is_none());
    let parent_name = keyframe_of.as_ref().map(|(parent, _)| parent.clone());
    let camera_note = move || {
        let on = draft.with(|d| d.camera.is_some());
        match (&parent_name, parent_camera, on) {
            (Some(parent), None, false) => {
                format!("Off, it is 2D like “{parent}”. On, a Move into it glides into 3D.")
            }
            (Some(parent), Some(_), false) => {
                format!("Off, it follows the camera of “{parent}”. On, it moves the camera when you switch to it.")
            }
            (_, _, true) if items_3d => "Items with depth or angles already look different through the camera.".to_owned(),
            (_, _, true) => "Items sit in one 3D space. Alt-drag on the canvas to orbit, Alt+Shift-drag to dolly.".to_owned(),
            (None, _, false) => "Starts from a camera that shows the scene as it is now.".to_owned(),
        }
    };
    // The distance that puts the selected item in focus, through the draft's camera.
    let selection = studio.selection;
    let focus_scene = StoredValue::new(scene_id.clone());
    let focus_target = move || {
        let camera = draft.with(|d| d.camera)?;
        studio.collection.with(|c| {
            selection
                .with(|s| focus_scene.with_value(|scene| focus_on(c.as_ref()?, scene, s, camera)))
        })
    };
    let toggle_camera = move |on: bool| {
        draft.update(|d| {
            d.camera = on.then(|| {
                start
                    .flatten()
                    .unwrap_or_else(|| SceneCamera::framing(canvas))
            });
        });
    };
    // `caption` is the short axis mark beside a triplet field; empty when the row
    // names the control. `label` is the input's accessible name.
    let number = move |caption: &'static str,
                       label: &'static str,
                       field: CameraField,
                       step: &'static str| {
        view! {
            <label class="camera-number">
                {(!caption.is_empty()).then(|| view! { <span class="label">{caption}</span> })}
                <input
                    type="number"
                    step=step
                    aria-label=label
                    prop:value=move || draft.with(|d| d.camera.map(|c| {
                        let v = field.get(&c);
                        format!("{}", (v * 100.0).round() / 100.0 + 0.0)
                    }).unwrap_or_default())
                    on:change=move |event| {
                        if let Ok(value) = event_target_value(&event).trim().parse::<f32>() {
                            draft.update(|d| {
                                if let Some(camera) = d.camera.as_mut() {
                                    field.set(camera, value);
                                }
                            });
                        }
                    }
                />
            </label>
        }
    };
    let camera_fields = move || {
        draft.with(|d| d.camera.is_some()).then(|| {
            view! {
                <div class="camera-fields">
                    <h3 class="camera-head">"Camera"</h3>
                    <div class="camera-row">
                        <span class="label">"Position"</span>
                        <div class="camera-axes">
                            {number("X", "Position X", CameraField::Position(0), "1")}
                            {number("Y", "Position Y", CameraField::Position(1), "1")}
                            {number("Z", "Position Z", CameraField::Position(2), "1")}
                        </div>
                    </div>
                    <div class="camera-row">
                        <span class="label">"Looks at"</span>
                        <div class="camera-axes">
                            {number("X", "Target X", CameraField::Target(0), "1")}
                            {number("Y", "Target Y", CameraField::Target(1), "1")}
                            {number("Z", "Target Z", CameraField::Target(2), "1")}
                        </div>
                    </div>
                    <div class="camera-row">
                        <span class="label">"Field of view"</span>
                        <div class="camera-control">
                            {number("", "Field of view", CameraField::Fov, "0.5")}
                            <span class="unit">"°"</span>
                        </div>
                    </div>
                    <div class="camera-row">
                        <span class="label">"Roll"</span>
                        <div class="camera-control">
                            {number("", "Roll", CameraField::Roll, "0.5")}
                            <span class="unit">"°"</span>
                        </div>
                    </div>
                    <h3 class="camera-head">"Rendering"</h3>
                    <div class="camera-line">
                        <label class="camera-check">
                            <input
                                type="checkbox"
                                aria-label="Nearest on top"
                                prop:checked=move || draft.with(|d| d.camera.is_some_and(|c| c.depth_sort))
                                on:change=move |event| {
                                    let sorted = event_target_checked(&event);
                                    draft.update(|d| {
                                        if let Some(camera) = d.camera.as_mut() {
                                            camera.depth_sort = sorted;
                                        }
                                    });
                                }
                            />
                            <span>"Nearest on top"</span>
                        </label>
                        <span class="camera-hint">"off: stacking order decides"</span>
                    </div>
                    <label class="camera-check">
                        <input
                            type="checkbox"
                            aria-label="Depth of field"
                            prop:checked=move || draft.with(|d| d.camera.is_some_and(|c| c.focus_px.is_some()))
                            on:change=move |event| {
                                let on = event_target_checked(&event);
                                draft.update(|d| {
                                    if let Some(camera) = d.camera.as_mut() {
                                        depth_of_field(camera, on);
                                    }
                                });
                            }
                        />
                        <span>"Depth of field"</span>
                    </label>
                    {move || draft.with(|d| d.camera.is_some_and(|c| c.focus_px.is_some())).then(|| view! {
                        <div class="camera-sub">
                            <div class="camera-row">
                                <span class="label">"Focus distance"</span>
                                {number("", "Focus distance", CameraField::Focus, "1")}
                            </div>
                            <div class="camera-row">
                                <span class="label">"Blur"</span>
                                <div class="camera-control">
                                    {number("", "Blur", CameraField::Blur, "0.5")}
                                    <span class="unit">"px"</span>
                                </div>
                            </div>
                            <button
                                type="button"
                                class="btn"
                                title="Focus on the selected item"
                                prop:disabled=move || focus_target().is_none()
                                on:click=move |_| {
                                    if let Some(distance) = focus_target() {
                                        draft.update(|d| {
                                            if let Some(camera) = d.camera.as_mut() {
                                                CameraField::Focus.set(camera, distance);
                                            }
                                        });
                                    }
                                }
                            >
                                "Focus on selection"
                            </button>
                        </div>
                    })}
                </div>
            }
        })
    };
    let problem = RwSignal::new(None::<String>);
    let id = StoredValue::new(scene_id);

    let options = move || {
        studio.collection.with(|c| {
            c.as_ref()
                .map(|c| transition_options(&c.transitions))
                .unwrap_or_default()
        })
    };
    let select = move |into: bool, label: &'static str, note: &'static str| {
        let current = move || {
            draft
                .with(|d| {
                    if into {
                        &d.transition_in
                    } else {
                        &d.transition_out
                    }
                    .clone()
                })
                .map_or_else(String::new, |p| p.to_string())
        };
        let set = move |value: String| {
            let chosen = (!value.is_empty()).then(|| TransitionPresetId::new(value));
            draft.update(|d| {
                if into {
                    d.transition_in = chosen;
                } else {
                    d.transition_out = chosen;
                }
            });
        };
        view! {
            <label class="field">
                <span class="label">{label}</span>
                <select aria-label=label on:change=move |event| set(event_target_value(&event))>
                    {move || {
                        let now = current();
                        options()
                            .into_iter()
                            .map(|(value, name)| {
                                let selected = value == now;
                                view! { <option value=value selected=selected>{name}</option> }
                            })
                            .collect_view()
                    }}
                </select>
                <span class="field-note">{note}</span>
            </label>
        }
    };
    let hotkey_text =
        move || draft.with(|d| d.hotkey.as_ref().map(hotkeys::label).unwrap_or_default());
    let on_hotkey = move |event: leptos::ev::KeyboardEvent| {
        let code = event.code();
        let plain = !(event.ctrl_key() || event.alt_key() || event.shift_key() || event.meta_key());
        // Tab moves on, Escape cancels and Enter presses OK, as everywhere in a dialog.
        if plain && matches!(code.as_str(), "Tab" | "Escape" | "Enter" | "NumpadEnter") {
            return;
        }
        event.prevent_default();
        event.stop_propagation();
        if plain && matches!(code.as_str(), "Backspace" | "Delete") {
            draft.update(|d| d.hotkey = None);
            return;
        }
        if let Some(chord) = hotkeys::of_event(&event) {
            draft.update(|d| d.hotkey = Some(chord));
        }
    };
    let defaults = move |_| {
        draft.update(|d| {
            d.transition_in = None;
            d.transition_out = None;
            d.hotkey = None;
            d.show_in_multiview = true;
            d.include_audio = true;
            if keyframe_of.is_none() {
                d.background = None;
            }
        });
    };
    let ok = move |_| {
        let Some(s) = session.get() else { return };
        let built = studio.collection.with_untracked(|c| {
            let c = c.as_ref()?;
            Some(scene_edits(
                c,
                &id.get_value(),
                &before.get_value(),
                &draft.get_untracked(),
            ))
        });
        let edits = match built {
            None => {
                close();
                return;
            }
            Some(Err(why)) => {
                problem.set(Some(why));
                return;
            }
            Some(Ok(edits)) => edits,
        };
        if edits.is_empty() {
            close();
            return;
        }
        spawn_local(async move {
            match apply(&s, edits, None).await {
                Ok(_) => close(),
                Err(error) => _ = problem.try_set(Some(error.to_string())),
            }
        });
    };

    view! {
        <Modal label="Scene properties" class="props-dialog scene-props-dialog" cancel=close>
            <div class="modal-header">
                <div class="props-head">
                    <Icon name="layers" size=20 />
                    <div class="modal-titles">
                        <h2 class="modal-title">"Scene properties"</h2>
                        <span class="modal-sub">{subtitle}</span>
                    </div>
                </div>
                <button type="button" class="modal-close" aria-label="Close" on:click=move |_| close()>
                    "×"
                </button>
            </div>
            <div class="props-body">
            <div class="props-settings">
                <label class="field props-name">
                    <span class="label">"Name"</span>
                    <input
                        type="text"
                        prop:value=move || draft.with(|d| d.name.clone())
                        on:input=move |event| {
                            let value = event_target_value(&event);
                            draft.update(|d| d.name = value);
                        }
                    />
                </label>
                {select(true, "Transition in", "Used when switching into this scene.")}
                {select(false, "Transition out", "Used when leaving it, unless the next scene has its own “in”.")}
                <label class="field">
                    <span class="label">"Hotkey"</span>
                    <input
                        type="text"
                        class="hotkey-field"
                        readonly=true
                        placeholder="None"
                        prop:value=hotkey_text
                        on:keydown=on_hotkey
                    />
                    <span class="field-note">"Press the keys that switch to this scene. Backspace clears it."</span>
                </label>
                <label class="field">
                    <span class="label">"Show in multiview"</span>
                    <input
                        type="checkbox"
                        aria-label="Show in multiview"
                        prop:checked=move || draft.with(|d| d.show_in_multiview)
                        on:change=move |event| {
                            let shown = event_target_checked(&event);
                            draft.update(|d| d.show_in_multiview = shown);
                        }
                    />
                    <span class="field-note">"A multiview window shows it as one of its tiles."</span>
                </label>
                <div class="field">
                    <span class="label">"Background color"</span>
                    <div class="control">
                        <input
                            type="color"
                            aria-label="Background color swatch"
                            disabled=keyframe
                            prop:value=swatch
                            on:change=move |event| {
                                let picked = event_target_value(&event);
                                draft.update(|d| {
                                    d.background = Some(with_swatch(d.background.as_deref(), &picked));
                                });
                            }
                        />
                        <input
                            type="text"
                            class="color-text"
                            aria-label="Background color"
                            placeholder="None"
                            disabled=keyframe
                            prop:value=shown_background
                            on:change=move |event| {
                                let typed = event_target_value(&event);
                                let typed = typed.trim();
                                draft.update(|d| {
                                    d.background = (!typed.is_empty()).then(|| typed.to_owned());
                                });
                            }
                        />
                    </div>
                    <span class="field-note">{background_note}</span>
                </div>
                <label class="field">
                    <span class="label">"Include its audio when nested"</span>
                    <input
                        type="checkbox"
                        aria-label="Include its audio when nested"
                        prop:checked=move || draft.with(|d| d.include_audio)
                        on:change=move |event| {
                            let included = event_target_checked(&event);
                            draft.update(|d| d.include_audio = included);
                        }
                    />
                    <span class="field-note">"When off, its sources don't reach the Mixer through this scene."</span>
                </label>
                <div class="camera-field">
                    <label class="camera-check">
                        <input
                            type="checkbox"
                            aria-label=camera_label
                            disabled=camera_locked
                            prop:checked=move || draft.with(|d| d.camera.is_some())
                            on:change=move |event| toggle_camera(event_target_checked(&event))
                        />
                        <span>{camera_label}</span>
                    </label>
                    <span class="field-note">{camera_note}</span>
                    {camera_fields}
                    {move || (!keyframe && draft.with(|d| d.camera.is_some())).then(|| view! {
                        <CameraMoves scene_id=id.get_value() draft=draft />
                    })}
                </div>
                {move || problem.get().map(|e| view! { <p class="problem" role="alert">{e}</p> })}
            </div>
            <SceneFilterSide owner=owner parent=parent />
            </div>
            <div class="modal-footer">
                <button type="button" class="secondary props-defaults" on:click=defaults>"Defaults"</button>
                <button type="button" class="secondary" on:click=move |_| close()>"Cancel"</button>
                <button type="button" class="primary" data-primary on:click=ok>"OK"</button>
            </div>
        </Modal>
    }
    .into_any()
}

/// Whether any of `scene`'s items already has 3D (depth, tilt, turn or its own
/// perspective), which looks different once the scene has a camera (ADR-0094).
fn has_3d_items(collection: &Collection, scene_id: &SceneId) -> bool {
    let scene = collection
        .parent_of(scene_id)
        .or_else(|| collection.scene(scene_id));
    scene.is_some_and(|scene| {
        scene
            .items
            .iter()
            .filter_map(|item| item.geometry)
            .any(|g| {
                g.depth_px.get() != 0.0
                    || g.tilt_deg.get() != 0.0
                    || g.turn_deg.get() != 0.0
                    || g.perspective_px.is_some()
            })
    })
}

/// Turns `camera`'s depth of field on, focused on its target with a blur of
/// [`DEFAULT_BLUR_PX`] unless it has one, or off (ADR-0108 decision 7).
fn depth_of_field(camera: &mut SceneCamera, on: bool) {
    if on {
        camera.focus_px = Finite32::new(camera.target_distance());
        if camera.blur_px.get() == 0.0 {
            camera.blur_px = Finite32::new(DEFAULT_BLUR_PX).unwrap_or(Finite32::ZERO);
        }
    } else {
        camera.focus_px = None;
        camera.blur_px = Finite32::ZERO;
    }
}

/// The blur Depth of field starts with, in canvas pixels.
const DEFAULT_BLUR_PX: f32 = 6.0;

/// The focus distance that puts the selected item of `scene_id` in focus through
/// `camera`: its box centre's distance along the view, or `None` when no item of the
/// scene is selected, it has no geometry, or it isn't in front of the camera.
fn focus_on(
    collection: &Collection,
    scene_id: &SceneId,
    selection: &crate::store::Selection,
    camera: SceneCamera,
) -> Option<f32> {
    if selection.scene.as_ref() != Some(scene_id) {
        return None;
    }
    let (_, item) = collection.item_in(Some(scene_id), selection.item.as_ref()?)?;
    let view = camera.view(collection.canvas).ok()?;
    let distance = view.depth_of(item.geometry?) as f32;
    (distance > 0.0 && distance.is_finite()).then_some(distance)
}

/// One number of a camera, to show and to set.
#[derive(Clone, Copy)]
enum CameraField {
    Position(usize),
    Target(usize),
    Fov,
    Roll,
    Focus,
    Blur,
}

impl CameraField {
    fn get(self, camera: &SceneCamera) -> f32 {
        let axis = |v: &jstream_control::protocol::Vec3, i: usize| match i {
            0 => v.x.get(),
            1 => v.y.get(),
            _ => v.z.get(),
        };
        match self {
            CameraField::Position(i) => axis(&camera.position, i),
            CameraField::Target(i) => axis(&camera.target, i),
            CameraField::Fov => camera.fov_deg.get(),
            CameraField::Roll => camera.roll_deg.get(),
            CameraField::Focus => camera.focus_px.map_or(0.0, Finite32::get),
            CameraField::Blur => camera.blur_px.get(),
        }
    }

    /// `camera` with this number set to `value`, or unchanged when it isn't finite.
    fn set(self, camera: &mut SceneCamera, value: f32) {
        let Some(value) = Finite32::new(value) else {
            return;
        };
        let axis = |v: &mut jstream_control::protocol::Vec3, i: usize| match i {
            0 => v.x = value,
            1 => v.y = value,
            _ => v.z = value,
        };
        match self {
            CameraField::Position(i) => axis(&mut camera.position, i),
            CameraField::Target(i) => axis(&mut camera.target, i),
            CameraField::Fov => camera.fov_deg = value,
            CameraField::Roll => camera.roll_deg = value,
            CameraField::Focus => camera.focus_px = Some(value),
            CameraField::Blur => camera.blur_px = value,
        }
    }
}

/// The `#rrggbb` a colour swatch shows for `background`: its colour without alpha, or
/// black for none or for text that isn't a colour.
fn background_swatch(background: &str) -> String {
    parse_rgba(background.trim()).map_or_else(
        || "#000000".to_owned(),
        |[r, g, b, _]| format!("#{r:02x}{g:02x}{b:02x}"),
    )
}

/// `background` with its colour replaced by the swatch's `picked` `#rrggbb`, keeping an
/// alpha it had.
fn with_swatch(background: Option<&str>, picked: &str) -> String {
    match background.map(str::trim).and_then(parse_rgba) {
        Some([.., a]) if a != 0xff => format!("{picked}{a:02x}"),
        _ => picked.to_owned(),
    }
}

/// The scene's video chain (FL-06), each change sent at once, and the Add filter button.
/// A keyframe scene shows its parent's chain, which it has no other of (ADR-0065 §3).
#[component]
fn SceneFilterSide(owner: FilterOwner, parent: Option<String>) -> impl IntoView {
    let dialogs = expect_context::<FilterDialogs>();
    let add = {
        let owner = owner.clone();
        move |_| dialogs.add(owner.clone())
    };
    let note = match parent {
        Some(parent) => format!("those of “{parent}”, which this keyframe follows"),
        None => "the whole scene".to_owned(),
    };
    view! {
        <div class="props-side">
            <div class="props-side-head">
                <span class="props-side-title">"Filters"</span>
                <span class="props-side-note">{note}</span>
            </div>
            <Chains owner=owner video=true audio=false />
            <p class="props-side-foot">"Changes apply right away. Fine-tune settings in the Filters panel."</p>
            <button type="button" class="primary props-add-filter" on:click=add>"+ Add filter…"</button>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{HotkeyBinding, SceneInfo, SceneOptions};

    fn switch(id: &str) -> Action {
        Action::SceneSwitch {
            scene_id: SceneId::new(id),
            preset_id: None,
        }
    }

    fn draft(name: &str, hotkey: Option<&str>) -> SceneDraft {
        SceneDraft {
            name: name.into(),
            hotkey: hotkey.map(Chord::new),
            ..SceneDraft::default()
        }
    }

    #[test]
    fn only_what_changed_is_sent() {
        let c = Collection::default();
        let a = SceneId::new("a");
        let before = draft("Main", None);
        assert_eq!(scene_edits(&c, &a, &before, &before), Ok(vec![]));
        // A blank name is ignored.
        assert_eq!(scene_edits(&c, &a, &before, &draft("  ", None)), Ok(vec![]));

        let mut next = draft(" Intro ", None);
        next.transition_in = Some(TransitionPresetId::new("fade"));
        assert_eq!(
            scene_edits(&c, &a, &before, &next),
            Ok(vec![
                Edit::SceneRename {
                    scene_id: a.clone(),
                    name: "Intro".into()
                },
                Edit::SceneSetTransitions {
                    scene_id: a,
                    transition_in: Some(TransitionPresetId::new("fade")),
                    transition_out: None,
                },
            ])
        );
    }

    #[test]
    fn a_new_hotkey_replaces_the_old_and_a_taken_one_is_refused() {
        let c = Collection {
            hotkeys: vec![
                HotkeyBinding::new(Chord::new("F1"), &switch("a")),
                HotkeyBinding::new(Chord::new("F2"), &switch("b")),
            ],
            ..Collection::default()
        };
        let a = SceneId::new("a");
        let before = draft("A", Some("F1"));
        assert_eq!(
            scene_edits(&c, &a, &before, &draft("A", Some("F3"))),
            Ok(vec![
                Edit::HotkeyClear {
                    chord: Chord::new("F1")
                },
                Edit::HotkeySet {
                    chord: Chord::new("F3"),
                    action: switch("a"),
                },
            ])
        );
        assert_eq!(
            scene_edits(&c, &a, &before, &draft("A", None)),
            Ok(vec![Edit::HotkeyClear {
                chord: Chord::new("F1")
            }])
        );
        assert_eq!(
            scene_edits(&c, &a, &before, &draft("A", Some("F2"))),
            Err("F2 already switches to “b”. Clear that hotkey first.".into())
        );
    }

    /// The multiview switch sends the scene's metadata with only that flag changed.
    #[test]
    fn the_multiview_switch_keeps_the_other_metadata() {
        let mut scene: SceneInfo = serde_json::from_value(serde_json::json!({
            "scene_id": "a",
            "name": "A",
            "items": [],
        }))
        .expect("a scene");
        scene.meta.favourite = true;
        scene.meta.color = Some("#ff0000".into());
        let c = Collection {
            scenes: vec![scene],
            ..Collection::default()
        };
        let a = SceneId::new("a");
        let before = SceneDraft::of(&c, &a).expect("the scene");
        assert!(before.show_in_multiview);
        let hidden = SceneDraft {
            show_in_multiview: false,
            ..before.clone()
        };
        let Ok(edits) = scene_edits(&c, &a, &before, &hidden) else {
            panic!("edits");
        };
        let [Edit::SceneSetMeta { scene_id, meta }] = edits.as_slice() else {
            panic!("one set_meta, not {edits:?}");
        };
        assert_eq!(scene_id, &a);
        assert!(!meta.show_in_multiview && meta.favourite);
        assert_eq!(meta.color.as_deref(), Some("#ff0000"));
    }

    /// The nested audio switch sends `scene.set_options`.
    #[test]
    fn the_audio_switch_sends_the_scene_s_options() {
        let mut scene: SceneInfo = serde_json::from_value(serde_json::json!({
            "scene_id": "a",
            "name": "A",
            "items": [],
        }))
        .expect("a scene");
        scene.meta.favourite = true;
        let c = Collection {
            scenes: vec![scene],
            ..Collection::default()
        };
        let a = SceneId::new("a");
        let before = SceneDraft::of(&c, &a).expect("the scene");
        assert!(before.include_audio);
        let left_out = SceneDraft {
            include_audio: false,
            ..before.clone()
        };
        assert_eq!(
            scene_edits(&c, &a, &before, &left_out),
            Ok(vec![Edit::SceneSetOptions {
                scene_id: a,
                options: SceneOptions {
                    include_audio: false,
                    background: None,
                },
            }])
        );
    }

    /// The background colour is one of the options; it keeps the switch, a blank one is
    /// none, and one that isn't a colour is refused before anything is sent.
    #[test]
    fn the_background_colour_sends_the_scene_s_options() {
        let mut scene: SceneInfo = serde_json::from_value(serde_json::json!({
            "scene_id": "a",
            "name": "A",
            "include_audio": false,
            "items": [],
        }))
        .expect("a scene");
        scene.options.background = Some("#112233".into());
        let c = Collection {
            scenes: vec![scene],
            ..Collection::default()
        };
        let a = SceneId::new("a");
        let before = SceneDraft::of(&c, &a).expect("the scene");
        let with = |background: &str| SceneDraft {
            background: Some(background.into()),
            ..before.clone()
        };
        assert_eq!(
            scene_edits(&c, &a, &before, &with(" #0F111580 ")),
            Ok(vec![Edit::SceneSetOptions {
                scene_id: a.clone(),
                options: SceneOptions {
                    include_audio: false,
                    background: Some("#0F111580".into()),
                },
            }])
        );
        assert_eq!(
            scene_edits(&c, &a, &before, &with("  ")),
            Ok(vec![Edit::SceneSetOptions {
                scene_id: a.clone(),
                options: SceneOptions {
                    include_audio: false,
                    background: None,
                },
            }])
        );
        assert_eq!(scene_edits(&c, &a, &before, &with("#112233")), Ok(vec![]));
        assert!(scene_edits(&c, &a, &before, &with("blue")).is_err());
    }

    /// ADR-0094: turning the camera on, changing it and turning it off each send one
    /// `scene.set_camera`; a camera the engine would refuse is refused before sending.
    #[test]
    fn the_camera_sends_scene_set_camera() {
        let scene: SceneInfo = serde_json::from_value(serde_json::json!({
            "scene_id": "a",
            "name": "A",
            "items": [],
        }))
        .expect("a scene");
        let c = Collection {
            scenes: vec![scene],
            ..Collection::default()
        };
        let a = SceneId::new("a");
        let before = SceneDraft::of(&c, &a).expect("the scene");
        assert_eq!(before.camera, None);
        let framing = SceneCamera::framing(jstream_control::protocol::Canvas {
            width: 1920,
            height: 1080,
        });
        let on = SceneDraft {
            camera: Some(framing),
            ..before.clone()
        };
        assert_eq!(
            scene_edits(&c, &a, &before, &on),
            Ok(vec![Edit::SceneSetCamera {
                scene_id: a.clone(),
                camera: Some(framing),
            }])
        );
        assert_eq!(
            scene_edits(&c, &a, &on, &before),
            Ok(vec![Edit::SceneSetCamera {
                scene_id: a.clone(),
                camera: None,
            }])
        );
        let mut wide = framing;
        CameraField::Fov.set(&mut wide, 200.0);
        assert_eq!(CameraField::Fov.get(&wide), 200.0);
        let refused = SceneDraft {
            camera: Some(wide),
            ..before.clone()
        };
        assert!(scene_edits(&c, &a, &before, &refused).is_err());
        let mut moved = framing;
        CameraField::Position(2).set(&mut moved, 900.0);
        CameraField::Target(0).set(&mut moved, f32::NAN);
        assert_eq!(CameraField::Position(2).get(&moved), 900.0);
        assert_eq!(
            CameraField::Target(0).get(&moved),
            960.0,
            "a non-finite number is ignored"
        );
    }

    /// ADR-0113: Camera motion sends `scene.set_camera_motion` when it changes. Follow
    /// pointer is a filter since ADR-0120, so the dialog sends no follow.
    #[test]
    fn camera_motion_sends_its_edit() {
        let scene: SceneInfo = serde_json::from_value(serde_json::json!({
            "scene_id": "a",
            "name": "A",
            "items": [],
        }))
        .expect("a scene");
        let c = Collection {
            scenes: vec![scene],
            ..Collection::default()
        };
        let a = SceneId::new("a");
        let before = SceneDraft::of(&c, &a).expect("the scene");
        let motion = crate::panels::camera_moves::with_added(&[]).expect("room");
        let after = SceneDraft {
            camera_motion: motion.clone(),
            ..before.clone()
        };
        assert_eq!(
            scene_edits(&c, &a, &before, &after),
            Ok(vec![Edit::SceneSetCameraMotion {
                scene_id: a.clone(),
                motion,
            }])
        );
    }

    /// ADR-0108 decision 7: Depth of field starts focused on the camera's target with a
    /// blur of 6 px, turns off cleanly, and Focus on selection measures the selected
    /// item's distance along the view.
    #[test]
    fn depth_of_field_starts_on_the_target_and_focuses_on_the_selection() {
        use crate::store::Selection;
        use jstream_control::protocol::{
            Crop, ItemGeometry, ItemInfo, ItemTransform, PlacementForm, Size, Vec2,
        };
        let mut geometry = ItemGeometry::try_new(
            Vec2::new(960.0, 540.0).expect("finite"),
            Size::try_new(100.0, 100.0).expect("positive"),
        )
        .expect("geometry");
        geometry.depth_px = Finite32::new(-300.0).expect("finite");
        let mut scene: SceneInfo = serde_json::from_value(serde_json::json!({
            "scene_id": "a",
            "name": "A",
            "items": [],
        }))
        .expect("a scene");
        scene.items.push(ItemInfo {
            item_id: "i".into(),
            source_id: "s".into(),
            placement: PlacementForm::Geometry,
            transform: ItemTransform::IDENTITY,
            geometry: Some(geometry),
            crop: Crop::NONE,
            visible: true,
            effects: Vec::new(),
            effects_space: jstream_protocol::EffectsSpace::Box,
            opacity: Finite32::ONE,
            blend: Default::default(),
            transition: jstream_control::protocol::ItemTransition::default(),
            locked: false,
            motion: Vec::new(),
        });
        let c = Collection {
            scenes: vec![scene],
            ..Collection::default()
        };
        let a = SceneId::new("a");
        let mut camera = SceneCamera::framing(c.canvas);
        depth_of_field(&mut camera, true);
        let target = camera.target_distance();
        assert_eq!(camera.focus_px.map(Finite32::get), Some(target));
        assert_eq!(camera.blur_px.get(), DEFAULT_BLUR_PX);
        assert!(camera.validate().is_ok());
        let mut softer = camera;
        CameraField::Blur.set(&mut softer, 12.0);
        depth_of_field(&mut softer, true);
        assert_eq!(
            softer.blur_px.get(),
            12.0,
            "turning it on again keeps the blur"
        );
        let picked = Selection {
            scene: Some(a.clone()),
            item: Some(jstream_control::protocol::ItemId::new("i")),
            also: Vec::new(),
        };
        let focus = focus_on(&c, &a, &picked, camera).expect("in front");
        assert!((focus - (target + 300.0)).abs() < 0.01, "{focus}");
        assert_eq!(focus_on(&c, &a, &Selection::default(), camera), None);
        let mut too_soft = camera;
        CameraField::Blur.set(&mut too_soft, 80.0);
        let before = SceneDraft::of(&c, &a).expect("the scene");
        let draft = SceneDraft {
            camera: Some(too_soft),
            ..before.clone()
        };
        assert!(
            scene_edits(&c, &a, &before, &draft).is_err(),
            "a blur past 64 is refused"
        );
        depth_of_field(&mut camera, false);
        assert_eq!((camera.focus_px, camera.blur_px.get()), (None, 0.0));
    }

    /// ADR-0094 §6: a keyframe scene's Own camera starts from its parent's, or from the
    /// framing when the parent is 2D (ADR-0128); any other scene starts from the framing.
    #[test]
    fn a_keyframe_starts_its_own_camera_from_its_parents_or_the_framing() {
        let scenes: Vec<SceneInfo> = serde_json::from_value(serde_json::json!([
            { "scene_id": "p", "name": "P", "items": [] },
            { "scene_id": "k", "name": "K", "items": [], "variant_of": "p" },
        ]))
        .expect("scenes");
        let mut c = Collection {
            scenes,
            ..Collection::default()
        };
        let p = SceneId::new("p");
        let k = SceneId::new("k");
        let framing = SceneCamera::framing(c.canvas);
        assert_eq!(camera_start(&c, &p), Some(framing));
        assert_eq!(
            camera_start(&c, &k),
            Some(framing),
            "a 2D parent's keyframe may have one (ADR-0128)"
        );
        let mut parents = framing;
        CameraField::Position(0).set(&mut parents, 300.0);
        c.scenes[0].camera = Some(parents);
        assert_eq!(camera_start(&c, &k), Some(parents));
        assert_eq!(camera_start(&c, &SceneId::new("gone")), None);
    }

    /// A scene whose items already have depth or angles warns that a camera changes them.
    #[test]
    fn items_with_3d_are_noticed() {
        use jstream_control::protocol::{
            Crop, ItemGeometry, ItemInfo, ItemTransform, PlacementForm, Size, Vec2,
        };
        let mut geometry = ItemGeometry::try_new(
            Vec2::new(10.0, 10.0).expect("finite"),
            Size::try_new(10.0, 10.0).expect("positive"),
        )
        .expect("geometry");
        let mut scene: SceneInfo = serde_json::from_value(serde_json::json!({
            "scene_id": "a",
            "name": "A",
            "items": [],
        }))
        .expect("a scene");
        let mut c = Collection {
            scenes: vec![scene.clone()],
            ..Collection::default()
        };
        let a = SceneId::new("a");
        assert!(!has_3d_items(&c, &a));
        geometry.turn_deg = Finite32::new(30.0).expect("finite");
        scene.items.push(ItemInfo {
            motion: Default::default(),
            item_id: "i".into(),
            source_id: "s".into(),
            placement: PlacementForm::Geometry,
            transform: ItemTransform::IDENTITY,
            geometry: Some(geometry),
            crop: Crop::NONE,
            visible: true,
            effects: Vec::new(),
            opacity: Finite32::ONE,
            blend: Default::default(),
            transition: jstream_control::protocol::ItemTransition::default(),
            locked: false,
            effects_space: jstream_protocol::EffectsSpace::Box,
        });
        c.scenes = vec![scene];
        assert!(has_3d_items(&c, &a));
    }

    /// The swatch shows a background's colour without its alpha, and picking one keeps
    /// the alpha.
    #[test]
    fn the_swatch_keeps_a_background_s_alpha() {
        assert_eq!(background_swatch("#0F111580"), "#0f1115");
        assert_eq!(background_swatch(""), "#000000");
        assert_eq!(background_swatch("blue"), "#000000");
        assert_eq!(with_swatch(Some("#0f111580"), "#ff0000"), "#ff000080");
        assert_eq!(with_swatch(Some("#0f1115"), "#ff0000"), "#ff0000");
        assert_eq!(with_swatch(None, "#ff0000"), "#ff0000");
    }

    /// Rebinding a scene's hotkey keeps the preset its old binding switched with.
    #[test]
    fn a_rebind_keeps_the_binding_s_preset() {
        let fade = Action::SceneSwitch {
            scene_id: SceneId::new("a"),
            preset_id: Some(TransitionPresetId::new("fade")),
        };
        let c = Collection {
            hotkeys: vec![HotkeyBinding::new(Chord::new("F1"), &fade)],
            ..Collection::default()
        };
        let a = SceneId::new("a");
        let edits = scene_edits(&c, &a, &draft("A", Some("F1")), &draft("A", Some("F3")));
        assert_eq!(
            edits.map(|e| e.into_iter().last()),
            Ok(Some(Edit::HotkeySet {
                chord: Chord::new("F3"),
                action: fade,
            }))
        );
    }
}
