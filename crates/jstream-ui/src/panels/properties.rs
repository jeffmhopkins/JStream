//! The properties form: the selected item's source, with a field for each parameter its
//! kind's descriptor (`kind.describe`) lists, and no code per kind (ADR-0025). Fields
//! follow the descriptor's display order, groups, `advanced` and `visible_if` hints.
//! A changed field is checked with the descriptor's own check, then sent as a
//! one-parameter `source.set_settings` patch; "Reset" sends `null` for the default.
//! A capture target parameter (a display or window source's `target`) is a list of the
//! targets of its kind that `capture.targets` reports, read again each time the list
//! gets focus, so a window opened since shows up.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/adr/0025-typed-settings-kind-descriptors.md

use crate::app::SessionContext;
use crate::icons::Icon;
use crate::store::{Collection, Studio, apply, query};
use jstream_control::protocol::{
    AssetRoot, CaptureTargetInfo, CaptureTargetKind, Edit, KindDescriptor, Media, ParamDescriptor,
    ParamType, Query, QueryResult, Reference, Settings, SourceId, SourceInfo, SourceKind,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;
use std::collections::HashMap;

/// The parameters of one form section: those sharing a `group`, or the advanced ones.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    /// The group's name, `None` for parameters with no group.
    pub group: Option<String>,
    /// Whether this is the section of advanced parameters, shown collapsed.
    pub advanced: bool,
    /// Its parameters, in the descriptor's order.
    pub params: Vec<ParamDescriptor>,
}

/// The descriptor's parameters split into sections: one per group in order of first
/// appearance (ungrouped ones first), then every advanced parameter in one collapsed
/// section at the end.
pub fn sections(kind: &KindDescriptor) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut advanced = Vec::new();
    for param in &kind.params {
        if param.advanced {
            advanced.push(param.clone());
            continue;
        }
        match out.iter_mut().find(|s| s.group == param.group) {
            Some(section) => section.params.push(param.clone()),
            None => out.push(Section {
                group: param.group.clone(),
                advanced: false,
                params: vec![param.clone()],
            }),
        }
    }
    out.sort_by_key(|s| s.group.is_some());
    if !advanced.is_empty() {
        out.push(Section {
            group: Some("Advanced".into()),
            advanced: true,
            params: advanced,
        });
    }
    out
}

/// The value a parameter has in `settings`, or its default when the map lacks it.
pub fn value_of<'a>(param: &'a ParamDescriptor, settings: &'a Settings) -> &'a Value {
    settings.get(&param.id).unwrap_or(&param.default)
}

/// The `source.set_settings` patch of what the user changed in `draft` from `base`, the
/// settings the draft started from: each value that differs, and `null` (the default)
/// for each one the draft no longer has. A field the user left alone is not in it, so a
/// change another client made meanwhile stays.
pub fn draft_patch(base: &Settings, draft: &Settings) -> Settings {
    let mut patch: Settings = draft
        .iter()
        .filter(|(id, value)| base.get(id) != Some(*value))
        .map(|(id, value)| (id, value.clone()))
        .collect();
    for (id, _) in base.iter().filter(|(id, _)| draft.get(id).is_none()) {
        patch.insert(id, Value::Null);
    }
    patch
}

/// Whether a parameter is shown: always, unless its `visible_if` names another
/// parameter whose current value differs.
pub fn is_visible(param: &ParamDescriptor, kind: &KindDescriptor, settings: &Settings) -> bool {
    let Some(rule) = &param.visible_if else {
        return true;
    };
    let current = match kind.param(&rule.param) {
        Some(other) => value_of(other, settings),
        None => settings.get(&rule.param).unwrap_or(&Value::Null),
    };
    *current == rule.equals
}

