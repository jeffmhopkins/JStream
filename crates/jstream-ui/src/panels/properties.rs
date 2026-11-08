//! The properties form of a source, with a field for each parameter its kind's
//! descriptor (`kind.describe`) lists, and no code per kind (ADR-0025). The Properties
//! dialog (`source_properties.rs`) and a filter's settings (`filters.rs`) draw it. Fields
//! follow the descriptor's display order, groups, `advanced` and `visible_if` hints.
//! A changed field is checked with the descriptor's own check, then sent as a
//! one-parameter `source.set_settings` patch; "Reset" sends `null` for the default.
//! A capture target parameter (a display or window source's `target`) is a list of the
//! targets of its kind that `capture.targets` reports, read again each time the field
//! gets focus, so a window opened since shows up. A `webcam`'s device is one too, and
//! its format parameters (`capture_format`) are lists of what that device's listed
//! formats offer. The fields of one form share one `capture.targets` answer.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/adr/0025-typed-settings-kind-descriptors.md

use crate::app::SessionContext;
use crate::icons::Icon;
use crate::store::{Studio, query};
use jstream_control::protocol::{
    AssetRoot, CAMERA_CONTROL_AUTO, CameraControlInfo, CaptureFormatInfo, CaptureFormatPart,
    CaptureTargetInfo, CaptureTargetKind, FontInfo, KindDescriptor, Media, ParamDescriptor,
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
/// parameter whose current value differs, or which is itself hidden (a setting under a
/// checkbox under a switch goes when the switch is off).
pub fn is_visible(param: &ParamDescriptor, kind: &KindDescriptor, settings: &Settings) -> bool {
    visible_within(param, kind, settings, kind.params.len())
}

/// [`is_visible`], following at most `depth` more rules, so a cycle of them ends.
fn visible_within(
    param: &ParamDescriptor,
    kind: &KindDescriptor,
    settings: &Settings,
    depth: usize,
) -> bool {
    let Some(rule) = &param.visible_if else {
        return true;
    };
    let current = match kind.param(&rule.param) {
        Some(other) => {
            if depth == 0 || !visible_within(other, kind, settings, depth - 1) {
                return false;
            }
            value_of(other, settings)
        }
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
        ParamType::CaptureFormat {
            part: CaptureFormatPart::Fps,
            ..
        } => {
            let text = text.trim();
            let n: f64 = if text.is_empty() {
                0.0
            } else {
                text.parse().map_err(|_| "must be a number".to_owned())?
            };
            fps_value(n).ok_or_else(|| "must be a finite number".to_owned())?
        }
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

/// The fields for one kind's parameters, showing `settings`: a draft in the Properties
/// dialog, the stored ones in a filter's settings. `commit` gets each checked change, with
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
    // One `capture.targets` answer for every capture field of this form.
    provide_context(CaptureListing::for_kind(&kind));
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
    let input = input_for(
        &param,
        value.clone(),
        send.clone(),
        studio,
        &source_id,
        settings,
    );
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
            {param.warning.clone().map(|w| view! { <p class="field-warning">{w}</p> })}
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
    settings: Signal<Settings>,
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
        ParamType::Asset { accept } if takes_fonts(accept) => {
            font_input(param, value, send, accept)
        }
        ParamType::Asset { accept } => asset_input(param, value, send, accept),
        ParamType::CaptureTarget { kind, .. } => capture_target_input(param, value, send, *kind),
        ParamType::CaptureFormat { part, target } => {
            capture_format_input(param, value, send, *part, target.clone(), settings)
        }
        ParamType::CameraControls { target } => {
            camera_controls_input(value, send, target.clone(), settings)
        }
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
        CaptureTargetKind::Device => "First device listed",
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
            CaptureTargetKind::Device => "Device not connected",
            _ => "Window not open",
        };
        choices.push((current.to_owned(), format!("{gone} ({current})")));
    }
    choices
}

/// The `capture.targets` answer the capture fields of one form share: `None` until the
/// first answer, then the targets, or why they couldn't be listed.
/// It also knows the kind's `capture_format` parameters, so each one reads the parts the
/// others chose.
#[derive(Clone, Copy)]
struct CaptureListing {
    listed: RwSignal<Option<Result<Vec<CaptureTargetInfo>, String>>>,
    asked: RwSignal<bool>,
    /// Each `capture_format` parameter as (device parameter, part, its own ID).
    parts: StoredValue<Vec<(String, CaptureFormatPart, String)>>,
}

impl CaptureListing {
    fn new() -> Self {
        Self::for_params(&[])
    }

    fn for_kind(kind: &KindDescriptor) -> Self {
        Self::for_params(&kind.params)
    }

