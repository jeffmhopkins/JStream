//! The properties form: the selected item's source, with a field for each parameter its
//! kind's descriptor (`kind.describe`) lists, and no code per kind (ADR-0025). Fields
//! follow the descriptor's display order, groups, `advanced` and `visible_if` hints.
//! A changed field is checked with the descriptor's own check, then sent as a
//! one-parameter `source.set_settings` patch; "Reset" sends `null` for the default.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/adr/0025-typed-settings-kind-descriptors.md

use crate::app::SessionContext;
use crate::store::{Collection, Studio, apply};
use jstream_control::protocol::{
    AssetRoot, Edit, KindDescriptor, ParamDescriptor, ParamType, Reference, Settings, SourceId,
    SourceInfo, SourceKind,
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
            <h2>"Properties"</h2>
            <p class="placeholder">"Select a source to edit it."</p>
        }
        .into_any(),
        Target::Nested(source, scene) => view! {
            <h2>"Properties"</h2>
            <p class="source-title">{source.name}</p>
            <p class="muted">"Shows the scene “" {scene} "”. A nested scene has no settings."</p>
        }
        .into_any(),
        Target::Builtin(source, kind) => {
            let descriptor = move || studio.descriptors.with(|d| d.get(&kind).cloned());
            let source_id = source.source_id.clone();
            let title = source.name.clone();
            view! {
                <h2>"Properties"</h2>
                <p class="source-title">{title}</p>
                {move || describe_error.get().map(|e| view! { <p class="problem">{e}</p> })}
                {move || match descriptor() {
                    None => view! { <p class="placeholder">"Loading…"</p> }.into_any(),
                    Some(kind) if kind.params.is_empty() => {
                        view! { <p class="muted">{kind.name} " has no settings."</p> }.into_any()
                    }
                    Some(kind) => {
                        let source_id = source_id.clone();
                        view! {
                            <p class="muted">{kind.name.clone()} " source"</p>
                            <Form kind=kind source_id=source_id errors=errors commit=commit />
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

/// The fields for one kind's parameters.
#[component]
fn Form(
    kind: KindDescriptor,
    source_id: SourceId,
    errors: RwSignal<HashMap<String, String>>,
    commit: impl Fn(SourceId, String, Value) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
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
                        let source_id = source_id.clone();
                        let param = param.clone();
                        move || is_visible(&param, &kind, &settings_of(studio, &source_id))
                    };
                    let source_id = source_id.clone();
                    view! {
                        <Show when=visible>
                            <Field param=param.clone() source_id=source_id.clone() errors=errors commit=commit />
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
    errors: RwSignal<HashMap<String, String>>,
    commit: impl Fn(SourceId, String, Value) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let id = param.id.clone();
    let value = {
        let param = param.clone();
        let source_id = source_id.clone();
        move || value_of(&param, &settings_of(studio, &source_id)).clone()
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
            <button class="reset" title="Reset to the default" disabled=is_default on:click=reset>"↺"</button>
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
        ParamType::Source { .. } => {
            let own = source_id.clone();
            reference_select(value, send, move || {
                studio.collection.with(|c| {
                    c.iter()
                        .flat_map(|c| &c.sources)
                        .filter(|s| s.source_id != own)
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
        // A type this UI doesn't know: shown, not editable.
        _ => view! { <code class="readonly">{text}</code> }.into_any(),
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
    // Follows the stored root when it changes elsewhere (another client, an agent), so
    // a later path edit doesn't send a stale one.
    Effect::new({
        let parts = parts.clone();
        move |_| root.set(parts().0)
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
    use super::{asset_value, display, is_visible, parse, rgb_part, sections, value_of, with_rgb};
    use jstream_control::protocol::{
        AssetRoot, EnumOption, KindCategory, KindDescriptor, ParamDescriptor, ParamType, Reference,
        Settings, VisibleIf,
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
}