/// A typed field's text as the input shows it.
pub fn display(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The value a text, number, colour or choice input's text stands for, checked against
/// the parameter. The error is the descriptor's reason, for display under the field.
pub fn parse(param: &ParamDescriptor, text: &str) -> Result<Value, String> {
    let value = match &param.ty {
        ParamType::Int { .. } => {
            let n: i64 = text
                .trim()
                .parse()
                .map_err(|_| "must be a whole number".to_owned())?;
            Value::from(n)
        }
        ParamType::Float { .. } => {
            let n: f64 = text
                .trim()
                .parse()
                .map_err(|_| "must be a number".to_owned())?;
            serde_json::Number::from_f64(n)
                .map(Value::Number)
                .ok_or_else(|| "must be a finite number".to_owned())?
        }
        ParamType::Bool => Value::Bool(text == "true"),
        _ => Value::String(text.to_owned()),
    };
    param.check(&value)?;
    Ok(value)
}

/// A colour after the colour picker chose `rgb` (`#rrggbb`): the picker has no alpha,
/// so an alpha byte the old value carried is kept.
pub fn with_rgb(old: &str, rgb: &str) -> String {
    let alpha = old
        .strip_prefix('#')
        .filter(|h| h.len() == 8)
        .map(|h| &h[6..]);
    format!("{rgb}{}", alpha.unwrap_or_default())
}

/// The `#rrggbb` part of a colour, for the colour picker.
pub fn rgb_part(color: &str) -> String {
    color.get(..7).unwrap_or("#000000").to_owned()
}

/// The asset reference a root and a path make, or `null` for an empty path, checked
/// against the parameter.
pub fn asset_value(param: &ParamDescriptor, root: AssetRoot, path: &str) -> Result<Value, String> {
    let value = if path.is_empty() {
        Value::Null
    } else {
        Reference::Asset {
            root,
            path: path.to_owned(),
        }
        .to_value()
    };
    param.check(&value)?;
    Ok(value)
}

fn root_name(root: AssetRoot) -> &'static str {
    match root {
        AssetRoot::App => "app",
        AssetRoot::Absolute => "absolute",
        _ => "collection",
    }
}

fn root_from(name: &str) -> AssetRoot {
    match name {
        "app" => AssetRoot::App,
        "absolute" => AssetRoot::Absolute,
        _ => AssetRoot::Collection,
    }
}

/// What the form is showing: the source, and its kind's descriptor once read.
#[derive(Clone, Debug, PartialEq)]
enum Target {
    /// Nothing selected.
    Nothing,
    /// A `scene` source: it has no settings.
    Nested(SourceInfo, String),
    /// A built-in source, with its kind's ID.
    Builtin(SourceInfo, String),
}

fn target(collection: &Collection, item: Option<&jstream_control::protocol::ItemId>) -> Target {
    let source = item
        .and_then(|id| collection.item(id))
        .and_then(|(_, item)| collection.source(&item.source_id));
    match source {
        None => Target::Nothing,
        Some(source) => match &source.kind {
            SourceKind::Builtin { kind } => Target::Builtin(source.clone(), kind.clone()),
            SourceKind::Scene { scene_id } => {
                let name = collection
                    .scene(scene_id)
                    .map_or_else(|| scene_id.to_string(), |s| s.name.clone());
                Target::Nested(source.clone(), name)
            }
            _ => Target::Nothing,
        },
    }
}

/// The properties form for the selected item's source.
#[component]
pub fn Properties() -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    // Field errors by parameter ID: a refused check or a refused edit.
    let errors = RwSignal::new(HashMap::<String, String>::new());
    let describe_error = RwSignal::new(None::<String>);

    let current = Memo::new(move |_| {
        let item = studio.selection.with(|s| s.item.clone());
        studio.collection.with(|c| {
            c.as_ref()
                .map_or(Target::Nothing, |c| target(c, item.as_ref()))
        })
    });
    let shown = Memo::new(move |_| match current.get() {
        Target::Builtin(source, kind) => Some((source.source_id, kind)),
        _ => None,
    });
    // Clears the errors when another source is shown, and reads its kind's descriptor
    // the first time that kind is shown.
    Effect::new(move |_| {
        errors.set(HashMap::new());
        describe_error.set(None);
        let (Some((_, kind)), Some(s)) = (shown.get(), session.get()) else {
            return;
        };
        spawn_local(async move {
            if let Err(error) = studio.descriptor(&s, &kind).await {
                describe_error.set(Some(error.to_string()));
            }
        });
    });

    let commit = move |source_id: SourceId, param: String, value: Value| {
        let Some(s) = session.get() else { return };
        errors.update(|e| _ = e.remove(&param));
        let mut patch = Settings::new();
        patch.insert(param.clone(), value);
        spawn_local(async move {
            let edit = Edit::SourceSetSettings { source_id, patch };
            if let Err(error) = apply(&s, vec![edit], None).await {
                errors.update(|e| _ = e.insert(param, error.to_string()));
            }
        });
    };

    move || match current.get() {
        Target::Nothing => view! {
            <p class="placeholder">"Select a source to edit it."</p>
        }
        .into_any(),
        Target::Nested(_, scene) => view! {
            <p class="muted">"Shows the scene “" {scene} "”. A nested scene has no settings."</p>
        }
        .into_any(),
        Target::Builtin(source, kind) => {
            let descriptor = move || studio.descriptors.with(|d| d.get(&kind).cloned());
            let source_id = source.source_id.clone();
            view! {
                {move || describe_error.get().map(|e| view! { <p class="problem">{e}</p> })}
                {move || match descriptor() {
                    None => view! { <p class="placeholder">"Loading…"</p> }.into_any(),
                    Some(kind) if kind.params.is_empty() => {
                        view! { <p class="muted">{kind.name} " has no settings."</p> }.into_any()
                    }
                    Some(kind) => {
                        let source_id = source_id.clone();
                        let settings = {
                            let source_id = source_id.clone();
                            Signal::derive(move || settings_of(studio, &source_id))
                        };
                        view! {
                            <Form kind=kind source_id=source_id settings=settings errors=errors commit=commit />
                        }
                        .into_any()
                    }
                }}
            }
            .into_any()
        }
    }
}