    fn for_params(params: &[ParamDescriptor]) -> Self {
        let parts = params
            .iter()
            .filter_map(|p| match &p.ty {
                ParamType::CaptureFormat { part, target } => {
                    Some((target.clone(), *part, p.id.clone()))
                }
                _ => None,
            })
            .collect();
        Self {
            listed: RwSignal::new(None),
            asked: RwSignal::new(false),
            parts: StoredValue::new(parts),
        }
    }

    /// The ID of the parameter picking `part` of the device that `target` names.
    fn part_id(self, target: &str, part: CaptureFormatPart) -> Option<String> {
        self.parts.with_value(|parts| {
            parts
                .iter()
                .find(|(t, p, _)| t == target && *p == part)
                .map(|(_, _, id)| id.clone())
        })
    }

    /// The form's listing, or a new one outside a form.
    fn shared() -> Self {
        use_context::<Self>().unwrap_or_else(Self::new)
    }

    /// Asks for the list unless a field of the form already did.
    fn first(self) {
        if !self.asked.get_untracked() {
            self.asked.set(true);
            self.refresh();
        }
    }

    /// Asks for the list again. A failed refresh keeps the last list, so an open select
    /// stays one. The form may have closed meanwhile.
    fn refresh(self) {
        let Some(session) = expect_context::<SessionContext>().get() else {
            return;
        };
        let listed = self.listed;
        spawn_local(async move {
            let got = match query(&session, Query::CaptureTargets).await {
                Ok(QueryResult::CaptureTargets { targets }) => Ok(targets),
                Ok(other) => Err(format!("unexpected {} result", other.name())),
                Err(e) => Err(e.to_string()),
            };
            listed.try_update(|l| {
                if got.is_ok() || !matches!(l, Some(Ok(_))) {
                    *l = Some(got);
                }
            });
        });
    }
}

/// A frame rate as a setting's value: `0` stays a whole number, as the default is.
fn fps_value(fps: f64) -> Option<Value> {
    if fps == 0.0 {
        return Some(Value::from(0));
    }
    serde_json::Number::from_f64(fps).map(Value::Number)
}

/// A frame rate as a list shows it and keys it: at most two decimals, no trailing zeros.
pub fn fps_text(fps: f64) -> String {
    let text = format!("{fps:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// The formats `capture.targets` lists for the device `device` names: the first device
/// listed when `device` is empty, as a `webcam` with an empty `device` captures. Empty
/// when the device isn't listed.
pub fn device_formats<'t>(
    targets: &'t [CaptureTargetInfo],
    device: &str,
) -> &'t [CaptureFormatInfo] {
    let mut devices = targets
        .iter()
        .filter(|t| t.kind == CaptureTargetKind::Device);
    let found = if device.is_empty() {
        devices.next()
    } else {
        devices.find(|t| t.target == device)
    };
    found.map_or(&[], |t| t.formats.as_slice())
}

/// The controls of the device that `device` names, as [`device_formats`] finds it.
pub fn device_controls<'t>(
    targets: &'t [CaptureTargetInfo],
    device: &str,
) -> &'t [CameraControlInfo] {
    let mut devices = targets
        .iter()
        .filter(|t| t.kind == CaptureTargetKind::Device);
    let found = if device.is_empty() {
        devices.next()
    } else {
        devices.find(|t| t.target == device)
    };
    found.map_or(&[], |t| t.controls.as_slice())
}

