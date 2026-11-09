//! The Properties dialog (spec-workspace.md 6.2), one dialog for a source and its
//! filters (#1219): on the left a Properties entry and the source's filter chains, on
//! the right the entry picked, the source's name and settings or a filter's settings
//! ([`PropertiesAndFilters`]). The settings form is the one filters use (`properties.rs`),
//! here editing a draft: each setting the kind marks `live` shows on output as it is
//! edited, as a live value of the dialog's gesture (ADR-0091). OK sends what the user
//! changed since it opened as one batch, ending the gesture, so one undo step; Cancel,
//! Escape and the backdrop drop the draft and cancel the gesture, leaving nothing. Defaults
//! empties the draft, so OK resets each value the dialog opened with. Filter changes
//! apply at once, and Cancel puts back each one the dialog sent ([`FilterUndo`]); the
//! filter rows, pane and dialogs are the Filters panel's (`filters.rs`). A display, window
//! or webcam source shows a live picture of what its edited settings capture above the
//! form (`device_preview.rs`).
//!
//! Docs: docs/UI-FEATURES.md#dialogs

use crate::app::SessionContext;
use crate::dialog::Modal;
use crate::icons::Icon;
use crate::panels::add_source::type_label;
use crate::panels::custom_shader::{self, ShaderEditor};
use crate::panels::device_preview::{self, DevicePreview};
use crate::panels::filters::{FilterUndo, PropertiesAndFilters, chains_of};
use crate::panels::properties::{Form, draft_patch};
use crate::store::{Studio, apply, query};
use jstream_control::protocol::{
    Action, Batch, Call, CallResult, Edit, FilterId, FilterOwner, GestureId, KindDescriptor, Query,
    QueryResult, Settings, SourceId, SourceKind,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// The live values the dialog shows for `draft` (ADR-0091): each setting `kind` marks
/// `live` that the user changed from `base`, and each one already `sent`, which may be
/// back at its base value and must show that. A setting that isn't live waits for OK.
pub fn live_patch(
    kind: &KindDescriptor,
    base: &Settings,
    draft: &Settings,
    sent: &HashSet<String>,
) -> Settings {
    let live = |id: &str| kind.param(id).is_some_and(|p| p.live);
    let mut patch: Settings = draft_patch(base, draft)
        .iter()
        .filter(|(id, _)| live(id))
        .map(|(id, v)| (id, v.clone()))
        .collect();
    for id in sent {
        if patch.get(id).is_none() && live(id) {
            patch.insert(id.clone(), draft.get(id).cloned().unwrap_or(Value::Null));
        }
    }
    patch
}

/// The dialog's state as context: the source it shows while open, and whether a shader
/// source's editor opens over it as it opens.
#[derive(Clone, Copy)]
pub struct PropertiesDialog(RwSignal<Option<SourceId>>, RwSignal<bool>);

impl PropertiesDialog {
    /// A closed dialog.
    pub fn new() -> PropertiesDialog {
        PropertiesDialog(RwSignal::new(None), RwSignal::new(false))
    }

    /// Opens the dialog on `source_id`.
    pub fn open(&self, source_id: SourceId) {
        self.1.set(false);
        self.0.set(Some(source_id));
    }

    /// Opens the dialog on shader source `source_id` with its shader editor over it, as a
    /// new Shader from Add source does (ADR-0135).
    pub fn open_shader(&self, source_id: SourceId) {
        self.1.set(true);
        self.0.set(Some(source_id));
    }
}

impl Default for PropertiesDialog {
    fn default() -> PropertiesDialog {
        PropertiesDialog::new()
    }
}

/// Whether the dialog can show this source: one with settings of its own, so not a
/// nested scene, whose properties are the scene's (DL-03).
pub fn shows(kind: &SourceKind) -> bool {
    matches!(kind, SourceKind::Builtin { .. })
}

/// The dialog, drawn while it is open.
#[component]
pub fn SourceProperties() -> impl IntoView {
    let dialog = expect_context::<PropertiesDialog>();
    let studio = expect_context::<Studio>();
    let close = move || dialog.0.set(None);
    move || {
        dialog.0.get().map(|source_id| {
            // A source just created reaches the collection after the dialog opens, so
            // the dialog waits for it; one that leaves once shown closes it.
            let present = {
                let source_id = source_id.clone();
                Memo::new(move |_| {
                    studio.collection.with(|c| {
                        c.as_ref()
                            .and_then(|c| c.source(&source_id))
                            .is_some_and(|s| shows(&s.kind))
                    })
                })
            };
            let seen = StoredValue::new(false);
            move || {
                if present.get() {
                    seen.set_value(true);
                    let shader = dialog.1.get_untracked();
                    Some(view! {
                        <PropertiesBody source_id=source_id.clone() shader=shader close=close />
                    })
                } else {
                    if seen.get_value() {
                        request_animation_frame(close);
                    }
                    None
                }
            }
        })
    }
}

#[component]
fn PropertiesBody(
    source_id: SourceId,
    /// Whether a shader source's editor opens over the dialog at once (ADR-0135).
    shader: bool,
    close: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let source = studio.collection.with_untracked(|c| {
        c.as_ref()
            .and_then(|c| c.source(&source_id))
            .filter(|s| shows(&s.kind))
            .cloned()
    });
    let Some(source) = source else {
        // Gone, or not a source it shows: nothing to draw.
        request_animation_frame(close);
        return ().into_any();
    };
    let kind_id = match &source.kind {
        SourceKind::Builtin { kind } => kind.clone(),
        _ => String::new(),
    };
    let (type_name, icon) =
        type_label(&kind_id).unwrap_or(if custom_shader::pack_of(&kind_id).is_some() {
            ("Shader", "sparkle")
        } else {
            ("Source", "image")
        });
    let (picture, audio) = chains_of(&source);
    // Filter edits apply at once; Cancel puts back each one the dialog sent.
    let owner = FilterOwner::Source {
        source_id: source_id.clone(),
    };
    let undo = FilterUndo::begin(std::slice::from_ref(&owner));
    let selected = RwSignal::new(None::<FilterId>);

    // What the dialog opened on: OK sends only what the user changed from it, so an
    // edit another client made meanwhile stays (ADR-0032, last writer wins per field).
    let opened = StoredValue::new((source.settings.clone(), source.name.clone()));
    let draft = RwSignal::new(source.settings.clone());
    let name = RwSignal::new(source.name.clone());
    let errors = RwSignal::new(HashMap::<String, String>::new());
    let problem = RwSignal::new(None::<String>);
    // The dialog's gesture once a live setting changed, and the settings it has shown;
    // OK ends it with its batch, anything else cancels it (ADR-0091).
    let gesture = StoredValue::new(None::<GestureId>);
    let sent = StoredValue::new(HashSet::<String>::new());
    let committed = StoredValue::new(false);
    on_cleanup(move || {
        if committed.get_value() {
            return;
        }
        if let (Some(gesture_id), Some(s)) = (gesture.get_value(), session.get()) {
            spawn_local(async move {
                let action = Action::LiveCancel { gesture_id };
                let _cancelled = s.control.call(Call::Action { action }).await;
            });
        }
    });

    if let Some(s) = session.get() {
        let kind_id = kind_id.clone();
        spawn_local(async move {
            if let Err(error) = studio.descriptor(&s, &kind_id).await {
                problem.try_set(Some(error.to_string()));
            }
        });
    }
    let descriptor = {
        let kind_id = kind_id.clone();
        move || studio.descriptors.with(|d| d.get(&kind_id).cloned())
    };
    // Shows the draft's live settings on output: the first opens the gesture.
    let show_live = StoredValue::new({
        let source_id = source_id.clone();
        let descriptor = descriptor.clone();
        move || {
            let (Some(kind), Some(s)) = (descriptor(), session.get()) else {
                return;
            };
            let base = opened.with_value(|(base, _)| base.clone());
            let patch =
                sent.with_value(|sent| draft.with_untracked(|d| live_patch(&kind, &base, d, sent)));
            if patch.is_empty() {
                return;
            }
            sent.update_value(|sent| sent.extend(patch.iter().map(|(id, _)| id.to_owned())));
            let gesture_id = match gesture.get_value() {
                Some(id) => id,
                None => {
                    let id = GestureId::new(studio.fresh_id(&s, "props"));
                    gesture.set_value(Some(id.clone()));
                    id
                }
            };
            let action = Action::LiveSet {
                gesture_id,
                values: vec![Edit::SourceSetSettings {
                    source_id: source_id.clone(),
                    patch,
                }],
            };
            spawn_local(async move {
                if let Err(error) = s.control.call(Call::Action { action }).await {
                    problem.try_set(Some(error.to_string()));
                }
            });
        }
    });

    // A field's change goes to the draft; `null` is the default, so no value at all.
    let commit = move |_: SourceId, param: String, value: Value| {
        errors.update(|e| _ = e.remove(&param));
        draft.update(|d| {
            if value.is_null() {
                d.0.remove(&param);
            } else {
                d.insert(param, value);
            }
        });
        show_live.with_value(|show| show());
    };
    let defaults = move |_| {
        errors.set(HashMap::new());
        draft.set(Settings::new());
        show_live.with_value(|show| show());
    };
    let ok = {
        let source_id = source_id.clone();
        move |_| {
            let Some(s) = session.get() else { return };
            if studio
                .collection
                .with_untracked(|c| c.as_ref().and_then(|c| c.source(&source_id)).is_none())
            {
                close();
                return;
            }
            let (base, old_name) = opened.get_value();
            let mut edits = Vec::new();
            let new_name = name.get_untracked().trim().to_owned();
            if !new_name.is_empty() && new_name != old_name {
                edits.push(Edit::SourceRename {
                    source_id: source_id.clone(),
                    name: new_name,
                });
            }
            let patch = draft.with_untracked(|d| draft_patch(&base, d));
            if !patch.is_empty() {
                edits.push(Edit::SourceSetSettings {
                    source_id: source_id.clone(),
                    patch,
                });
            }
            let ending = gesture.get_value();
            if edits.is_empty() {
                // Changed and changed back: the gesture shows the opened values, and its
                // cancel (on close) drops it.
                close();
                return;
            }
            let Some(gesture_id) = ending else {
                spawn_local(async move {
                    match apply(&s, edits, None).await {
                        Ok(_) => close(),
                        Err(error) => _ = problem.try_set(Some(error.to_string())),
                    }
                });
                return;
            };
            // One batch ending the gesture: one undo step for the whole dialog.
            let batch = Batch {
                edits,
                expected_rev: None,
                idempotency_key: None,
                ends_gesture: Some(gesture_id),
            };
            spawn_local(async move {
                match s.control.call(Call::Batch(batch)).await {
                    Ok(answer) if matches!(answer.result, CallResult::Batch { .. }) => {
                        committed.set_value(true);
                        close();
                    }
                    Ok(_) => {
                        _ = problem.try_set(Some("The engine gave an unexpected answer.".into()))
                    }
                    Err(error) => _ = problem.try_set(Some(error.to_string())),
                }
            });
        }
    };

    // A shader source written in the editor (ADR-0135) opens it again from here. A change
    // writes the pack at once, as in the Filters window: Cancel doesn't undo it.
    let shader_pack = StoredValue::new(custom_shader::pack_of(&kind_id).map(str::to_owned));
    let editing = RwSignal::new(None::<custom_shader::Opened>);
    let open_shader = move || {
        let (Some(pack), Some(s)) = (shader_pack.get_value(), session.get()) else {
            return;
        };
        spawn_local(async move {
            let read = Query::PackRead {
                pack: pack.clone(),
                path: None,
            };
            let opened = match query(&s, read).await {
                Ok(QueryResult::PackRead { files }) => custom_shader::open(&pack, &files),
                Ok(other) => Err(format!("The engine answered `{}`.", other.name())),
                Err(error) => Err(error.to_string()),
            };
            match opened {
                Ok(opened) => _ = editing.try_set(Some(opened)),
                Err(error) => _ = problem.try_set(Some(error)),
            }
        });
    };
    if shader {
        open_shader();
    }
    let shader_button = shader_pack.get_value().map(|_| {
        view! {
            <button type="button" class="secondary props-shader" on:click=move |_| open_shader()>
                "Edit shader…"
            </button>
        }
    });
    let shader_editor = move || {
        let opened = editing.get()?;
        let pack = shader_pack.get_value()?;
        Some(view! {
            <ShaderEditor
                pack=pack
                opened=opened
                close=move || editing.set(None)
                save_as=|_, _| {}
            />
        })
    };

    // A display, window or webcam shows what the settings being edited capture, so
    // the user sees the device they pick before OK (DL-02).
    let preview = device_preview::shows(&kind_id)
        .then(|| view! { <DevicePreview kind_id=kind_id.clone() draft=draft /> });
    let title = format!("{type_name} properties");
    let subtitle = source.name.clone();
    let form = {
        let source_id = source_id.clone();
        move || match descriptor() {
            // A refused `kind.describe` shows as the problem instead.
            None if problem.with(Option::is_some) => ().into_any(),
            None => view! { <p class="placeholder">"Loading…"</p> }.into_any(),
            Some(kind) if kind.params.is_empty() => {
                view! { <p class="muted">{kind.name} " has no settings."</p> }.into_any()
            }
            Some(kind) => {
                let source_id = source_id.clone();
                view! {
                    <Form
                        kind=kind
                        source_id=source_id
                        settings=draft
                        errors=errors
                        commit=commit
                    />
                }
                .into_any()
            }
        }
    };

    // Cancel: the draft and its gesture go (on cleanup), and every filter edit is put
    // back.
    let cancel = move || {
        let edits = undo.restore();
        if let (false, Some(s)) = (edits.is_empty(), session.get()) {
            studio.apply(&s, edits);
        }
        close();
    };

    view! {
        <Modal label="Properties" class="props-dialog" cancel=cancel>
            <div class="modal-header">
                <div class="props-head">
                    <Icon name=icon size=20 />
                    <div class="modal-titles">
                        <h2 class="modal-title">{title}</h2>
                        <span class="modal-sub">{subtitle}</span>
                    </div>
                </div>
                <button type="button" class="modal-close" aria-label="Close" on:click=move |_| cancel()>
                    "×"
                </button>
            </div>
            <PropertiesAndFilters
                owner=owner
                video=picture
                audio=audio
                selected=selected
                note="everywhere this source appears"
            >
                {preview}
                <label class="field props-name">
                    <span class="label">"Name"</span>
                    <input type="text" bind:value=name />
                </label>
                {shader_button}
                {form}
                {move || problem.get().map(|e| view! { <p class="problem">{e}</p> })}
            </PropertiesAndFilters>
            <div class="modal-footer">
                <button
                    type="button"
                    class="secondary props-defaults"
                    hidden=move || selected.with(Option::is_some)
                    on:click=defaults
                >
                    "Defaults"
                </button>
                <button type="button" class="secondary" on:click=move |_| cancel()>"Cancel"</button>
                <button type="button" class="primary" data-primary on:click=ok>"OK"</button>
            </div>
        </Modal>
        {shader_editor}
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{KindCategory, ParamDescriptor, ParamType};
    use serde_json::json;

    fn param(id: &str, live: bool) -> ParamDescriptor {
        ParamDescriptor {
            warning: None,
            id: id.into(),
            ty: ParamType::Color { alpha: false },
            default: json!("#000000"),
            label: id.into(),
            description: None,
            group: None,
            advanced: false,
            visible_if: None,
            live,
        }
    }

    fn kind() -> KindDescriptor {
        KindDescriptor {
            kind_id: "test".into(),
            category: KindCategory::Source,
            name: "Test".into(),
            version: 1,
            video: true,
            audio: false,
            max_outset_px: None,
            params: vec![
                param("fill", true),
                param("edge", true),
                param("device", false),
            ],
        }
    }

    fn settings(pairs: &[(&str, Value)]) -> Settings {
        pairs.iter().map(|(k, v)| (*k, v.clone())).collect()
    }

    #[test]
    fn live_patch_holds_the_changed_live_settings_only() {
        let base = settings(&[("fill", json!("#ff0000")), ("device", json!("a"))]);
        let draft = settings(&[("fill", json!("#00ff00")), ("device", json!("b"))]);
        let patch = live_patch(&kind(), &base, &draft, &HashSet::new());
        assert_eq!(patch, settings(&[("fill", json!("#00ff00"))]));
    }

    #[test]
    fn a_live_setting_already_sent_stays_in_the_patch_when_put_back() {
        let base = settings(&[("fill", json!("#ff0000"))]);
        let sent: HashSet<String> = ["fill".into(), "edge".into()].into();
        // Fill is back at its base value and edge was never stored: both must show that.
        let patch = live_patch(&kind(), &base, &base.clone(), &sent);
        assert_eq!(
            patch,
            settings(&[("fill", json!("#ff0000")), ("edge", Value::Null)])
        );
    }

    #[test]
    fn defaults_send_null_for_each_live_setting_the_source_stored() {
        let base = settings(&[("fill", json!("#ff0000")), ("device", json!("a"))]);
        let patch = live_patch(&kind(), &base, &Settings::default(), &HashSet::new());
        assert_eq!(patch, settings(&[("fill", Value::Null)]));
    }
}