/// The settings now stored for `source_id`, read reactively.
fn settings_of(studio: Studio, source_id: &SourceId) -> Settings {
    studio.collection.with(|c| {
        c.as_ref()
            .and_then(|c| c.source(source_id))
            .map(|s| s.settings.clone())
            .unwrap_or_default()
    })
}

/// The fields for one kind's parameters, showing `settings`: the stored ones in the
/// Inspector, a draft in the Properties dialog. `commit` gets each checked change, with
/// `null` for the default.
#[component]
pub fn Form(
    /// The source's kind, whose parameters the form shows.
    kind: KindDescriptor,
    /// The source, for its reference fields and for `commit`.
    source_id: SourceId,
    /// The settings shown.
    #[prop(into)]
    settings: Signal<Settings>,
    /// Field errors by parameter ID.
    errors: RwSignal<HashMap<String, String>>,
    /// Takes each checked change.
    commit: impl Fn(SourceId, String, Value) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let sections = sections(&kind);
    sections
        .into_iter()
        .map(|section| {
            let fields = section
                .params
                .into_iter()
                .map(|param| {
                    let kind = kind.clone();
                    let visible = {
                        let param = param.clone();
                        move || settings.with(|s| is_visible(&param, &kind, s))
                    };
                    let source_id = source_id.clone();
                    view! {
                        <Show when=visible>
                            <Field param=param.clone() source_id=source_id.clone() settings=settings errors=errors commit=commit />
                        </Show>
                    }
                })
                .collect_view();
            match (section.advanced, section.group) {
                (true, group) => view! {
                    <details class="section">
                        <summary>{group}</summary>
                        {fields}
                    </details>
                }
                .into_any(),
                (false, Some(group)) => view! {
                    <fieldset class="section">
                        <legend>{group}</legend>
                        {fields}
                    </fieldset>
                }
                .into_any(),
                (false, None) => view! { <div class="section">{fields}</div> }.into_any(),
            }
        })
        .collect_view()
}

/// One parameter's label, input, reset button and error.
#[component]
fn Field(
    param: ParamDescriptor,
    source_id: SourceId,
    settings: Signal<Settings>,
    errors: RwSignal<HashMap<String, String>>,
    commit: impl Fn(SourceId, String, Value) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let id = param.id.clone();
    let value = {
        let param = param.clone();
        move || settings.with(|s| value_of(&param, s).clone())
    };
    // Checks a typed value and sends it, or shows why it was refused.
    let send = {
        let source_id = source_id.clone();
        let id = id.clone();
        move |checked: Result<Value, String>| match checked {
            Ok(v) => commit(source_id.clone(), id.clone(), v),
            Err(reason) => errors.update(|e| _ = e.insert(id.clone(), reason)),
        }
    };
    let input = input_for(&param, value.clone(), send.clone(), studio, &source_id);
    let reset = {
        let source_id = source_id.clone();
        let id = id.clone();
        move |_| commit(source_id.clone(), id.clone(), Value::Null)
    };
    let error = {
        let id = id.clone();
        move || errors.with(|e| e.get(&id).cloned())
    };
    let unit = match &param.ty {
        ParamType::Int { unit, .. } | ParamType::Float { unit, .. } => unit.clone(),
        _ => None,
    };
    let is_default = {
        let default = param.default.clone();
        move || value() == default
    };
    view! {
        <div class="field" title=param.description.clone().unwrap_or_default()>
            <label>
                <span class="label">{param.label.clone()}</span>
                <span class="control">
                    {input}
                    {unit.map(|u| view! { <span class="unit">{u}</span> })}
                </span>
            </label>
            <button class="reset" title="Reset to the default" aria-label="Reset to the default"
                disabled=is_default on:click=reset>
                <Icon name="reset" size=13 />
            </button>
            {move || error().map(|e| view! { <p class="problem">{e}</p> })}
        </div>
    }
}