/// The label a camera control's name shows as: `white_balance` is "White balance".
pub fn control_label(name: &str) -> String {
    let words = name.replace('_', " ");
    let mut chars = words.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// A `camera_controls` value with the control `name` set to `setting`: a number, or
/// `"auto"`. `None` removes the entry, so the device keeps what it has.
pub fn with_control(value: &Value, name: &str, setting: Option<Value>) -> Value {
    let mut object = value.as_object().cloned().unwrap_or_default();
    match setting {
        Some(setting) => {
            object.insert(name.to_owned(), setting);
        }
        None => {
            object.remove(name);
        }
    }
    Value::Object(object)
}

/// What one control of a `camera_controls` value shows: whether the device sets it
/// itself, and the number the slider holds. A control the value doesn't name shows as
/// the device reports it.
pub fn control_shown(value: &Value, control: &CameraControlInfo) -> (bool, i32) {
    match value.get(&control.name) {
        Some(Value::String(s)) if s == CAMERA_CONTROL_AUTO => (true, control.value),
        Some(v) => match v.as_i64().and_then(|n| i32::try_from(n).ok()) {
            Some(n) => (false, n),
            None => (control.auto == Some(true), control.value),
        },
        None => (control.auto == Some(true), control.value),
    }
}

/// The label a `pixel_format` value shows as.
pub fn pixel_format_label(value: &str) -> String {
    match value {
        "mjpeg" => "MJPEG".into(),
        "yuy2" => "YUY2".into(),
        "nv12" => "NV12".into(),
        "bgra" => "BGRA".into(),
        "rgba" => "RGBA".into(),
        other => other.to_uppercase(),
    }
}

/// The choices of a `capture_format` list for `part`, as (value, label): "Auto" first
/// (the engine's choice), then each value `formats` offers with the other parts the
/// settings chose (`resolution`, `fps` and `pixel_format`, empty or 0 for any): sizes
/// largest first, rates fastest first, pixel formats in the device's order. A `current`
/// value no format offers stays a choice, marked, so the list still shows what the
/// source asks.
pub fn format_choices(
    part: CaptureFormatPart,
    formats: &[CaptureFormatInfo],
    resolution: &str,
    fps: f64,
    pixel_format: &str,
) -> Vec<(String, String)> {
    let size = |f: &CaptureFormatInfo| format!("{}x{}", f.width, f.height);
    let fps_matches = |f: &CaptureFormatInfo| fps == 0.0 || (f.fps - fps).abs() < 0.005;
    let matching = |f: &&CaptureFormatInfo| match part {
        CaptureFormatPart::Resolution => {
            fps_matches(f) && (pixel_format.is_empty() || f.pixel_format == pixel_format)
        }
        CaptureFormatPart::Fps => {
            (resolution.is_empty() || size(f) == resolution)
                && (pixel_format.is_empty() || f.pixel_format == pixel_format)
        }
        _ => (resolution.is_empty() || size(f) == resolution) && fps_matches(f),
    };
    let offered: Vec<&CaptureFormatInfo> = formats.iter().filter(matching).collect();
    let mut choices = vec![(String::new(), "Auto".to_owned())];
    let (current, values): (String, Vec<(String, String)>) = match part {
        CaptureFormatPart::Resolution => {
            let mut sizes: Vec<(u32, u32)> = offered.iter().map(|f| (f.width, f.height)).collect();
            sizes.sort_by_key(|&(w, h)| std::cmp::Reverse(u64::from(w) * u64::from(h)));
            sizes.dedup();
            (
                resolution.to_owned(),
                sizes
                    .into_iter()
                    .map(|(w, h)| (format!("{w}x{h}"), format!("{w}×{h}")))
                    .collect(),
            )
        }
        CaptureFormatPart::Fps => {
            let mut rates: Vec<String> = Vec::new();
            let mut sorted: Vec<f64> = offered.iter().map(|f| f.fps).collect();
            sorted.sort_by(|a, b| b.total_cmp(a));
            for rate in sorted {
                let text = fps_text(rate);
                if !rates.contains(&text) {
                    rates.push(text);
                }
            }
            (
                if fps == 0.0 {
                    String::new()
                } else {
                    fps_text(fps)
                },
                rates
                    .into_iter()
                    .map(|r| (r.clone(), format!("{r} fps")))
                    .collect(),
            )
        }
        _ => {
            let mut kinds: Vec<String> = Vec::new();
            for f in &offered {
                if !kinds.contains(&f.pixel_format) {
                    kinds.push(f.pixel_format.clone());
                }
            }
            (
                pixel_format.to_owned(),
                kinds
                    .into_iter()
                    .map(|k| {
                        let label = pixel_format_label(&k);
                        (k, label)
                    })
                    .collect(),
            )
        }
    };
    choices.extend(values);
    if !current.is_empty() && !choices.iter().any(|(value, _)| *value == current) {
        let label = match part {
            CaptureFormatPart::Resolution => current.replace('x', "×"),
            CaptureFormatPart::Fps => format!("{current} fps"),
            _ => pixel_format_label(&current),
        };
        choices.push((current.clone(), format!("{label} (not offered)")));
    }
    choices
}

/// One part of a device's format, picked from what `capture.targets` lists for the
/// device that parameter `target` names, given the other parts chosen. Until the list
/// arrives, and when the engine can't list it, it is the value as text.
fn capture_format_input(
    param: &ParamDescriptor,
    value: impl Fn() -> Value + Clone + Send + Sync + 'static,
    send: impl Fn(Result<Value, String>) + Clone + Send + Sync + 'static,
    part: CaptureFormatPart,
    target: String,
    settings: Signal<Settings>,
) -> AnyView {
    let listing = CaptureListing::shared();
    let listed = listing.listed;
    listing.first();
    let ids = [
        CaptureFormatPart::Resolution,
        CaptureFormatPart::Fps,
        CaptureFormatPart::PixelFormat,
    ]
    .map(|p| listing.part_id(&target, p));
    let choices = move || {
        listed.with(|l| match l {
            Some(Ok(targets)) => settings.with(|s| {
                let get = |id: &Option<String>| id.as_deref().and_then(|id| s.get(id));
                let text = |id: &Option<String>| get(id).map(display).unwrap_or_default();
                let fps = get(&ids[1]).and_then(Value::as_f64).unwrap_or(0.0);
                Some(format_choices(
                    part,
                    device_formats(targets, &s.get(&target).map(display).unwrap_or_default()),
                    &text(&ids[0]),
                    fps,
                    &text(&ids[2]),
                ))
            }),
            _ => None,
        })
    };
    let current = {
        let value = value.clone();
        move || match part {
            CaptureFormatPart::Fps => {
                let fps = value().as_f64().unwrap_or(0.0);
                if fps == 0.0 {
                    String::new()
                } else {
                    fps_text(fps)
                }
            }
            _ => display(&value()),
        }
    };
    let p = param.clone();
    let on_change = move |event: leptos::ev::Event| send(parse(&p, &event_target_value(&event)));
    let listed_now = Memo::new(move |_| listed.with(|l| matches!(l, Some(Ok(_)))));
    let body = move || {
        if listed_now.get() {
            let choices = choices.clone();
            let current_now = current.clone();
            let selected = {
                let current = current.clone();
                move || {
                    listed.track();
                    settings.track();
                    current()
                }
            };
            view! {
                <select class="capture-format" prop:value=selected.clone()
                    on:focus=move |_| listing.refresh() on:change=on_change.clone()>
                    {move || {
                        // Each option says whether it is the current one, as a capture
                        // target's do: `prop:value` can run before the options exist (#1371).
                        let now = current_now();
                        choices()
                            .unwrap_or_default()
                            .into_iter()
                            .map(|(v, label)| {
                                let on = v == now;
                                view! { <option value=v selected=on>{label}</option> }
                            })
                            .collect_view()
                    }}
                </select>
            }
            .into_any()
        } else {
            view! {
                <input type="text" prop:value=current.clone()
                    on:change=on_change.clone() />
            }
            .into_any()
        }
    };
    view! { {body} }.into_any()
}

/// A webcam's camera controls: a slider and a number for each control the device that
/// `target` picks lists in `capture.targets`, with an Auto box for one the device can
/// set itself. Each change sends the whole object with that control's entry set.
/// Until the list arrives, and for a device with no controls, it says so.
fn camera_controls_input(
    value: impl Fn() -> Value + Clone + Send + Sync + 'static,
    send: impl Fn(Result<Value, String>) + Clone + Send + Sync + 'static,
    target: String,
    settings: Signal<Settings>,
) -> AnyView {
    let listing = CaptureListing::shared();
    let listed = listing.listed;
    listing.first();
    let controls = move || {
        listed.with(|l| match l {
            Some(Ok(targets)) => settings.with(|s| {
                let device = s.get(&target).map(display).unwrap_or_default();
                Some(device_controls(targets, &device).to_vec())
            }),
            Some(Err(_)) => Some(Vec::new()),
            None => None,
        })
    };
    let body = move || match controls() {
        None => view! { <span class="hint">"Listing the camera's controls…"</span> }.into_any(),
        Some(list) if list.is_empty() => {
            view! { <span class="hint">"This camera lists no controls."</span> }.into_any()
        }
        Some(list) => list
            .into_iter()
            .map(|control| camera_control_row(control, value.clone(), send.clone()))
            .collect_view()
            .into_any(),
    };
    view! { <div class="camera-controls">{body}</div> }.into_any()
}

/// One control's row in [`camera_controls_input`].
fn camera_control_row(
    control: CameraControlInfo,
    value: impl Fn() -> Value + Clone + Send + Sync + 'static,
    send: impl Fn(Result<Value, String>) + Clone + Send + Sync + 'static,
) -> AnyView {
    let shown = {
        let (value, control) = (value.clone(), control.clone());
        move || control_shown(&value(), &control)
    };
    let number = {
        let shown = shown.clone();
        move || shown().1.to_string()
    };
    let is_auto = {
        let shown = shown.clone();
        move || shown().0
    };
    let on_number = {
        let (value, send, name) = (value.clone(), send.clone(), control.name.clone());
        let (min, max) = (control.min, control.max);
        move |event: leptos::ev::Event| {
            let text = event_target_value(&event);
            let got = match text.trim().parse::<i32>() {
                Ok(n) if (min..=max).contains(&n) => {
                    Ok(with_control(&value(), &name, Some(Value::from(n))))
                }
                Ok(_) => Err(format!("{name} must be {min} to {max}")),
                Err(_) => Err(format!("{name} must be a whole number")),
            };
            send(got)
        }
    };
    let auto_box = control.auto.map(|_| {
        let (value, send, name) = (value.clone(), send.clone(), control.name.clone());
        let is_auto = is_auto.clone();
        let number = number.clone();
        view! {
            <label class="camera-control-auto">
                <input type="checkbox" prop:checked=is_auto
                    on:change=move |event| {
                        let setting = if event_target_checked(&event) {
                            Value::from(CAMERA_CONTROL_AUTO)
                        } else {
                            Value::from(number().parse::<i32>().unwrap_or_default())
                        };
                        send(Ok(with_control(&value(), &name, Some(setting))))
                    } />
                "Auto"
            </label>
        }
    });
    let (min, max, step) = (control.min, control.max, control.step.max(1));
    view! {
        <div class="camera-control">
            <span class="camera-control-name">{control_label(&control.name)}</span>
            <input type="range" min=min.to_string() max=max.to_string() step=step.to_string()
                prop:value=number.clone() prop:disabled=is_auto.clone() on:change=on_number.clone() />
            <input type="number" min=min.to_string() max=max.to_string() step=step.to_string()
                prop:value=number prop:disabled=is_auto on:change=on_number />
            {auto_box}
        </div>
    }
    .into_any()
}

/// A display or window to capture, picked from what `capture.targets` lists now. Until
/// the list arrives, and when the engine can't list targets, it is the ID as text.
fn capture_target_input(
    param: &ParamDescriptor,
    value: impl Fn() -> Value + Clone + Send + Sync + 'static,
    send: impl Fn(Result<Value, String>) + Clone + Send + Sync + 'static,
    kind: CaptureTargetKind,
) -> AnyView {
    let listing = CaptureListing::shared();
    let listed = listing.listed;
    let refresh = move || listing.refresh();
    listing.first();
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
            let current_now = current.clone();
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
                        // Each option says whether it is the current one: `prop:value` can
                        // run before the options exist, and then selects nothing (#1371).
                        let now = current_now();
                        choices()
                            .into_iter()
                            .map(|(v, label)| {
                                let on = v == now;
                                view! { <option value=v selected=on>{label}</option> }
                            })
                            .collect_view()
                    }}
                </select>
            }
            .into_any()
        }
        Err(reason) => view! {
            <input type="text" maxlength=max_len.to_string() title=reason
                prop:value=current.clone() on:focus=move |_| refresh()
                on:change=on_text.clone() />
        }
        .into_any(),
    };
    view! { {body} }.into_any()
}

