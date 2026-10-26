//! The Properties dialog (spec-workspace.md 6.2): a source's name and settings on the
//! left, its filters on the right. The settings form is the Inspector's (`properties.rs`),
//! here editing a draft: OK sends what the user changed since it opened as one batch,
//! Cancel, Escape and the backdrop drop it, and Defaults empties it, so OK resets each
//! value the dialog opened with. Filter changes apply at once, as in the prototype; the
//! filter rows and dialogs are the Filters panel's (`filters.rs`).
//!
//! Docs: docs/UI-FEATURES.md#dialogs

use crate::app::SessionContext;
use crate::dialog::Modal;
use crate::icons::Icon;
use crate::panels::add_source::type_label;
use crate::panels::filters::{Chains, FilterDialogs, chains_of};
use crate::panels::properties::{Form, draft_patch};
use crate::store::{Studio, apply};
use jstream_control::protocol::{Edit, FilterOwner, Settings, SourceId, SourceKind};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;
use std::collections::HashMap;

/// The dialog's state as context: the source it shows while open.
#[derive(Clone, Copy)]
pub struct PropertiesDialog(RwSignal<Option<SourceId>>);

impl PropertiesDialog {
    /// A closed dialog.
    pub fn new() -> PropertiesDialog {
        PropertiesDialog(RwSignal::new(None))
    }

    /// Opens the dialog on `source_id`.
    pub fn open(&self, source_id: SourceId) {
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
                    Some(view! { <PropertiesBody source_id=source_id.clone() close=close /> })
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
    let (type_name, icon) = type_label(&kind_id).unwrap_or(("Source", "image"));
    let (picture, audio) = chains_of(&source);

    // What the dialog opened on: OK sends only what the user changed from it, so an
    // edit another client made meanwhile stays (ADR-0032, last writer wins per field).
    let opened = StoredValue::new((source.settings.clone(), source.name.clone()));
    let draft = RwSignal::new(source.settings.clone());
    let name = RwSignal::new(source.name.clone());
    let errors = RwSignal::new(HashMap::<String, String>::new());
    let problem = RwSignal::new(None::<String>);

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
    };
    let defaults = move |_| {
        errors.set(HashMap::new());
        draft.set(Settings::new());
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
        }
    };

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

    view! {
        <Modal label="Properties" class="props-dialog" cancel=close>
            <div class="modal-header">
                <div class="props-head">
                    <Icon name=icon size=20 />
                    <div class="modal-titles">
                        <h2 class="modal-title">{title}</h2>
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
                        <input type="text" bind:value=name />
                    </label>
                    {form}
                    {move || problem.get().map(|e| view! { <p class="problem">{e}</p> })}
                </div>
                <FilterSide source_id=source_id.clone() video=picture audio=audio />
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

/// The source's filter chains, each change sent at once, and the Add filter button.
#[component]
fn FilterSide(source_id: SourceId, video: bool, audio: bool) -> impl IntoView {
    let dialogs = expect_context::<FilterDialogs>();
    let add = {
        let source_id = source_id.clone();
        move |_| {
            dialogs.add(FilterOwner::Source {
                source_id: source_id.clone(),
            })
        }
    };
    let owner = FilterOwner::Source { source_id };
    view! {
        <div class="props-side">
            <div class="props-side-head">
                <span class="props-side-title">"Filters"</span>
                <span class="props-side-note">"everywhere this source appears"</span>
            </div>
            <Chains owner=owner video=video audio=audio />
            <p class="props-side-foot">"Changes apply right away. Fine-tune settings in the Filters panel."</p>
            <button type="button" class="primary props-add-filter" on:click=add>"+ Add filter…"</button>
        </div>
    }
}