/// The input for one parameter type. Each sends a checked value when it is changed
/// (a text field when it loses focus or Enter is pressed).
fn input_for(
    param: &ParamDescriptor,
    value: impl Fn() -> Value + Clone + Send + Sync + 'static,
    send: impl Fn(Result<Value, String>) + Clone + Send + Sync + 'static,
    studio: Studio,
    source_id: &SourceId,
) -> AnyView {
    let p = param.clone();
    let text = {
        let value = value.clone();
        move || display(&value())
    };
    let on_text = {
        let send = send.clone();
        move |event: leptos::ev::Event| send(parse(&p, &event_target_value(&event)))
    };
    match &param.ty {
        ParamType::Bool => {
            let send = send.clone();
            view! {
                <input
                    type="checkbox"
                    prop:checked=move || value() == Value::Bool(true)
                    on:change=move |event| send(Ok(Value::Bool(event_target_checked(&event))))
                />
            }
            .into_any()
        }
        ParamType::Int { min, max, step, .. } => view! {
            <input
                type="number"
                min=min.map(|n| n.to_string())
                max=max.map(|n| n.to_string())
                step=step.map_or_else(|| "1".to_owned(), |n| n.to_string())
                prop:value=text
                on:change=on_text
            />
        }
        .into_any(),
        ParamType::Float { min, max, step, .. } => {
            // A range with both ends gets a slider beside the number.
            let slider = min.zip(*max).map(|(lo, hi)| {
                let on_text = on_text.clone();
                let text = text.clone();
                let step = step.map_or_else(|| "any".to_owned(), |n| n.to_string());
                view! {
                    <input type="range" min=lo.to_string() max=hi.to_string() step=step
                        prop:value=text on:change=on_text />
                }
            });
            view! {
                {slider}
                <input
                    type="number"
                    min=min.map(|n| n.to_string())
                    max=max.map(|n| n.to_string())
                    step=step.map_or_else(|| "any".to_owned(), |n| n.to_string())
                    prop:value=text
                    on:change=on_text
                />
            }
            .into_any()
        }
        ParamType::Enum { options } => {
            let options = options.clone();
            view! {
                <select prop:value=text on:change=on_text>
                    {options
                        .into_iter()
                        .map(|o| view! { <option value=o.value>{o.label}</option> })
                        .collect_view()}
                </select>
            }
            .into_any()
        }
        ParamType::Color { .. } => {
            let p = param.clone();
            let picked = {
                let send = send.clone();
                let value = value.clone();
                move |event: leptos::ev::Event| {
                    let old = display(&value());
                    send(parse(&p, &with_rgb(&old, &event_target_value(&event))));
                }
            };
            let swatch = {
                let text = text.clone();
                move || rgb_part(&text())
            };
            view! {
                <input type="color" prop:value=swatch on:change=picked />
                <input type="text" class="color-text" prop:value=text on:change=on_text />
            }
            .into_any()
        }
        ParamType::String { multiline, max_len } => {
            if *multiline {
                view! { <textarea maxlength=max_len.to_string() prop:value=text on:change=on_text /> }
                    .into_any()
            } else {
                view! { <input type="text" maxlength=max_len.to_string() prop:value=text on:change=on_text /> }
                    .into_any()
            }
        }
        ParamType::Asset { accept } => asset_input(param, value, send, accept),
        ParamType::CaptureTarget { kind, .. } => capture_target_input(param, value, send, *kind),
        ParamType::Scene => reference_select(value, send, move || {
            studio.collection.with(|c| {
                c.iter()
                    .flat_map(|c| &c.scenes)
                    .map(|s| {
                        let r = Reference::Scene {
                            scene_id: s.scene_id.clone(),
                        };
                        (r.to_value(), s.name.clone())
                    })
                    .collect()
            })
        }),
        ParamType::Source { media } => {
            let own = source_id.clone();
            let media = *media;
            reference_select(value, send, move || {
                studio.collection.with(|c| {
                    c.iter()
                        .flat_map(|c| &c.sources)
                        .filter(|s| s.source_id != own && offers(s, media))
                        .map(|s| {
                            let r = Reference::Source {
                                source_id: s.source_id.clone(),
                            };
                            (r.to_value(), s.name.clone())
                        })
                        .collect()
                })
            })
        }
        ParamType::Bus => reference_select(value, send, move || {
            studio.collection.with(|c| {
                c.iter()
                    .flat_map(|c| &c.buses)
                    .map(|b| {
                        let r = Reference::Bus {
                            bus_id: b.bus_id.clone(),
                        };
                        (r.to_value(), b.name.clone())
                    })
                    .collect()
            })
        }),
        // A type this UI doesn't know: shown, not editable.
        _ => view! { <code class="readonly">{text}</code> }.into_any(),
    }
}