/// Whether an `asset` parameter takes font files only, so the form offers the installed
/// fonts (ADR-0092): its `accept` names `ttf` or `otf`, and nothing but font types.
pub fn takes_fonts(accept: &[String]) -> bool {
    let font = |e: &String| {
        ["ttf", "otf", "ttc"]
            .iter()
            .any(|f| e.eq_ignore_ascii_case(f))
    };
    accept
        .iter()
        .any(|e| ["ttf", "otf"].iter().any(|f| e.eq_ignore_ascii_case(f)))
        && accept.iter().all(font)
}

/// The font list's value for the bundled font (no file).
pub const BUNDLED_FONT: &str = "";
/// The font list's value for a file it doesn't list, picked by root and path.
pub const OTHER_FONT: &str = "other";

/// The choices of a font list, as (value, label), and which one `current` is. First the
/// bundled font, then each font `fonts` lists by family and style (a value `font:` and
/// its path), then "Other file…". A stored file the list doesn't name selects "Other
/// file…", so its root and path show.
pub fn font_choices(fonts: &[FontInfo], current: &Value) -> (Vec<(String, String)>, String) {
    let mut choices = vec![(BUNDLED_FONT.to_owned(), "Bundled font".to_owned())];
    for font in fonts {
        choices.push((
            format!("font:{}", font.path),
            format!("{} {}", font.remote_family, font.remote_style),
        ));
    }
    choices.push((OTHER_FONT.to_owned(), "Other file…".to_owned()));
    let selected = match Reference::from_value(current) {
        _ if current.is_null() => BUNDLED_FONT.to_owned(),
        Some(Reference::Asset {
            root: AssetRoot::Absolute,
            path,
        }) if fonts.iter().any(|f| f.path == path) => format!("font:{path}"),
        _ => OTHER_FONT.to_owned(),
    };
    (choices, selected)
}

