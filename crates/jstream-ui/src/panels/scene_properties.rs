//! The scene Properties dialog (spec-workspace.md 6.2.5): a scene's name, its
//! Transition in and out, the hotkey that switches to it, whether a multiview shows it,
//! its background colour, and whether its audio reaches the Mixer when it is nested.
//! Everything is a draft until OK, which sends what changed as one batch
//! (`scene.rename`, `scene.set_transitions`, `scene.set_meta`, `scene.set_options`,
//! `hotkey.clear` and `hotkey.set`); Cancel, Escape and the backdrop drop it, and
//! Defaults clears the overrides, the hotkey and the background and checks both switches
//! again, keeping the name. A keyframe scene shows its parent's background, which its
//! dialog shows and doesn't edit. The scene's filters show on the right, each change sent
//! at once, as the source Properties dialog's are.
//!
//! Docs: docs/UI-FEATURES.md#dialogs

use crate::app::SessionContext;
use crate::dialog::Modal;
use crate::hotkeys;
use crate::icons::Icon;
use crate::panels::filters::{Chains, FilterDialogs, scene_owner};
use crate::panels::inspector::transition_options;
use crate::store::{Collection, Studio, apply};
use jstream_control::protocol::{
    Action, Chord, Edit, FilterOwner, SceneId, TransitionPresetId, parse_rgba,
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
        })
    }
}

/// The edits that turn `before` into `draft` for `scene_id`, in one batch: a rename
/// (a blank name is ignored), the overrides, the multiview flag (the scene's other
/// metadata kept), the options, then the hotkey (the old chord cleared
/// before the new one is bound, keeping the preset the old binding named). Refused when
/// the background isn't a colour, or the new chord is bound already, which
/// `scene.set_options` and `hotkey.set` would refuse too.
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
        return Err("The background colour must be #rrggbb or #rrggbbaa.".into());
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