/// Whether a source reference parameter restricted to `media` offers `source`. An
/// audio reference offers only sources with a mixer channel: those that report audio
/// settings, less `scene` sources, which have none of their own (ADR-0028), so the
/// select lists what the engine accepts (#459). A video one offers every source, as
/// a source's info doesn't say whether its kind makes video.
fn offers(source: &SourceInfo, media: Option<Media>) -> bool {
    match media {
        Some(Media::Audio) => {
            source.audio.is_some() && !matches!(source.kind, SourceKind::Scene { .. })
        }
        _ => true,
    }
}

/// A select of references, with "None" for `null`. Options are keyed by their JSON.
fn reference_select(
    value: impl Fn() -> Value + Send + Sync + 'static,
    send: impl Fn(Result<Value, String>) + Send + Sync + 'static,
    choices: impl Fn() -> Vec<(Value, String)> + Send + Sync + 'static,
) -> AnyView {
    let current = move || match value() {
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let on_change = move |event: leptos::ev::Event| {
        let picked = event_target_value(&event);
        if picked.is_empty() {
            send(Ok(Value::Null));
        } else {
            send(serde_json::from_str(&picked).map_err(|e| e.to_string()));
        }
    };
    view! {
        <select prop:value=current on:change=on_change>
            <option value="">"None"</option>
            {move || {
                choices()
                    .into_iter()
                    .map(|(v, name)| view! { <option value=v.to_string()>{name}</option> })
                    .collect_view()
            }}
        </select>
    }
    .into_any()
}

/// The choices of a capture target list, as (value, label): the empty value first (the
/// primary display, or no window), then each target of `kind` that `targets` lists, in
/// its order. A `current` value it doesn't list (a window closed since, or a display
/// unplugged) stays a choice, so the list still shows what the source names.
pub fn target_choices(
    kind: CaptureTargetKind,
    targets: &[CaptureTargetInfo],
    current: &str,
) -> Vec<(String, String)> {
    let empty = match kind {
        CaptureTargetKind::Display => "Primary display",
        _ => "None",
    };
    let mut choices = vec![(String::new(), empty.to_owned())];
    for target in targets.iter().filter(|t| t.kind == kind) {
        let name = if target.name.trim().is_empty() {
            format!("Untitled ({})", target.target)
        } else {
            target.name.clone()
        };
        let label = match target.size {
            Some(size) => format!("{name} · {}×{}", size.width, size.height),
            None => name,
        };
        choices.push((target.target.clone(), label));
    }
    if !current.is_empty() && !choices.iter().any(|(value, _)| value == current) {
        let gone = match kind {
            CaptureTargetKind::Display => "Display not connected",
            _ => "Window not open",
        };
        choices.push((current.to_owned(), format!("{gone} ({current})")));
    }
    choices
}

/// A display or window to capture, picked from what `capture.targets` lists now. Until
/// the list arrives, and when the engine can't list targets, it is the ID as text.
fn capture_target_input(
    param: &ParamDescriptor,
    value: impl Fn() -> Value + Clone + Send + Sync + 'static,
    send: impl Fn(Result<Value, String>) + Clone + Send + Sync + 'static,
    kind: CaptureTargetKind,
) -> AnyView {
    let session = expect_context::<SessionContext>();
    // `None` until the first answer; then the targets, or why they couldn't be listed.
    let listed = RwSignal::new(None::<Result<Vec<CaptureTargetInfo>, String>>);
    let refresh = move || {
        let Some(session) = session.get() else {
            return;
        };
        spawn_local(async move {
            let got = match query(&session, Query::CaptureTargets).await {
                Ok(QueryResult::CaptureTargets { targets }) => Ok(targets),
                Ok(other) => Err(format!("unexpected {} result", other.name())),
                Err(e) => Err(e.to_string()),
            };
            // The form may have closed meanwhile.
            listed.try_set(Some(got));
        });
    };
    refresh();
    let current = {
        let value = value.clone();
        move || display(&value())
    };
    let p = param.clone();
    let on_text = {
        let send = send.clone();
        move |event: leptos::ev::Event| send(parse(&p, &event_target_value(&event)))
    };
    let p = param.clone();
    let on_pick = move |event: leptos::ev::Event| send(parse(&p, &event_target_value(&event)));
    // Which input shows: the list once targets arrived, else the text. A refresh changes
    // only the options, so an open list keeps its element and its focus.
    let mode = Memo::new(move |_| {
        listed.with(|l| match l {
            Some(Ok(_)) => Ok(()),
            Some(Err(reason)) => Err(format!("Can't list them: {reason}")),
            None => Err(String::new()),
        })
    });
    let choices = {
        let current = current.clone();
        move || {
            listed.with(|l| match l {
                Some(Ok(targets)) => target_choices(kind, targets, &current()),
                _ => Vec::new(),
            })
        }
    };
    let max_len = match &param.ty {
        ParamType::CaptureTarget { max_len, .. } => *max_len,
        _ => 0,
    };
    let body = move || match mode.get() {
        Ok(()) => {
            let choices = choices.clone();
            // Set again when the options change, which may have reset the selection.
            let selected = {
                let current = current.clone();
                move || {
                    listed.track();
                    current()
                }
            };
            view! {
                <select class="capture-target" prop:value=selected.clone()
                    on:focus=move |_| refresh() on:change=on_pick.clone()>
                    {move || {
                        choices()
                            .into_iter()
                            .map(|(v, label)| view! { <option value=v>{label}</option> })
                            .collect_view()
                    }}
                </select>
            }
            .into_any()
        }
        Err(reason) => view! {
            <input type="text" maxlength=max_len.to_string() title=reason
                prop:value=current.clone() on:change=on_text.clone() />
        }
        .into_any(),
    };
    view! { {body} }.into_any()
}

/// An asset reference: which root, and a path under it. An empty path is no file.
fn asset_input(
    param: &ParamDescriptor,
    value: impl Fn() -> Value + Clone + Send + Sync + 'static,
    send: impl Fn(Result<Value, String>) + Clone + Send + Sync + 'static,
    accept: &[String],
) -> AnyView {
    let parts = move || match Reference::from_value(&value()) {
        Some(Reference::Asset { root, path }) => (root, path),
        _ => (AssetRoot::Collection, String::new()),
    };
    let root = RwSignal::new(untrack(|| parts().0));
    // Follows the stored root when a stored reference has one (another client or an
    // agent may change it), so a later path edit doesn't send a stale root. With no file
    // stored the user's pick stands: there is no stored root to follow.
    Effect::new({
        let parts = parts.clone();
        move |_| {
            let (stored, path) = parts();
            if !path.is_empty() && root.get_untracked() != stored {
                root.set(stored);
            }
        }
    });
    let path = {
        let parts = parts.clone();
        move || parts().1
    };
    let p = param.clone();
    let on_path = {
        let send = send.clone();
        move |event: leptos::ev::Event| {
            send(asset_value(
                &p,
                root.get_untracked(),
                &event_target_value(&event),
            ));
        }
    };
    let p = param.clone();
    let on_root = move |event: leptos::ev::Event| {
        let picked = root_from(&event_target_value(&event));
        root.set(picked);
        let current = parts().1;
        if !current.is_empty() {
            send(asset_value(&p, picked, &current));
        }
    };
    let hint = (!accept.is_empty()).then(|| accept.join(", "));
    view! {
        <select class="asset-root" prop:value=move || root_name(root.get()) on:change=on_root>
            <option value="collection">"Collection folder"</option>
            <option value="app">"App assets"</option>
            <option value="absolute">"Absolute path"</option>
        </select>
        <input type="text" placeholder=hint.unwrap_or_else(|| "path".into()) prop:value=path on:change=on_path />
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::{
        asset_value, display, draft_patch, is_visible, offers, parse, rgb_part, sections,
        target_choices, value_of, with_rgb,
    };
    use jstream_control::protocol::{
        AssetRoot, CaptureTargetInfo, CaptureTargetKind, EnumOption, KindCategory, KindDescriptor,
        ParamDescriptor, ParamType, PixelSize, Reference, Settings, VisibleIf,
    };
    use serde_json::{Value, json};

    fn param(id: &str, ty: ParamType, default: Value) -> ParamDescriptor {
        ParamDescriptor {
            id: id.into(),
            ty,
            default,
            label: id.into(),
            description: None,
            group: None,
            advanced: false,
            visible_if: None,
            live: true,
        }
    }

    fn kind(params: Vec<ParamDescriptor>) -> KindDescriptor {
        KindDescriptor {
            kind_id: "test".into(),
            category: KindCategory::Source,
            name: "Test".into(),
            version: 1,
            video: true,
            audio: false,
            max_outset_px: None,
            params,
        }
    }

    fn int(min: i64, max: i64) -> ParamType {
        ParamType::Int {
            min: Some(min),
            max: Some(max),
            step: None,
            unit: None,
        }
    }

    #[test]
    fn sections_follow_groups_then_advanced() {
        let mut a = param("a", ParamType::Bool, json!(false));
        a.group = Some("Look".into());
        let b = param("b", ParamType::Bool, json!(false));
        let mut c = param("c", ParamType::Bool, json!(false));
        c.advanced = true;
        let mut d = param("d", ParamType::Bool, json!(false));
        d.group = Some("Look".into());
        let out = sections(&kind(vec![a, b, c, d]));
        let shape: Vec<(Option<&str>, bool, Vec<&str>)> = out
            .iter()
            .map(|s| {
                (
                    s.group.as_deref(),
                    s.advanced,
                    s.params.iter().map(|p| p.id.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (None, false, vec!["b"]),
                (Some("Look"), false, vec!["a", "d"]),
                (Some("Advanced"), true, vec!["c"]),
            ]
        );
    }

    #[test]
    fn visible_if_reads_the_other_value_or_its_default() {
        let mode = param(
            "mode",
            ParamType::Enum {
                options: vec![
                    EnumOption {
                        value: "fill".into(),
                        label: "Fill".into(),
                    },
                    EnumOption {
                        value: "image".into(),
                        label: "Image".into(),
                    },
                ],
            },
            json!("fill"),
        );
        let mut file = param("file", ParamType::Asset { accept: vec![] }, Value::Null);
        file.visible_if = Some(VisibleIf {
            param: "mode".into(),
            equals: json!("image"),
        });
        let k = kind(vec![mode.clone(), file.clone()]);
        let mut settings = Settings::new();
        assert!(!is_visible(&file, &k, &settings), "the default decides");
        settings.insert("mode", json!("image"));
        assert!(is_visible(&file, &k, &settings));
        assert!(is_visible(&mode, &k, &settings));
        assert_eq!(value_of(&mode, &settings), &json!("image"));
        assert_eq!(value_of(&file, &settings), &Value::Null);
    }

    #[test]
    fn text_is_parsed_and_checked_by_the_descriptor() {
        let n = param("n", int(0, 10), json!(0));
        assert_eq!(parse(&n, " 7 "), Ok(json!(7)));
        assert_eq!(parse(&n, "11"), Err("must be from 0 to 10".into()));
        assert_eq!(parse(&n, "1.5"), Err("must be a whole number".into()));
        let f = param(
            "f",
            ParamType::Float {
                min: None,
                max: None,
                step: None,
                unit: None,
            },
            json!(0.0),
        );
        assert_eq!(parse(&f, "0.5"), Ok(json!(0.5)));
        assert!(parse(&f, "x").is_err());
        let c = param("c", ParamType::Color { alpha: false }, json!("#000000"));
        assert_eq!(parse(&c, "#a0b1c2"), Ok(json!("#a0b1c2")));
        assert!(parse(&c, "red").is_err());
        let t = param(
            "t",
            ParamType::String {
                multiline: false,
                max_len: 3,
            },
            json!(""),
        );
        assert!(parse(&t, "four").is_err());
        assert_eq!(display(&json!("abc")), "abc");
        assert_eq!(display(&json!(2.5)), "2.5");
        assert_eq!(display(&Value::Null), "");
    }

    /// #459: an audio source reference (a waveform's `audio_source`) offers only the
    /// sources with a mixer channel; any other source reference offers every source.
    #[test]
    fn an_audio_reference_offers_only_sources_with_audio() {
        use jstream_control::protocol::{
            AudioFollow, Finite32, Media, SceneId, SourceAudio, SourceId, SourceInfo, SourceKind,
        };
        let audio = SourceAudio {
            enabled: true,
            follow: AudioFollow::Always,
            gain_db: Finite32::ZERO,
            muted: false,
        };
        let source = |kind: SourceKind, audio: Option<SourceAudio>| SourceInfo {
            source_id: SourceId::new("s"),
            name: "S".into(),
            kind,
            settings: Settings::new(),
            duplicated_from: None,
            audio,
            filters: Default::default(),
            meta: Default::default(),
        };
        let mic = source(
            SourceKind::Builtin {
                kind: "audio_input".into(),
            },
            Some(audio),
        );
        let image = source(
            SourceKind::Builtin {
                kind: "image".into(),
            },
            None,
        );
        let nested = source(
            SourceKind::Scene {
                scene_id: SceneId::new("inner"),
            },
            Some(audio),
        );
        assert!(offers(&mic, Some(Media::Audio)));
        assert!(!offers(&image, Some(Media::Audio)));
        assert!(!offers(&nested, Some(Media::Audio)));
        for any in [None, Some(Media::Video)] {
            assert!(offers(&image, any) && offers(&mic, any) && offers(&nested, any));
        }
    }

    #[test]
    fn the_colour_picker_keeps_alpha() {
        assert_eq!(with_rgb("#11223380", "#aabbcc"), "#aabbcc80");
        assert_eq!(with_rgb("#112233", "#aabbcc"), "#aabbcc");
        assert_eq!(rgb_part("#11223380"), "#112233");
        assert_eq!(rgb_part(""), "#000000");
    }

    #[test]
    fn assets_are_references_or_null() {
        let file = param(
            "file",
            ParamType::Asset {
                accept: vec!["png".into()],
            },
            Value::Null,
        );
        assert_eq!(
            asset_value(&file, AssetRoot::Collection, "img/a.png"),
            Ok(json!({"type": "asset", "root": "collection", "path": "img/a.png"}))
        );
        assert_eq!(asset_value(&file, AssetRoot::App, ""), Ok(Value::Null));
        assert!(asset_value(&file, AssetRoot::Collection, "../a.png").is_err());
        assert!(asset_value(&file, AssetRoot::Collection, "a.jpg").is_err());
        let mask = param("mask", ParamType::Scene, Value::Null);
        let scene = Reference::Scene {
            scene_id: "s".into(),
        };
        assert!(mask.check(&scene.to_value()).is_ok());
    }

    #[test]
    fn the_draft_patch_sends_changes_and_nulls_for_defaults() {
        let stored: Settings = [
            ("text", json!("Hi")),
            ("size", json!(34)),
            ("bold", json!(true)),
        ]
        .into_iter()
        .collect();
        // The text changed, the size went back to its default, a colour was set and
        // the bold flag stayed.
        let draft: Settings = [
            ("text", json!("Hello")),
            ("bold", json!(true)),
            ("color", json!("#fff")),
        ]
        .into_iter()
        .collect();
        let patch = draft_patch(&stored, &draft);
        let want: Settings = [
            ("text", json!("Hello")),
            ("size", Value::Null),
            ("color", json!("#fff")),
        ]
        .into_iter()
        .collect();
        assert_eq!(patch, want);
        assert!(draft_patch(&stored, &stored).is_empty());
        // Defaults: an empty draft resets everything stored.
        assert_eq!(draft_patch(&stored, &Settings::new()).iter().count(), 3);
    }

    #[test]
    fn the_draft_patch_leaves_other_clients_changes_alone() {
        // The dialog opened on `base`; meanwhile another client changed the text and set
        // a colour. The user changed only the size, so only the size is sent.
        let base: Settings = [("text", json!("A")), ("size", json!(34))]
            .into_iter()
            .collect();
        let mut draft = base.clone();
        draft.insert("size".to_owned(), json!(48));
        let want: Settings = [("size", json!(48))].into_iter().collect();
        assert_eq!(draft_patch(&base, &draft), want);
        // Nothing changed: nothing is sent, whatever is stored now.
        assert!(draft_patch(&base, &base).is_empty());
    }

    fn target(kind: CaptureTargetKind, id: &str, name: &str) -> CaptureTargetInfo {
        CaptureTargetInfo {
            kind,
            target: id.into(),
            name: name.into(),
            size: None,
            primary: false,
        }
    }

    #[test]
    fn a_capture_target_lists_its_kind_and_keeps_a_closed_one() {
        let mut listed = vec![
            target(CaptureTargetKind::Display, "d1", "Display 1"),
            target(CaptureTargetKind::Window, "w1", "Notepad"),
            target(CaptureTargetKind::Window, "w2", " "),
        ];
        listed[1].size = Some(PixelSize {
            width: 800,
            height: 600,
        });
        let pairs = |choices: Vec<(String, String)>| {
            choices
                .into_iter()
                .map(|(v, l)| format!("{v}={l}"))
                .collect::<Vec<_>>()
        };
        // Only windows, with their size when known; an untitled one by its ID.
        assert_eq!(
            pairs(target_choices(CaptureTargetKind::Window, &listed, "w1")),
            ["=None", "w1=Notepad · 800×600", "w2=Untitled (w2)"]
        );
        // A display's empty value is the primary display.
        assert_eq!(
            pairs(target_choices(CaptureTargetKind::Display, &listed, "")),
            ["=Primary display", "d1=Display 1"]
        );
        // A window closed since stays the selected choice.
        assert_eq!(
            pairs(target_choices(
                CaptureTargetKind::Window,
                &listed[..1],
                "gone"
            )),
            ["=None", "gone=Window not open (gone)"]
        );
    }
}