/// A font setting: a list of the installed fonts (`font.list`, ADR-0092), with the
/// bundled font first and "Other file…" last, which shows the root and path of any
/// other file. Until the list arrives, or if the engine can't list them, only the root
/// and path show.
fn font_input(
    param: &ParamDescriptor,
    value: impl Fn() -> Value + Clone + Send + Sync + 'static,
    send: impl Fn(Result<Value, String>) + Clone + Send + Sync + 'static,
    accept: &[String],
) -> AnyView {
    let session = expect_context::<SessionContext>();
    let listed = RwSignal::new(None::<Result<Vec<FontInfo>, String>>);
    if let Some(session) = session.get() {
        spawn_local(async move {
            let got = match query(&session, Query::FontList).await {
                Ok(QueryResult::FontList { fonts }) => Ok(fonts),
                Ok(other) => Err(format!("unexpected {} result", other.name())),
                Err(e) => Err(e.to_string()),
            };
            listed.try_set(Some(got));
        });
    }
    // "Other file…" picked with a listed font or none stored: the root and path show
    // until another choice is made.
    let other = RwSignal::new(false);
    let choices = {
        let value = value.clone();
        Memo::new(move |_| {
            listed.with(|l| match l {
                Some(Ok(fonts)) => Some(font_choices(fonts, &value())),
                _ => None,
            })
        })
    };
    let p = param.clone();
    let on_pick = {
        let send = send.clone();
        move |event: leptos::ev::Event| {
            let picked = event_target_value(&event);
            if picked == OTHER_FONT {
                other.set(true);
                return;
            }
            other.set(false);
            let value = match picked.strip_prefix("font:") {
                Some(path) => asset_value(&p, AssetRoot::Absolute, path),
                None => asset_value(&p, AssetRoot::Absolute, ""),
            };
            send(value);
        }
    };
    let file = asset_input(param, value, send, accept);
    let shows_file = move || {
        other.get()
            || choices.with(|c| {
                c.as_ref()
                    .is_none_or(|(_, selected)| selected == OTHER_FONT)
            })
    };
    let reason = move || match listed.get() {
        Some(Err(reason)) => Some(format!("Can't list the installed fonts: {reason}")),
        _ => None,
    };
    view! {
        <Show when=move || choices.with(Option::is_some)>
            <select class="font-pick"
                prop:value=move || {
                    choices.with(|c| match c {
                        Some(_) if other.get() => OTHER_FONT.to_owned(),
                        Some((_, selected)) => selected.clone(),
                        None => String::new(),
                    })
                }
                on:change=on_pick.clone()>
                {move || {
                    choices
                        .get()
                        .map(|(list, _)| list)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(v, label)| view! { <option value=v>{label}</option> })
                        .collect_view()
                }}
            </select>
        </Show>
        <span class="font-file" class:hidden=move || !shows_file() title=reason>{file}</span>
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
    let on_root = {
        let send = send.clone();
        move |event: leptos::ev::Event| {
            let picked = root_from(&event_target_value(&event));
            root.set(picked);
            let current = parts().1;
            if !current.is_empty() {
                send(asset_value(&p, picked, &current));
            }
        }
    };
    let hint = (!accept.is_empty()).then(|| accept.join(", "));
    let extensions = accept.to_vec();
    let open_project = use_context::<crate::project::OpenProject>();
    let dialog_title = param.label.clone();
    let p = param.clone();
    let send = send.clone();
    view! {
        <select class="asset-root" prop:value=move || root_name(root.get()) on:change=on_root>
            <option value="collection">"Collection folder"</option>
            <option value="app">"App assets"</option>
            <option value="absolute">"Absolute path"</option>
        </select>
        <input type="text" placeholder=hint.unwrap_or_else(|| "path".into()) prop:value=path.clone() on:change=on_path />
        // App assets are the app's own files, not something a person picks on disk.
        {crate::browse::browse_button(
            crate::browse::BrowseKind::Open,
            extensions,
            dialog_title,
            move || root.get() != AssetRoot::App,
            {
                let path = path.clone();
                move || crate::browse::file_name(&path())
            },
            {
                let path = path.clone();
                move || {
                    let collection = crate::project::collection_folder(open_project);
                    crate::browse::start_folder(
                        root.get_untracked(),
                        &path(),
                        collection.as_deref(),
                    )
                }
            },
            move |picked: String| {
                let collection = crate::project::collection_folder(open_project);
                let (picked_root, path) = crate::browse::place_asset(collection.as_deref(), &picked);
                let value = asset_value(&p, picked_root, &path);
                if value.is_ok() {
                    root.set(picked_root);
                }
                send(value);
            },
        )}
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::{
        BUNDLED_FONT, OTHER_FONT, asset_value, control_label, control_shown, device_controls,
        device_formats, display, draft_patch, font_choices, format_choices, fps_text, is_visible,
        offers, parse, rgb_part, sections, takes_fonts, target_choices, value_of, with_control,
        with_rgb,
    };
    use jstream_control::protocol::{
        AssetRoot, CAMERA_CONTROL_AUTO, CameraControlInfo, CaptureFormatInfo, CaptureFormatPart,
        CaptureTargetInfo, CaptureTargetKind, EnumOption, FontInfo, KindCategory, KindDescriptor,
        ParamDescriptor, ParamType, PixelSize, Reference, Settings, VisibleIf,
    };
    use serde_json::{Value, json};

    fn param(id: &str, ty: ParamType, default: Value) -> ParamDescriptor {
        ParamDescriptor {
            warning: None,
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
    fn a_setting_under_a_hidden_checkbox_is_hidden() {
        let on = |id: &str| VisibleIf {
            param: id.into(),
            equals: json!(true),
        };
        let switch = param("api", ParamType::Bool, json!(false));
        let mut check = param("audio", ParamType::Bool, json!(false));
        check.visible_if = Some(on("api"));
        let mut bus = param("bus", ParamType::Bus, Value::Null);
        bus.visible_if = Some(on("audio"));
        let k = kind(vec![switch, check, bus.clone()]);
        let mut settings = Settings::new();
        settings.insert("audio", json!(true));
        assert!(!is_visible(&bus, &k, &settings), "the switch is off");
        settings.insert("api", json!(true));
        assert!(is_visible(&bus, &k, &settings));
        // A cycle of rules ends.
        let mut a = param("a", ParamType::Bool, json!(true));
        a.visible_if = Some(on("b"));
        let mut b = param("b", ParamType::Bool, json!(true));
        b.visible_if = Some(on("a"));
        let k = kind(vec![a.clone(), b]);
        assert!(!is_visible(&a, &k, &Settings::new()));
    }

    #[test]
    fn only_a_font_only_asset_offers_the_installed_fonts() {
        let accept = |a: &[&str]| a.iter().map(|e| (*e).to_owned()).collect::<Vec<_>>();
        assert!(takes_fonts(&accept(&["ttf", "otf", "ttc"])));
        assert!(takes_fonts(&accept(&["OTF"])));
        assert!(!takes_fonts(&accept(&["ttc"])), "no single-font type");
        assert!(!takes_fonts(&accept(&["ttf", "png"])));
        assert!(!takes_fonts(&[]));
    }

    #[test]
    fn the_font_list_selects_the_stored_font() {
        let font = |family: &str, style: &str, path: &str| FontInfo {
            remote_family: family.into(),
            remote_style: style.into(),
            path: path.into(),
        };
        let fonts = [
            font("Inter", "Bold", "/f/Inter-Bold.otf"),
            font("Inter", "Regular", "/f/Inter.otf"),
        ];
        let asset = |root, path: &str| {
            Reference::Asset {
                root,
                path: path.into(),
            }
            .to_value()
        };
        let (choices, selected) = font_choices(&fonts, &Value::Null);
        let labels: Vec<_> = choices.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(
            labels,
            ["Bundled font", "Inter Bold", "Inter Regular", "Other file…"]
        );
        assert_eq!(choices[2].0, "font:/f/Inter.otf");
        assert_eq!(selected, BUNDLED_FONT);
        let (_, selected) = font_choices(&fonts, &asset(AssetRoot::Absolute, "/f/Inter.otf"));
        assert_eq!(selected, "font:/f/Inter.otf");
        // A file the list doesn't name, or one under another root, shows its root and path.
        let (_, selected) = font_choices(&fonts, &asset(AssetRoot::Absolute, "/x/Mine.ttf"));
        assert_eq!(selected, OTHER_FONT);
        let (_, selected) = font_choices(&fonts, &asset(AssetRoot::Collection, "/f/Inter.otf"));
        assert_eq!(selected, OTHER_FONT);
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
            formats: Vec::new(),
            controls: Vec::new(),
        }
    }

    #[test]
    fn a_camera_control_shows_its_setting_or_what_the_device_reports() {
        let exposure = CameraControlInfo {
            name: "exposure".into(),
            min: -11,
            max: -2,
            step: 1,
            default: -6,
            value: -5,
            auto: Some(true),
        };
        let mut cam = target(CaptureTargetKind::Device, "cam", "Cam");
        cam.controls = vec![exposure.clone()];
        let listed = [target(CaptureTargetKind::Display, "d1", "Display 1"), cam];
        assert_eq!(device_controls(&listed, "").len(), 1);
        assert_eq!(device_controls(&listed, "cam")[0].name, "exposure");
        assert!(device_controls(&listed, "gone").is_empty());
        assert_eq!(control_label("white_balance"), "White balance");
        // Not named: as the device reports it.
        assert_eq!(control_shown(&json!({}), &exposure), (true, -5));
        let set = with_control(&json!({"gain": 3}), "exposure", Some(json!(-8)));
        assert_eq!(set, json!({"gain": 3, "exposure": -8}));
        assert_eq!(control_shown(&set, &exposure), (false, -8));
        let auto = with_control(&set, "exposure", Some(json!(CAMERA_CONTROL_AUTO)));
        assert_eq!(control_shown(&auto, &exposure), (true, -5));
        assert_eq!(with_control(&auto, "exposure", None), json!({"gain": 3}));
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

    fn format(width: u32, height: u32, fps: f64, pixel_format: &str) -> CaptureFormatInfo {
        CaptureFormatInfo {
            width,
            height,
            fps,
            pixel_format: pixel_format.into(),
        }
    }

    fn webcam() -> Vec<CaptureTargetInfo> {
        let mut cam = target(CaptureTargetKind::Device, "cam", "Webcam");
        cam.formats = vec![
            format(1280, 720, 30.0, "yuy2"),
            format(1920, 1080, 30.0, "mjpeg"),
            format(1920, 1080, 60.0, "mjpeg"),
            format(1280, 720, 29.97, "mjpeg"),
        ];
        let mut card = target(CaptureTargetKind::Device, "card", "Capture card");
        card.formats = vec![format(3840, 2160, 30.0, "nv12")];
        vec![
            target(CaptureTargetKind::Display, "d1", "Display 1"),
            cam,
            card,
        ]
    }

    #[test]
    fn a_device_lists_with_its_own_labels_and_keeps_an_unplugged_one() {
        let listed = webcam();
        let values = |choices: Vec<(String, String)>| {
            choices
                .into_iter()
                .map(|(v, l)| format!("{v}={l}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            values(target_choices(CaptureTargetKind::Device, &listed, "")),
            ["=First device listed", "cam=Webcam", "card=Capture card"]
        );
        assert_eq!(
            values(target_choices(CaptureTargetKind::Device, &listed[..1], "x"))[1],
            "x=Device not connected (x)"
        );
        // An empty device is the first listed, as the engine captures.
        assert_eq!(device_formats(&listed, "").len(), 4);
        assert_eq!(device_formats(&listed, "card").len(), 1);
        assert!(device_formats(&listed, "gone").is_empty());
    }

    #[test]
    fn format_lists_offer_what_the_other_parts_leave() {
        let listed = webcam();
        let formats = device_formats(&listed, "cam");
        let values = |choices: Vec<(String, String)>| {
            choices.into_iter().map(|(v, _)| v).collect::<Vec<_>>()
        };
        // Sizes largest first, with Auto first.
        assert_eq!(
            values(format_choices(
                CaptureFormatPart::Resolution,
                formats,
                "",
                0.0,
                ""
            )),
            ["", "1920x1080", "1280x720"]
        );
        // Rates for the chosen size, fastest first.
        assert_eq!(
            values(format_choices(
                CaptureFormatPart::Fps,
                formats,
                "1920x1080",
                0.0,
                ""
            )),
            ["", "60", "30"]
        );
        assert_eq!(
            values(format_choices(
                CaptureFormatPart::Fps,
                formats,
                "1280x720",
                0.0,
                ""
            )),
            ["", "30", "29.97"]
        );
        // Pixel formats for the chosen size and rate, in the device's order.
        assert_eq!(
            values(format_choices(
                CaptureFormatPart::PixelFormat,
                formats,
                "1280x720",
                30.0,
                ""
            )),
            ["", "yuy2"]
        );
        assert_eq!(
            format_choices(CaptureFormatPart::PixelFormat, formats, "", 0.0, "")[1].1,
            "YUY2"
        );
        // A value the device doesn't offer stays, marked.
        let kept = format_choices(CaptureFormatPart::Resolution, formats, "640x480", 0.0, "");
        assert_eq!(
            kept.last().unwrap(),
            &("640x480".into(), "640×480 (not offered)".into())
        );
        let kept = format_choices(CaptureFormatPart::Fps, formats, "1920x1080", 25.0, "");
        assert_eq!(kept.last().unwrap().0, "25");
    }

    #[test]
    fn a_frame_rate_parses_as_a_number_and_auto_as_zero() {
        let fps = param(
            "fps",
            ParamType::CaptureFormat {
                part: CaptureFormatPart::Fps,
                target: "device".into(),
            },
            json!(0),
        );
        assert_eq!(parse(&fps, ""), Ok(json!(0)));
        assert_eq!(parse(&fps, "29.97"), Ok(json!(29.97)));
        assert!(parse(&fps, "fast").is_err());
        assert!(parse(&fps, "5000").is_err());
        assert_eq!(fps_text(30.0), "30");
        assert_eq!(fps_text(29.97), "29.97");
        assert_eq!(fps_text(7.5), "7.5");
        let resolution = param(
            "resolution",
            ParamType::CaptureFormat {
                part: CaptureFormatPart::Resolution,
                target: "device".into(),
            },
            json!(""),
        );
        assert_eq!(parse(&resolution, "1920x1080"), Ok(json!("1920x1080")));
        assert!(parse(&resolution, "big").is_err());
    }
}
