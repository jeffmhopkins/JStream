//! The Transitions panel: in studio mode the scene on preview, the scene on program and
//! the transition that takes preview to program; then the collection's transition
//! presets, in a picker with the picked one's editor, and each scene's overrides
//! ([ADR-0029](../../../../docs/adr/0029-transitions.md)). Also the scene menus'
//! Transition in and out submenus ([`override_submenu`]).
//!
//! The engine has no studio-mode switch: preview is always there (`studio.set_preview`)
//! and `scene.switch` puts a scene on program with a transition; studio mode is the
//! page's (`Studio::studio_mode`), and only shows or hides the bar. The panel shows which
//! preset a switch will use, following the engine's order (the one the switch names, the
//! new scene's `transition_in`, the old scene's `transition_out`, the default), so the
//! label under the button matches what the engine does.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#transitions

use crate::app::SessionContext;
use crate::panels::number::NumberField;
use crate::panels::outputs::{action, edit};
use crate::session::Session;
use futures_util::StreamExt;
use jstream_control::protocol::{
    Action, CUT, Call, CallResult, Easing, Edit, Event, KindCategory, KindDescriptor,
    MAX_DURATION_MS, ParamDescriptor, ParamType, Query, QueryResult, Rev, SceneId, SceneRole,
    Settings, TransitionPresetId, TransitionPresetInfo, TransitionsInfo, builtin_transition_kinds,
    check_preset_timing,
};
use jstream_control::{Control, ControlError};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;
use std::collections::BTreeSet;
use std::future::Future;
use std::time::Duration;

/// One scene as the panel shows it: its name and its transition overrides. Groups are
/// left out, as the scene list leaves them out.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneRow {
    /// The scene's ID.
    pub scene_id: SceneId,
    /// Its display name.
    pub name: String,
    /// The preset a switch into it uses, unless the switch names one.
    pub transition_in: Option<TransitionPresetId>,
    /// The preset a switch out of it uses, unless the switch or the next scene names one.
    pub transition_out: Option<TransitionPresetId>,
}

/// The scenes, presets and studio state at one revision.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// The revision `state.get` read.
    pub rev: Rev,
    /// The scenes, in display order, without groups.
    pub scenes: Vec<SceneRow>,
    /// The transition presets and the default.
    pub transitions: TransitionsInfo,
    /// The scene on program.
    pub program: Option<SceneId>,
    /// The scene on preview.
    pub preview: Option<SceneId>,
}

impl Snapshot {
    /// The scene with this ID.
    pub fn scene(&self, scene_id: &SceneId) -> Option<&SceneRow> {
        self.scenes.iter().find(|scene| &scene.scene_id == scene_id)
    }

    /// A scene's display name, or its ID when the snapshot doesn't hold it (a group, or
    /// a scene deleted since).
    pub fn scene_name(&self, scene_id: &SceneId) -> String {
        self.scene(scene_id)
            .map_or_else(|| scene_id.to_string(), |scene| scene.name.clone())
    }

    /// The preset with this ID.
    pub fn preset(&self, preset_id: &TransitionPresetId) -> Option<&TransitionPresetInfo> {
        self.transitions
            .presets
            .iter()
            .find(|preset| &preset.preset_id == preset_id)
    }
}

/// Reads the scenes and presets (`state.get`), then which scenes are on program and on
/// preview (`studio.get`).
pub async fn load(control: &dyn Control) -> Result<Snapshot, String> {
    let (rev, scenes, transitions) = match query(control, Query::StateGet).await? {
        QueryResult::StateGet {
            rev,
            scenes,
            transitions,
            ..
        } => (rev, scenes, transitions),
        other => return Err(unexpected("state.get", other.name())),
    };
    let scenes = scenes
        .into_iter()
        .filter(|scene| scene.role == SceneRole::Scene)
        .map(|scene| SceneRow {
            scene_id: scene.scene_id,
            name: scene.name,
            transition_in: scene.transition_in,
            transition_out: scene.transition_out,
        })
        .collect();
    let (program, preview) = match query(control, Query::StudioGet).await? {
        QueryResult::StudioGet {
            program_scene,
            preview_scene,
        } => (program_scene, preview_scene),
        other => return Err(unexpected("studio.get", other.name())),
    };
    Ok(Snapshot {
        rev,
        scenes,
        transitions: *transitions,
        program,
        preview,
    })
}

/// The transition kinds the engine has, with their parameters (`kind.list`, then
/// `kind.describe` for each transition kind). An engine that can't list them (an older
/// one, or the mock) gets the built-in kinds every engine with presets has: `cut`,
/// `fade` and `move`.
pub async fn transition_kinds(control: &dyn Control) -> Vec<KindDescriptor> {
    let Ok(QueryResult::KindList { kinds }) = query(control, Query::KindList).await else {
        return builtin_transition_kinds();
    };
    let mut described = Vec::new();
    for summary in kinds
        .into_iter()
        .filter(|kind| kind.category == KindCategory::Transition)
    {
        let describe = Query::KindDescribe {
            kind_id: summary.kind_id,
        };
        if let Ok(QueryResult::KindDescribe { kind, .. }) = query(control, describe).await {
            described.push(kind);
        }
    }
    if described.is_empty() {
        builtin_transition_kinds()
    } else {
        described
    }
}

/// Sends one query and returns its result.
async fn query(control: &dyn Control, query: Query) -> Result<QueryResult, String> {
    match control.call(Call::Query { query }).await {
        Ok(response) => match response.result {
            CallResult::Query { result } => Ok(result),
            _ => Err("the engine answered a query with something else".into()),
        },
        Err(error) => Err(describe(&error)),
    }
}

/// A failed call, for the panel.
fn describe(error: &ControlError) -> String {
    error.to_string()
}

/// A query answered with another query's result.
fn unexpected(asked: &str, got: &str) -> String {
    format!("the engine answered `{asked}` with `{got}`")
}

/// Why "Transition" can't take preview to program now, or `None` when it can.
pub fn switch_blocker(snapshot: &Snapshot) -> Option<&'static str> {
    match &snapshot.preview {
        None => Some("Put a scene on preview first."),
        Some(preview) if snapshot.program.as_ref() == Some(preview) => {
            Some("That scene is already on program. Put another one on preview.")
        }
        Some(_) => None,
    }
}

/// The `scene.switch` that puts the preview scene on program with `preset`, or with
/// the preset the engine resolves when it is `None`. `None` when
/// [`switch_blocker`] says it can't.
pub fn switch_action(snapshot: &Snapshot, preset: Option<TransitionPresetId>) -> Option<Action> {
    if switch_blocker(snapshot).is_some() {
        return None;
    }
    Some(Action::SceneSwitch {
        scene_id: snapshot.preview.clone()?,
        preset_id: preset,
    })
}

/// The preset "Cut" switches with: the default when it is a cut, else the first `cut`
/// preset. `None` when the collection has none.
pub fn cut_preset(transitions: &TransitionsInfo) -> Option<TransitionPresetId> {
    let is_cut = |preset: &&TransitionPresetInfo| preset.kind == CUT;
    transitions
        .presets
        .iter()
        .filter(is_cut)
        .find(|preset| preset.preset_id == transitions.default)
        .or_else(|| transitions.presets.iter().find(is_cut))
        .map(|preset| preset.preset_id.clone())
}

/// Where the preset a switch uses comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The switch names it.
    Chosen,
    /// The new scene's `transition_in`.
    SceneIn,
    /// The old scene's `transition_out`.
    SceneOut,
    /// The collection's default.
    Default,
}

/// The preset a switch from program to preview will use, and why, in the engine's
/// order: `chosen`, else the preview scene's `transition_in`, else the program scene's
/// `transition_out`, else the default. A name that no preset has is skipped, as a
/// deleted preset is cleared from the scenes.
pub fn expected_preset<'a>(
    snapshot: &'a Snapshot,
    chosen: Option<&TransitionPresetId>,
) -> Option<(&'a TransitionPresetInfo, Reason)> {
    let into = snapshot
        .preview
        .as_ref()
        .and_then(|id| snapshot.scene(id))
        .and_then(|scene| scene.transition_in.as_ref());
    let out_of = snapshot
        .program
        .as_ref()
        .and_then(|id| snapshot.scene(id))
        .and_then(|scene| scene.transition_out.as_ref());
    [
        (chosen, Reason::Chosen),
        (into, Reason::SceneIn),
        (out_of, Reason::SceneOut),
        (Some(&snapshot.transitions.default), Reason::Default),
    ]
    .into_iter()
    .find_map(|(id, reason)| Some((snapshot.preset(id?)?, reason)))
}

/// The line under the button that says which preset the next switch uses.
pub fn expected_label(snapshot: &Snapshot, chosen: Option<&TransitionPresetId>) -> String {
    match expected_preset(snapshot, chosen) {
        None => "No preset: the switch cuts.".into(),
        Some((preset, reason)) => {
            let why = match reason {
                Reason::Chosen => "chosen",
                Reason::SceneIn => "the preview scene's transition in",
                Reason::SceneOut => "the program scene's transition out",
                Reason::Default => "the default",
            };
            format!("Uses {} ({why}).", preset.name)
        }
    }
}

/// The `studio.set_preview` that puts the scene that was on program back on preview
/// after a switch, so the two trade places. `None` when there was no program scene or
/// it is the scene just switched to.
pub fn swap_action(before: Option<SceneId>, switched_to: &SceneId) -> Option<Action> {
    before
        .filter(|scene_id| scene_id != switched_to)
        .map(|scene_id| Action::StudioSetPreview { scene_id })
}

/// The `scene.set_transitions` that sets a scene's two overrides.
pub fn overrides_edit(
    scene_id: SceneId,
    transition_in: Option<TransitionPresetId>,
    transition_out: Option<TransitionPresetId>,
) -> Edit {
    Edit::SceneSetTransitions {
        scene_id,
        transition_in,
        transition_out,
    }
}

/// An unused preset ID made from `name`: lower case, with runs of anything but letters
/// and digits turned into one `-`, and `-2`, `-3`… added when it is taken.
pub fn next_preset_id<'a>(
    existing: impl IntoIterator<Item = &'a TransitionPresetId> + Clone,
    name: &str,
) -> TransitionPresetId {
    let mut base = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            base.push(c);
        } else if !base.is_empty() && !base.ends_with('-') {
            base.push('-');
        }
    }
    let base = match base.trim_end_matches('-') {
        "" => "transition".to_owned(),
        trimmed => trimmed.to_owned(),
    };
    let taken = |id: &str| existing.clone().into_iter().any(|t| t.as_str() == id);
    if !taken(&base) {
        return TransitionPresetId::new(base);
    }
    (2_u32..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken(id))
        .map_or_else(|| TransitionPresetId::new(base), TransitionPresetId::new)
}

/// Every easing, in the order the form offers them.
pub const EASINGS: [Easing; 4] = [
    Easing::EaseInOut,
    Easing::EaseOut,
    Easing::Linear,
    Easing::Spring,
];

/// An easing's wire name, which the form uses as its option value.
pub fn easing_value(easing: Easing) -> &'static str {
    match easing {
        Easing::EaseInOut => "ease_in_out",
        Easing::EaseOut => "ease_out",
        Easing::Linear => "linear",
        Easing::Spring => "spring",
    }
}

/// An easing's label.
pub fn easing_label(easing: Easing) -> &'static str {
    match easing {
        Easing::EaseInOut => "Ease in and out",
        Easing::EaseOut => "Ease out",
        Easing::Linear => "Linear",
        Easing::Spring => "Spring",
    }
}

/// The easing an option value names.
pub fn parse_easing(value: &str) -> Option<Easing> {
    EASINGS
        .into_iter()
        .find(|&easing| easing_value(easing) == value)
}

/// A kind's display name, or its ID when the engine didn't describe it.
pub fn kind_name(kinds: &[KindDescriptor], kind_id: &str) -> String {
    kinds
        .iter()
        .find(|kind| kind.kind_id == kind_id)
        .map_or_else(|| kind_id.to_owned(), |kind| kind.name.clone())
}

/// Every parameter of `kind` at its default.
pub fn default_settings(kind: &KindDescriptor) -> Settings {
    kind.params
        .iter()
        .map(|param| (param.id.clone(), param.default.clone()))
        .collect()
}

/// A preset as the form edits it. The duration is the text in its field.
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    /// The display name.
    pub name: String,
    /// The transition kind's ID.
    pub kind: String,
    /// The duration in milliseconds, as typed.
    pub duration: String,
    /// The easing.
    pub easing: Easing,
    /// The kind's settings.
    pub settings: Settings,
}

impl Draft {
    /// A new preset: a 300 ms ease-in-out fade, at the kind's defaults.
    pub fn new(kinds: &[KindDescriptor]) -> Draft {
        let mut draft = Draft {
            name: String::new(),
            kind: String::new(),
            duration: "300".into(),
            easing: Easing::EaseInOut,
            settings: Settings::new(),
        };
        let kind = kinds
            .iter()
            .find(|kind| kind.kind_id == jstream_control::protocol::FADE)
            .or_else(|| kinds.first())
            .map_or(jstream_control::protocol::FADE, |kind| {
                kind.kind_id.as_str()
            });
        draft.set_kind(kinds, kind);
        draft
    }

    /// An existing preset, as it is.
    pub fn from_preset(preset: &TransitionPresetInfo) -> Draft {
        Draft {
            name: preset.name.clone(),
            kind: preset.kind.clone(),
            duration: preset.duration_ms.to_string(),
            easing: preset.easing,
            settings: preset.settings.clone(),
        }
    }

    /// Changes the kind. The settings start over from the new kind's defaults, as the
    /// engine's do.
    pub fn set_kind(&mut self, kinds: &[KindDescriptor], kind_id: &str) {
        if self.kind == kind_id {
            return;
        }
        self.kind = kind_id.to_owned();
        self.settings = kinds
            .iter()
            .find(|kind| kind.kind_id == kind_id)
            .map(default_settings)
            .unwrap_or_default();
    }
}

/// The duration typed in the form, in milliseconds.
pub fn parse_duration(typed: &str) -> Result<u32, String> {
    let duration: u32 = typed
        .trim()
        .parse()
        .map_err(|_| "The duration is a whole number of milliseconds.".to_owned())?;
    match check_preset_timing(duration) {
        Some(problem) => Err(format!("The duration {problem}.")),
        None => Ok(duration),
    }
}

/// The name typed in the form, trimmed. Empty is refused.
fn parse_name(typed: &str) -> Result<String, String> {
    match typed.trim() {
        "" => Err("Give the preset a name.".into()),
        name => Ok(name.to_owned()),
    }
}

/// The `transition.update` edit that turns `before` into `draft`, naming only what
/// changed. `None` when nothing did. After a kind change the engine starts from the new
/// kind's defaults, so the patch holds the settings that differ from those.
pub fn update_edit(
    before: &TransitionPresetInfo,
    draft: &Draft,
    kinds: &[KindDescriptor],
) -> Result<Option<Edit>, String> {
    let name = parse_name(&draft.name)?;
    let duration_ms = parse_duration(&draft.duration)?;
    let kind_changes = draft.kind != before.kind;
    let baseline = if kind_changes {
        kinds
            .iter()
            .find(|kind| kind.kind_id == draft.kind)
            .map(default_settings)
            .unwrap_or_default()
    } else {
        before.settings.clone()
    };
    let patch: Settings = draft
        .settings
        .iter()
        .filter(|&(id, value)| baseline.get(id) != Some(value))
        .map(|(id, value)| (id, value.clone()))
        .collect();
    let update = Edit::TransitionUpdate {
        preset_id: before.preset_id.clone(),
        name: (name != before.name).then_some(name),
        kind: kind_changes.then(|| draft.kind.clone()),
        duration_ms: (duration_ms != before.duration_ms).then_some(duration_ms),
        easing: (draft.easing != before.easing).then_some(draft.easing),
        patch,
    };
    let unchanged = matches!(
        &update,
        Edit::TransitionUpdate { name: None, kind: None, duration_ms: None, easing: None, patch, .. }
            if patch.is_empty()
    );
    Ok((!unchanged).then_some(update))
}

/// The settings value a parameter's field holds, from the field's text (a checkbox
/// gives `true` or `false`). Parameters the form can't edit are refused.
pub fn param_value(param: &ParamDescriptor, typed: &str) -> Result<Value, String> {
    let label = &param.label;
    match &param.ty {
        ParamType::Bool => Ok(Value::Bool(typed == "true")),
        ParamType::Int { min, max, .. } => {
            let n: i64 = typed
                .trim()
                .parse()
                .map_err(|_| format!("{label} is a whole number."))?;
            if min.is_some_and(|min| n < min) || max.is_some_and(|max| n > max) {
                return Err(format!(
                    "{label} goes from {} to {}.",
                    min.map_or("any".into(), |m| m.to_string()),
                    max.map_or("any".into(), |m| m.to_string())
                ));
            }
            Ok(Value::from(n))
        }
        ParamType::Float { min, max, .. } => {
            let x: f64 = typed
                .trim()
                .parse()
                .ok()
                .filter(|x: &f64| x.is_finite())
                .ok_or_else(|| format!("{label} is a number."))?;
            if min.is_some_and(|min| x < min) || max.is_some_and(|max| x > max) {
                return Err(format!("{label} is out of range."));
            }
            Ok(Value::from(x))
        }
        ParamType::Enum { options } => options
            .iter()
            .find(|option| option.value == typed)
            .map(|option| Value::String(option.value.clone()))
            .ok_or_else(|| format!("{label} has no option {typed}.")),
        ParamType::String { .. } => Ok(Value::String(typed.to_owned())),
        _ => Err(format!("{label} can't be edited on this page.")),
    }
}

/// A settings value as its field shows it.
fn field_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// A preset's short form for a menu hint: its kind, and for every kind but a cut its
/// duration ("Move 600 ms", "Cut").
pub fn preset_hint(preset: &TransitionPresetInfo, kinds: &[KindDescriptor]) -> String {
    let kind = kind_name(kinds, &preset.kind);
    if preset.kind == CUT {
        kind
    } else {
        format!("{kind} {} ms", preset.duration_ms)
    }
}

/// A preset as the picker lists it: `"<name> — Cut"` or `"<name> — <Kind> · N ms"`, with
/// " (default)" on the default.
pub fn picker_label(
    preset: &TransitionPresetInfo,
    transitions: &TransitionsInfo,
    kinds: &[KindDescriptor],
) -> String {
    let kind = kind_name(kinds, &preset.kind);
    let what = if preset.kind == CUT {
        kind
    } else {
        format!("{kind} · {} ms", preset.duration_ms)
    };
    let default = if preset.preset_id == transitions.default {
        " (default)"
    } else {
        ""
    };
    format!("{} — {what}{default}", preset.name)
}

/// How many scenes name `preset_id` as their transition in, and how many as their
/// transition out.
pub fn override_counts(scenes: &[SceneRow], preset_id: &TransitionPresetId) -> (usize, usize) {
    let count = |pick: fn(&SceneRow) -> Option<&TransitionPresetId>| {
        scenes.iter().filter(|s| pick(s) == Some(preset_id)).count()
    };
    (
        count(|s| s.transition_in.as_ref()),
        count(|s| s.transition_out.as_ref()),
    )
}

/// The editor's note under a preset: "The default: used unless a scene overrides it. "
/// on the default, then "Overrides: N in, M out."
pub fn override_note(snapshot: &Snapshot, preset_id: &TransitionPresetId) -> String {
    let (into, out) = override_counts(&snapshot.scenes, preset_id);
    let default = if &snapshot.transitions.default == preset_id {
        "The default: used unless a scene overrides it. "
    } else {
        ""
    };
    format!("{default}Overrides: {into} in, {out} out.")
}

/// The preset the editor starts on: the one the switch names, else the selected
/// scene's transition in, else the default; each only while it exists.
pub fn initial_pick(
    snapshot: &Snapshot,
    chosen: Option<&TransitionPresetId>,
    selected_scene: Option<&SceneId>,
) -> Option<TransitionPresetId> {
    let into = selected_scene
        .and_then(|id| snapshot.scene(id))
        .and_then(|scene| scene.transition_in.as_ref());
    [chosen, into, Some(&snapshot.transitions.default)]
        .into_iter()
        .flatten()
        .find(|id| snapshot.preset(id).is_some())
        .cloned()
}

/// The preset "+ New preset…" adds: "New preset", a 500 ms ease-in-out fade at its
/// kind's defaults, as in the prototype.
pub fn new_preset_edit(
    existing: &TransitionsInfo,
    kinds: &[KindDescriptor],
) -> (TransitionPresetId, Edit) {
    let preset_id = next_preset_id(existing.presets.iter().map(|p| &p.preset_id), "New preset");
    let mut draft = Draft::new(kinds);
    draft.name = "New preset".into();
    draft.duration = "500".into();
    let edit = Edit::TransitionCreate {
        preset_id: preset_id.clone(),
        name: draft.name.clone(),
        kind: draft.kind.clone(),
        duration_ms: 500,
        easing: Easing::EaseInOut,
        settings: draft.settings,
    };
    (preset_id, edit)
}

/// One line of a scene's Transition in or out submenu.
#[derive(Clone, Debug, PartialEq)]
pub struct OverrideLine {
    /// The preset the line sets, or `None` for "Default", which clears the override.
    pub preset: Option<TransitionPresetId>,
    /// What it reads.
    pub label: String,
    /// Its hint: the preset's kind and duration.
    pub hint: Option<String>,
    /// Whether it is the scene's setting now.
    pub checked: bool,
}

/// A scene's Transition in (`into`) or out submenu (spec-workspace.md 5.2): "Default
/// (<the default's name>)", checked when the scene has no override, then each preset
/// with its kind and duration, checked on the scene's override. Also the opener's hint:
/// the override's name, or "Default".
pub fn override_lines(
    transitions: &TransitionsInfo,
    current: Option<&TransitionPresetId>,
    kinds: &[KindDescriptor],
) -> (String, Vec<OverrideLine>) {
    let current = current.filter(|id| transitions.presets.iter().any(|p| &p.preset_id == *id));
    let default_name = transitions
        .presets
        .iter()
        .find(|p| p.preset_id == transitions.default)
        .map_or_else(|| transitions.default.to_string(), |p| p.name.clone());
    let opener = current
        .and_then(|id| transitions.presets.iter().find(|p| &p.preset_id == id))
        .map_or_else(|| "Default".to_owned(), |p| p.name.clone());
    let lines = std::iter::once(OverrideLine {
        preset: None,
        label: format!("Default ({default_name})"),
        hint: None,
        checked: current.is_none(),
    })
    .chain(transitions.presets.iter().map(|preset| OverrideLine {
        preset: Some(preset.preset_id.clone()),
        label: preset.name.clone(),
        hint: Some(preset_hint(preset, kinds)),
        checked: current == Some(&preset.preset_id),
    }))
    .collect();
    (opener, lines)
}

/// A scene's "Transition in ▸" (`into`) or "Transition out ▸" menu item, whose lines set
/// its override (`scene.set_transitions`, keeping the other direction) and end with
/// "Manage presets…", which shows the Transitions panel. Disabled without a session.
pub fn override_submenu(
    studio: crate::store::Studio,
    session: SessionContext,
    dock: Option<crate::workspace::DockState>,
    scene_id: &SceneId,
    into: bool,
) -> crate::menu::Entry {
    use crate::menu::Entry;
    let title = if into {
        "Transition in"
    } else {
        "Transition out"
    };
    let found = studio.collection.with_untracked(|c| {
        let c = c.as_ref()?;
        let scene = c.scene(scene_id)?;
        Some((
            c.transitions.clone(),
            scene.transition_in.clone(),
            scene.transition_out.clone(),
        ))
    });
    let Some((transitions, now_in, now_out)) = found else {
        return Entry::submenu(title, Vec::new());
    };
    let kinds = builtin_transition_kinds();
    let current = if into { &now_in } else { &now_out };
    let (opener, lines) = override_lines(&transitions, current.as_ref(), &kinds);
    let mut entries = Vec::new();
    for (n, line) in lines.into_iter().enumerate() {
        let (scene_id, now_in, now_out) = (scene_id.clone(), now_in.clone(), now_out.clone());
        let chosen = line.preset.clone();
        let mut entry = Entry::item(line.label, move || {
            let Some(s) = session.get() else { return };
            let (transition_in, transition_out) = if into {
                (chosen.clone(), now_out.clone())
            } else {
                (now_in.clone(), chosen.clone())
            };
            studio.apply(
                &s,
                vec![overrides_edit(
                    scene_id.clone(),
                    transition_in,
                    transition_out,
                )],
            );
        })
        .checked(line.checked);
        if let Some(hint) = line.hint {
            entry = entry.hint(hint);
        }
        entries.push(entry);
        if n == 0 {
            entries.push(Entry::Head("Override".into()));
        }
    }
    entries.push(Entry::Sep);
    entries.push(
        Entry::item("Manage presets…", move || {
            if let Some(dock) = dock {
                dock.0.update(|d| d.show(crate::dock::PanelId::Transitions));
            }
        })
        .disabled_if(dock.is_none()),
    );
    Entry::submenu(title, entries).hint(opener)
}

/// A switch's transition while it runs, for the progress line.
#[derive(Clone, Debug, PartialEq)]
struct Running {
    /// The scene it goes to.
    to: SceneId,
    /// How long it runs, in milliseconds.
    duration_ms: u32,
    /// Counts transitions, so the timer of one that was replaced doesn't end the next.
    seq: u64,
}

/// The program and preview of the last `studio.changed`.
pub type StudioState = (Option<SceneId>, Option<SceneId>);

/// Gives a reload's `answer` the program and preview of the last `studio.changed`
/// (`last`) when one came in while the read was in flight (`changed`), since the event's
/// are newer than the answer's. The event is kept apart from the snapshot, so this holds
/// on the first load too, before the panel has a snapshot.
pub fn keep_newer_studio(answer: &mut Snapshot, changed: bool, last: Option<StudioState>) {
    if changed && let Some((program, preview)) = last {
        answer.program = program;
        answer.preview = preview;
    }
}

/// The panel's state, shared by its parts. Every field is `Copy`.
#[derive(Clone, Copy)]
struct Panel {
    /// The open session.
    session: SessionContext,
    /// The scenes, presets and studio state, once loaded.
    snapshot: RwSignal<Option<Snapshot>>,
    /// The transition kinds, with their parameters.
    kinds: RwSignal<Vec<KindDescriptor>>,
    /// The preset the next switch names, or `None` to let the engine choose.
    chosen: RwSignal<Option<TransitionPresetId>>,
    /// Whether the scene that was on program goes on preview after a switch.
    swap: RwSignal<bool>,
    /// The transition on screen, while one runs.
    running: RwSignal<Option<Running>>,
    /// The last failed call, shown until the next one succeeds or is dismissed.
    problem: RwSignal<Option<String>>,
    /// Bumped by each reload, so an older reload that finishes late is dropped.
    loads: StoredValue<u64>,
    /// Bumped by each `studio.changed`, so a reload's older program and preview don't
    /// overwrite an event's newer ones.
    changes: StoredValue<u64>,
    /// The program and preview of the last `studio.changed`, which a reload keeps when
    /// `changes` moved while it was in flight, even on the first load.
    studio: StoredValue<Option<StudioState>>,
    /// Bumped by each transition the panel shows, so a replaced one's timer can't
    /// clear its successor.
    transitions: StoredValue<u64>,
}

impl Panel {
    /// Runs `work` on the open session and shows its error, if it fails. Does nothing
    /// without a session.
    fn run<F, Fut>(self, work: F)
    where
        F: FnOnce(Session) -> Fut + 'static,
        Fut: Future<Output = Result<(), String>> + 'static,
    {
        let Some(session) = self.session.get() else {
            return;
        };
        spawn_local(async move {
            let outcome = work(session).await;
            self.problem.try_set(outcome.err());
        });
    }

    /// Re-reads the scenes, presets and studio state.
    fn reload(self) {
        let Some(session) = self.session.get() else {
            return;
        };
        let Some(mine) = self.loads.try_update_value(|n| {
            *n += 1;
            *n
        }) else {
            return;
        };
        let asked = self.changes.try_get_value();
        spawn_local(async move {
            let loaded = load(&*session.control).await;
            if self.loads.try_get_value() != Some(mine) {
                return;
            }
            match loaded {
                Ok(mut snapshot) => {
                    // A `studio.changed` came in while this read was in flight: its
                    // program and preview are newer than the answer's.
                    keep_newer_studio(
                        &mut snapshot,
                        self.changes.try_get_value() != asked,
                        self.studio.try_get_value().flatten(),
                    );
                    // A chosen preset that was deleted goes back to the engine's choice.
                    self.chosen.try_update(|chosen| {
                        if chosen
                            .as_ref()
                            .is_some_and(|id| snapshot.preset(id).is_none())
                        {
                            *chosen = None;
                        }
                    });
                    self.snapshot.try_set(Some(snapshot));
                }
                Err(problem) => {
                    self.problem.try_set(Some(problem));
                }
            }
        });
    }

    /// Reads the transition kinds once per session.
    fn load_kinds(self) {
        let Some(session) = self.session.get() else {
            return;
        };
        spawn_local(async move {
            let kinds = transition_kinds(&*session.control).await;
            self.kinds.try_set(kinds);
        });
    }

    /// Follows `studio.changed`: the new program and preview, and the transition's
    /// progress line for as long as it runs.
    fn studio_changed(
        self,
        program: Option<SceneId>,
        preview: Option<SceneId>,
        duration_ms: Option<u32>,
    ) {
        self.changes.try_update_value(|n| *n += 1);
        // Before the first load answers there is no snapshot, so the program this event
        // moves from is the one the last `studio.changed` stored (#925).
        let before = self
            .studio
            .try_update_value(|studio| {
                studio
                    .replace((program.clone(), preview.clone()))
                    .map(|(before, _)| before)
            })
            .flatten();
        let moved = self
            .snapshot
            .try_update(|snapshot| match snapshot {
                Some(snapshot) => {
                    let moved = snapshot.program != program;
                    snapshot.program.clone_from(&program);
                    snapshot.preview = preview;
                    moved
                }
                None => before.is_some_and(|before| before != program),
            })
            .unwrap_or_default();
        let (Some(to), Some(duration_ms)) = (program, duration_ms.filter(|&ms| ms > 0)) else {
            // A cut (or a scene deleted from under program) ends what was on screen.
            if moved {
                self.running.try_set(None);
            }
            return;
        };
        let Some(seq) = self.transitions.try_update_value(|n| {
            *n += 1;
            *n
        }) else {
            return;
        };
        self.running.try_set(Some(Running {
            to,
            duration_ms,
            seq,
        }));
        set_timeout(
            move || {
                self.running.try_update(|running| {
                    if running.as_ref().is_some_and(|r| r.seq == seq) {
                        *running = None;
                    }
                });
            },
            Duration::from_millis(u64::from(duration_ms)),
        );
    }

    /// Puts the preview scene on program with `preset` (or the engine's choice), then,
    /// if swapping, puts the old program scene on preview.
    fn switch(self, preset: Option<TransitionPresetId>) {
        let Some((switch, before)) = self.snapshot.with_untracked(|snapshot| {
            let snapshot = snapshot.as_ref()?;
            Some((switch_action(snapshot, preset)?, snapshot.program.clone()))
        }) else {
            return;
        };
        let swap = self.swap.get_untracked();
        self.run(move |session| async move {
            let to = match &switch {
                Action::SceneSwitch { scene_id, .. } => scene_id.clone(),
                _ => return Ok(()),
            };
            action(&*session.control, switch).await?;
            if let Some(back) = swap.then(|| swap_action(before, &to)).flatten() {
                action(&*session.control, back).await?;
            }
            Ok(())
        });
    }
}

/// The studio-mode panel: preview, program, the transition between them, and the
/// transition presets. Reads the open session from [`SessionContext`] context.
#[component]
pub fn StudioMode() -> impl IntoView {
    let Some(session) = use_context::<SessionContext>() else {
        return view! { <p class="placeholder">"Not connected."</p> }.into_any();
    };
    let panel = Panel {
        session,
        snapshot: RwSignal::new(None),
        kinds: RwSignal::new(builtin_transition_kinds()),
        chosen: RwSignal::new(None),
        swap: RwSignal::new(true),
        running: RwSignal::new(None),
        problem: RwSignal::new(None),
        loads: StoredValue::new(0),
        changes: StoredValue::new(0),
        studio: StoredValue::new(None),
        transitions: StoredValue::new(0),
    };

    // Follow the engine: re-read on a collection change (or lost events), and take
    // studio changes as they come. The loop holds only the event stream, which ends
    // when the session closes.
    if let Some(open) = session.get() {
        let mut events = open.control.events();
        drop(open);
        panel.reload();
        panel.load_kinds();
        spawn_local(async move {
            while let Some(event) = events.next().await {
                match event {
                    Event::StateChanged { .. } | Event::EventsDropped { .. } => panel.reload(),
                    Event::StudioChanged {
                        program_scene,
                        preview_scene,
                        transition,
                        ..
                    } => panel.studio_changed(
                        program_scene,
                        preview_scene,
                        transition.map(|t| t.duration_ms),
                    ),
                    _ => {}
                }
            }
        });
    }

    let scenes = move || {
        panel.snapshot.with(|snapshot| {
            snapshot
                .as_ref()
                .map(|snapshot| snapshot.scenes.clone())
                .unwrap_or_default()
        })
    };
    let preview = move || {
        panel
            .snapshot
            .with(|s| s.as_ref().and_then(|s| s.preview.clone()))
    };
    let program_name = move || {
        panel.snapshot.with(|snapshot| {
            let snapshot = snapshot.as_ref()?;
            Some(snapshot.scene_name(snapshot.program.as_ref()?))
        })
    };
    let blocker = move || {
        panel.snapshot.with(|snapshot| match snapshot {
            Some(snapshot) => switch_blocker(snapshot),
            None => Some("Loading…"),
        })
    };
    let presets = move || {
        panel.snapshot.with(|snapshot| {
            snapshot
                .as_ref()
                .map(|snapshot| snapshot.transitions.presets.clone())
                .unwrap_or_default()
        })
    };
    let cut = move || {
        panel
            .snapshot
            .with(|snapshot| snapshot.as_ref().and_then(|s| cut_preset(&s.transitions)))
    };
    let label = move || {
        panel.snapshot.with(|snapshot| {
            snapshot
                .as_ref()
                .map(|snapshot| expected_label(snapshot, panel.chosen.get().as_ref()))
        })
    };
    let set_preview = move |event: leptos::ev::Event| {
        let scene_id = SceneId::new(event_target_value(&event));
        panel.run(move |session| async move {
            action(&*session.control, Action::StudioSetPreview { scene_id }).await
        });
    };
    let shared = use_context::<crate::store::Studio>();
    let studio_mode = move || shared.is_none_or(|s| s.studio_mode.get());
    let choose = move |event: leptos::ev::Event| {
        let value = event_target_value(&event);
        panel
            .chosen
            .set((!value.is_empty()).then(|| TransitionPresetId::new(value)));
    };

    view! {
        <section class="studio" aria-label="Transitions">
            <Show
                when=studio_mode
                fallback=|| view! {
                    <p class="hint studio-off">
                        "Studio mode is off: clicking a scene puts it on program. Turn it on in \
                         the header to set a scene up on preview first."
                    </p>
                }
            >
            <div class="studio-bar">
                <label class="slot">
                    <span class="slot-name">"Preview"</span>
                    <select on:change=set_preview aria-label="Scene on preview">
                        <option
                            value=""
                            disabled=true
                            prop:selected=move || {
                                let preview = preview();
                                !scenes().iter().any(|scene| Some(&scene.scene_id) == preview.as_ref())
                            }
                        >
                            "Choose a scene"
                        </option>
                        <For each=scenes key=|scene| (scene.scene_id.clone(), scene.name.clone()) let:scene>
                            {
                                let scene_id = scene.scene_id.clone();
                                view! {
                                    <option
                                        value=scene.scene_id.to_string()
                                        prop:selected=move || preview().as_ref() == Some(&scene_id)
                                    >
                                        {scene.name}
                                    </option>
                                }
                            }
                        </For>
                    </select>
                </label>
                <div class="studio-actions">
                    <select on:change=choose aria-label="Transition preset">
                        <option value="" prop:selected=move || panel.chosen.get().is_none()>
                            "Scene or default preset"
                        </option>
                        <For each=presets key=|preset| (preset.preset_id.clone(), preset.name.clone()) let:preset>
                            {
                                let preset_id = preset.preset_id.clone();
                                view! {
                                    <option
                                        value=preset.preset_id.to_string()
                                        prop:selected=move || panel.chosen.get().as_ref() == Some(&preset_id)
                                    >
                                        {preset.name}
                                    </option>
                                }
                            }
                        </For>
                    </select>
                    <button
                        class="primary"
                        disabled=move || blocker().is_some()
                        title=move || blocker().unwrap_or("Puts the preview scene on program")
                        on:click=move |_| panel.switch(panel.chosen.get_untracked())
                    >
                        "Transition"
                    </button>
                    <button
                        disabled=move || blocker().is_some() || cut().is_none()
                        title=move || {
                            if cut().is_none() { "No cut preset: add one below" } else { "Switches at once" }
                        }
                        on:click=move |_| panel.switch(cut())
                    >
                        "Cut"
                    </button>
                    <label class="inline-label">
                        <input type="checkbox" class="inline-check" bind:checked=panel.swap />
                        "Swap"
                    </label>
                </div>
                <div class="slot">
                    <span class="slot-name">"Program"</span>
                    <strong class="program-name">
                        {move || program_name().unwrap_or_else(|| "None".into())}
                    </strong>
                </div>
            </div>
            <p class="hint" role="status">
                {move || match panel.running.get() {
                    Some(running) => {
                        let to = panel
                            .snapshot
                            .with(|s| s.as_ref().map(|s| s.scene_name(&running.to)))
                            .unwrap_or_default();
                        view! {
                            <progress aria-label="Transition running"></progress>
                            " Transitioning to " {to} " (" {running.duration_ms} " ms)"
                        }
                            .into_any()
                    }
                    None => label().unwrap_or_default().into_any(),
                }}
            </p>
            </Show>
            {move || {
                panel.problem.get().map(|problem| {
                    view! {
                        <p class="problem" role="alert">
                            {problem}
                            " "
                            <button class="link" on:click=move |_| panel.problem.set(None)>
                                "Dismiss"
                            </button>
                        </p>
                    }
                })
            }}
            <Presets panel=panel />
            <details class="overrides-section">
                <summary>"Per-scene presets"</summary>
                <SceneOverrides panel=panel />
            </details>
        </section>
    }
    .into_any()
}

/// The presets (spec-panels.md 10): a picker of every preset ending in "+ New
/// preset…", and the picked one's editor. Each field sends its change at once.
#[component]
fn Presets(panel: Panel) -> impl IntoView {
    let shared = use_context::<crate::store::Studio>();
    // The preset picked in the list; `None` until one is, when the editor starts on the
    // switch's choice, the selected scene's transition in, or the default.
    let picked = RwSignal::new(None::<TransitionPresetId>);
    let shown = Memo::new(move |_| {
        panel.snapshot.with(|snapshot| {
            let snapshot = snapshot.as_ref()?;
            picked
                .get()
                .filter(|id| snapshot.preset(id).is_some())
                .or_else(|| {
                    let scene = shared.and_then(|s| s.selection.with(|sel| sel.scene.clone()));
                    initial_pick(snapshot, panel.chosen.get().as_ref(), scene.as_ref())
                })
        })
    });
    let options = move || {
        panel.snapshot.with(|snapshot| {
            snapshot
                .as_ref()
                .map(|snapshot| {
                    panel.kinds.with(|kinds| {
                        snapshot
                            .transitions
                            .presets
                            .iter()
                            .map(|p| {
                                (
                                    p.preset_id.to_string(),
                                    picker_label(p, &snapshot.transitions, kinds),
                                )
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .unwrap_or_default()
        })
    };
    let pick = move |event: leptos::ev::Event| {
        let value = event_target_value(&event);
        if value == NEW_PRESET {
            let Some(made) = panel.snapshot.with_untracked(|s| {
                let s = s.as_ref()?;
                Some(
                    panel
                        .kinds
                        .with_untracked(|kinds| new_preset_edit(&s.transitions, kinds)),
                )
            }) else {
                return;
            };
            let (preset_id, create) = made;
            picked.set(Some(preset_id));
            panel.run(move |session| async move { edit(&*session.control, create).await });
        } else if !value.is_empty() {
            picked.set(Some(TransitionPresetId::new(value)));
        }
    };
    view! {
        <section class="presets" aria-label="Presets">
            <h3>"Presets"</h3>
            <p class="hint">
                "Pick a preset to edit it. Scenes use the default unless you override it: \
                 right-click a scene, or set it in its Properties or the Inspector."
            </p>
            <select class="preset-picker" aria-label="Preset to edit" on:change=pick>
                {move || {
                    let now = shown.get().map(|id| id.to_string()).unwrap_or_default();
                    options()
                        .into_iter()
                        .chain(std::iter::once((NEW_PRESET.to_owned(), "+ New preset…".to_owned())))
                        .map(|(value, label)| {
                            let selected = value == now;
                            view! { <option value=value selected=selected>{label}</option> }
                        })
                        .collect_view()
                }}
            </select>
            {move || shown.get().map(|preset_id| view! {
                <PresetEditor panel=panel preset_id=preset_id picked=picked />
            })}
        </section>
    }
}

/// The picker's value for "+ New preset…", which no preset ID can be (IDs have no
/// spaces).
const NEW_PRESET: &str = "+ new";

/// The picked preset's fields: Name, its kind (Cut, Fade or Move), then, except for a
/// cut, its duration, its kind's parameters and its easing; its override counts; and
/// Make default and Delete, which the default doesn't have. Each change is one
/// `transition.update`.
#[component]
fn PresetEditor(
    panel: Panel,
    preset_id: TransitionPresetId,
    picked: RwSignal<Option<TransitionPresetId>>,
) -> impl IntoView {
    let id = StoredValue::new(preset_id);
    let preset = Memo::new(move |_| {
        panel
            .snapshot
            .with(|s| s.as_ref().and_then(|s| s.preset(&id.get_value()).cloned()))
    });
    let draft = RwSignal::new(preset.get_untracked().map_or_else(
        || panel.kinds.with_untracked(|k| Draft::new(k)),
        |p| Draft::from_preset(&p),
    ));
    // The engine's preset wins: each change it reports resets the fields to it.
    Effect::new(move |_| {
        if let Some(preset) = preset.get() {
            draft.set(Draft::from_preset(&preset));
        }
    });
    let field_problem = RwSignal::new(None::<String>);
    let invalid = RwSignal::new(BTreeSet::<String>::new());
    // Sends what the draft changed, or shows why it can't.
    let save = move || {
        if invalid.with_untracked(|ids| !ids.is_empty()) {
            return;
        }
        let Some(before) = preset.get_untracked() else {
            return;
        };
        let kinds = panel.kinds.get_untracked();
        match update_edit(&before, &draft.get_untracked(), &kinds) {
            Err(problem) => field_problem.set(Some(problem)),
            Ok(None) => field_problem.set(None),
            Ok(Some(change)) => {
                field_problem.set(None);
                panel.run(move |session| async move { edit(&*session.control, change).await });
            }
        }
    };
    let is_default = move || {
        panel.snapshot.with(|s| {
            s.as_ref()
                .is_some_and(|s| s.transitions.default == id.get_value())
        })
    };
    let is_cut = move || draft.with(|d| d.kind == CUT);
    let params = move || {
        let kind_id = draft.with(|draft| draft.kind.clone());
        panel.kinds.with(|kinds| {
            kinds
                .iter()
                .find(|kind| kind.kind_id == kind_id)
                .map(|kind| kind.params.clone())
                .unwrap_or_default()
        })
    };
    let note = move || {
        panel
            .snapshot
            .with(|s| s.as_ref().map(|s| override_note(s, &id.get_value())))
    };
    let make_default = move |_| {
        let preset_id = id.get_value();
        panel.run(move |session| async move {
            edit(&*session.control, Edit::TransitionSetDefault { preset_id }).await
        });
    };
    let delete = move |_| {
        let preset_id = id.get_value();
        picked.set(None);
        panel.run(move |session| async move {
            edit(&*session.control, Edit::TransitionDelete { preset_id }).await
        });
    };
    let duration = Signal::derive(move || draft.with(|d| d.duration.trim().parse::<f64>().ok()));
    let send_duration = move |ms: f64| {
        let typed = (ms.round().max(0.0) as u32).to_string();
        if draft.with_untracked(|d| d.duration == typed) {
            return false;
        }
        draft.update(|d| d.duration = typed);
        save();
        true
    };
    let choose_easing = move |event: leptos::ev::Event| {
        if let Some(easing) = parse_easing(&event_target_value(&event)) {
            draft.update(|d| d.easing = easing);
            save();
        }
    };
    let kind_radio = move |kind: KindDescriptor| {
        let kind_id = kind.kind_id.clone();
        let checked = {
            let kind_id = kind_id.clone();
            move || draft.with(|d| d.kind == kind_id)
        };
        view! {
            <label class="segment" class:on=checked.clone()>
                <input
                    type="radio"
                    name="preset-kind"
                    prop:checked=checked.clone()
                    on:change=move |_| {
                        panel
                            .kinds
                            .with_untracked(|kinds| draft.update(|d| d.set_kind(kinds, &kind_id)));
                        invalid.set(BTreeSet::new());
                        save();
                    }
                />
                {kind.name}
            </label>
        }
    };
    view! {
        <div class="preset-editor">
            <label class="preset-row">
                <span class="field-label">"Name"</span>
                <input
                    type="text"
                    prop:value=move || draft.with(|d| d.name.clone())
                    on:change=move |event| {
                        let value = event_target_value(&event);
                        draft.update(|d| d.name = value);
                        save();
                    }
                />
            </label>
            <div class="segmented" role="radiogroup" aria-label="Type">
                {move || panel.kinds.get().into_iter().map(kind_radio).collect_view()}
            </div>
            <Show when=move || !is_cut()>
                <div class="preset-fields">
                    <NumberField label="Duration" unit="ms" value=duration step=10.0 decimals=0
                        min=0.0 max=f64::from(MAX_DURATION_MS) send=send_duration />
                    <For each=params key=|param| param.id.clone() let:param>
                        <ParamField param=param draft=draft problem=field_problem invalid=invalid saved=save />
                    </For>
                    <label class="select-field">
                        <span class="field-label">"Easing"</span>
                        <select aria-label="Easing" on:change=choose_easing>
                            {EASINGS
                                .into_iter()
                                .map(|easing| view! {
                                    <option
                                        value=easing_value(easing)
                                        prop:selected=move || draft.with(|d| d.easing == easing)
                                    >
                                        {easing_label(easing)}
                                    </option>
                                })
                                .collect_view()}
                        </select>
                    </label>
                </div>
            </Show>
            {move || field_problem.get().map(|problem| view! { <p class="problem" role="alert">{problem}</p> })}
            <p class="note">{note}</p>
            <Show when=move || !is_default()>
                <div class="buttons">
                    <button on:click=make_default>"Make default"</button>
                    <button class="danger-text" on:click=delete>"Delete"</button>
                </div>
            </Show>
        </div>
    }
}

/// One of the kind's parameters in the preset editor. Enum parameters are a list,
/// booleans a checkbox, numbers and text a field; others are shown read-only. A value it
/// takes goes into the draft, then `saved` runs.
#[component]
fn ParamField(
    param: ParamDescriptor,
    draft: RwSignal<Draft>,
    problem: RwSignal<Option<String>>,
    invalid: RwSignal<BTreeSet<String>>,
    saved: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let id = param.id.clone();
    let current = {
        let id = id.clone();
        move || draft.with(|d| field_text(d.settings.get(&id)))
    };
    let param = StoredValue::new(param);
    let set = move |typed: String| {
        let id = id.clone();
        match param.with_value(|param| param_value(param, &typed)) {
            Ok(value) => {
                invalid.update(|ids| {
                    ids.remove(&id);
                });
                if invalid.with(BTreeSet::is_empty) {
                    problem.set(None);
                }
                draft.update(|d| {
                    d.settings.insert(id, value);
                });
                saved();
            }
            Err(why) => {
                invalid.update(|ids| {
                    ids.insert(id);
                });
                problem.set(Some(why));
            }
        }
    };
    let label = param.with_value(|p| p.label.clone());
    let hint = param.with_value(|p| p.description.clone());
    let control = match param.with_value(|p| p.ty.clone()) {
        ParamType::Enum { options } => {
            let current = current.clone();
            let set = set.clone();
            view! {
                <select on:change=move |event| set(event_target_value(&event))>
                    {options
                        .into_iter()
                        .map(|option| {
                            let current = current.clone();
                            let value = option.value.clone();
                            view! {
                                <option value=option.value prop:selected=move || current() == value>
                                    {option.label}
                                </option>
                            }
                        })
                        .collect_view()}
                </select>
            }
            .into_any()
        }
        ParamType::Bool => {
            let current = current.clone();
            let set = set.clone();
            view! {
                <input
                    type="checkbox"
                    class="inline-check"
                    prop:checked=move || current() == "true"
                    on:change=move |event| {
                        set(event_target_checked(&event).to_string());
                    }
                />
            }
            .into_any()
        }
        ParamType::Int { min, max, step, .. } => {
            let current = current.clone();
            let set = set.clone();
            view! {
                <input
                    type="number"
                    min=min.map(|n| n.to_string())
                    max=max.map(|n| n.to_string())
                    step=step.map(|n| n.to_string())
                    prop:value=current
                    on:change=move |event| set(event_target_value(&event))
                />
            }
            .into_any()
        }
        ParamType::Float { .. } | ParamType::String { .. } => {
            let current = current.clone();
            let set = set.clone();
            view! {
                <input type="text" prop:value=current on:change=move |event| set(event_target_value(&event)) />
            }
            .into_any()
        }
        _ => view! { <span class="hint">{current}</span> }.into_any(),
    };
    let unit = param.with_value(|p| match &p.ty {
        ParamType::Int { unit, .. } | ParamType::Float { unit, .. } => unit.clone(),
        _ => None,
    });
    view! {
        <label title=hint>
            {label}
            {unit.map(|unit| format!(" ({unit})"))}
            {control}
        </label>
    }
}

/// Each scene's `transition_in` and `transition_out`.
#[component]
fn SceneOverrides(panel: Panel) -> impl IntoView {
    let scenes = move || {
        panel.snapshot.with(|snapshot| {
            snapshot
                .as_ref()
                .map(|snapshot| snapshot.scenes.clone())
                .unwrap_or_default()
        })
    };
    view! {
        <h3>"Per-scene presets"</h3>
        <p class="hint">"A preset chosen for the switch wins over these; a scene's In wins over the other scene's Out."</p>
        <table class="overrides">
            <thead>
                <tr><th>"Scene"</th><th>"In"</th><th>"Out"</th></tr>
            </thead>
            <tbody>
                <For
                    each=scenes
                    key=|scene| {
                        (
                            scene.scene_id.clone(),
                            scene.name.clone(),
                            scene.transition_in.clone(),
                            scene.transition_out.clone(),
                        )
                    }
                    let:scene
                >
                    <OverrideRow panel=panel scene=scene />
                </For>
            </tbody>
        </table>
    }
}

/// One scene's two overrides.
#[component]
fn OverrideRow(panel: Panel, scene: SceneRow) -> impl IntoView {
    let row = StoredValue::new(scene.clone());
    let set = move |into: bool, value: String| {
        let chosen = (!value.is_empty()).then(|| TransitionPresetId::new(value));
        let scene = row.get_value();
        let (transition_in, transition_out) = if into {
            (chosen, scene.transition_out)
        } else {
            (scene.transition_in, chosen)
        };
        let change = overrides_edit(scene.scene_id, transition_in, transition_out);
        panel.run(move |session| async move { edit(&*session.control, change).await });
    };
    let picker = move |into: bool| {
        let current = row.with_value(|scene| {
            if into {
                scene.transition_in.clone()
            } else {
                scene.transition_out.clone()
            }
        });
        let presets = move || {
            panel.snapshot.with(|s| {
                s.as_ref()
                    .map(|s| s.transitions.presets.clone())
                    .unwrap_or_default()
            })
        };
        let label = if into {
            "Transition in"
        } else {
            "Transition out"
        };
        view! {
            <select aria-label=label on:change=move |event| set(into, event_target_value(&event))>
                <option value="" prop:selected=current.is_none()>"None"</option>
                {move || {
                    presets()
                        .into_iter()
                        .map(|preset| {
                            let selected = current.as_ref() == Some(&preset.preset_id);
                            view! {
                                <option value=preset.preset_id.to_string() prop:selected=selected>
                                    {preset.name}
                                </option>
                            }
                        })
                        .collect_view()
                }}
            </select>
        }
    };
    view! {
        <tr>
            <td>{scene.name}</td>
            <td>{picker(true)}</td>
            <td>{picker(false)}</td>
        </tr>
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Draft, Reason, SceneRow, Snapshot, cut_preset, default_settings, expected_label,
        expected_preset, initial_pick, keep_newer_studio, load, new_preset_edit, next_preset_id,
        override_counts, override_lines, override_note, param_value, parse_duration, parse_easing,
        picker_label, preset_hint, swap_action, switch_action, switch_blocker, transition_kinds,
        update_edit,
    };
    use jstream_control::protocol::{
        Action, Easing, Edit, FADE, Hello, MOVE, PROTOCOL_VERSION, Rev, SceneId, Settings,
        TransitionPresetId, TransitionPresetInfo, TransitionsInfo, builtin_transition_kinds,
        initial_preset,
    };
    use jstream_control::{Control, MockControl, MockEngine};
    use serde_json::json;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    /// Polls a future that is ready at once, as the mock's are.
    fn now<F: Future>(future: F) -> F::Output {
        match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the mock's futures are ready at once"),
        }
    }

    fn connected(engine: &MockEngine) -> MockControl {
        let control = engine.connect();
        let hello = Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "test".into(),
        };
        now(control.hello(hello)).expect("hello");
        control
    }

    fn scene(id: &str) -> SceneRow {
        SceneRow {
            scene_id: SceneId::new(id),
            name: id.to_uppercase(),
            transition_in: None,
            transition_out: None,
        }
    }

    fn fade(id: &str) -> TransitionPresetInfo {
        let kinds = builtin_transition_kinds();
        let fade = kinds.iter().find(|k| k.kind_id == FADE).expect("fade");
        TransitionPresetInfo {
            preset_id: TransitionPresetId::new(id),
            name: format!("Fade {id}"),
            kind: FADE.into(),
            duration_ms: 500,
            easing: Easing::EaseOut,
            settings: default_settings(fade),
        }
    }

    /// Scenes `a` (program) and `b` (preview), with the initial `cut` preset as the
    /// default and two fades.
    fn studio() -> Snapshot {
        let mut transitions = TransitionsInfo::default();
        transitions.presets.push(fade("in"));
        transitions.presets.push(fade("out"));
        Snapshot {
            rev: Rev(1),
            scenes: vec![scene("a"), scene("b")],
            transitions,
            program: Some(SceneId::new("a")),
            preview: Some(SceneId::new("b")),
        }
    }

    /// A `studio.changed` during a reload, even the first one (the panel had no
    /// snapshot yet), wins over the answer's program and preview.
    #[test]
    fn an_event_during_a_reload_keeps_its_program_and_preview() {
        let event = (Some(SceneId::new("b")), None);

        let mut answer = studio();
        keep_newer_studio(&mut answer, true, Some(event.clone()));
        assert_eq!(answer.program, Some(SceneId::new("b")));
        assert_eq!(answer.preview, None);

        // No event while it was in flight: the answer stands, even after an older one.
        let mut answer = studio();
        keep_newer_studio(&mut answer, false, Some(event));
        assert_eq!(answer, studio());

        let mut answer = studio();
        keep_newer_studio(&mut answer, true, None);
        assert_eq!(answer, studio());
    }

    /// `studio_changed` records the event's program and preview even before the first
    /// load has answered (no snapshot yet), so the reload can keep them (#884).
    #[test]
    fn an_event_before_the_first_load_is_stored() {
        use super::Panel;
        use crate::app::SessionContext;
        use leptos::prelude::*;

        let panel = Panel {
            session: SessionContext(StoredValue::new_local(None)),
            snapshot: RwSignal::new(None),
            kinds: RwSignal::new(Vec::new()),
            chosen: RwSignal::new(None),
            swap: RwSignal::new(false),
            running: RwSignal::new(None),
            problem: RwSignal::new(None),
            loads: StoredValue::new(0),
            changes: StoredValue::new(0),
            studio: StoredValue::new(None),
            transitions: StoredValue::new(0),
        };
        panel.studio_changed(Some(SceneId::new("b")), None, None);
        assert_eq!(
            panel.studio.get_value(),
            Some((Some(SceneId::new("b")), None))
        );
        assert_eq!(panel.changes.get_value(), 1);
        assert!(panel.snapshot.get_untracked().is_none());
    }

    /// A cut that follows a transition, both before the first load has answered, ends
    /// the transition: `moved` comes from the stored program when there is no snapshot
    /// (#925).
    #[test]
    fn a_cut_before_the_first_load_ends_the_running_transition() {
        use super::{Panel, Running};
        use crate::app::SessionContext;
        use leptos::prelude::*;

        let panel = Panel {
            session: SessionContext(StoredValue::new_local(None)),
            snapshot: RwSignal::new(None),
            kinds: RwSignal::new(Vec::new()),
            chosen: RwSignal::new(None),
            swap: RwSignal::new(false),
            running: RwSignal::new(None),
            problem: RwSignal::new(None),
            loads: StoredValue::new(0),
            changes: StoredValue::new(0),
            studio: StoredValue::new(None),
            transitions: StoredValue::new(0),
        };
        // The transition's own `studio.changed` would start a timer, which needs a
        // browser, so its outcome is set by hand: "b" on program, transitioning to it.
        panel
            .studio
            .set_value(Some((Some(SceneId::new("b")), None)));
        panel.running.set(Some(Running {
            to: SceneId::new("b"),
            duration_ms: 1000,
            seq: 1,
        }));

        // An event that keeps "b" on program leaves the transition running.
        panel.studio_changed(Some(SceneId::new("b")), Some(SceneId::new("a")), None);
        assert!(panel.running.get_untracked().is_some());

        // A cut to "a" ends it.
        panel.studio_changed(Some(SceneId::new("a")), None, None);
        assert!(panel.running.get_untracked().is_none());
        assert_eq!(
            panel.studio.get_value(),
            Some((Some(SceneId::new("a")), None))
        );
        assert!(panel.snapshot.get_untracked().is_none());
    }

    /// The scenes and studio state come from the engine, and the panel's switch and
    /// preview calls move them.
    #[test]
    fn the_panel_s_calls_drive_preview_and_program() {
        let engine = MockEngine::new();
        let control = connected(&engine);
        for id in ["a", "b"] {
            let create = Edit::SceneCreate {
                scene_id: SceneId::new(id),
                name: id.to_uppercase(),
                role: None,
            };
            now(super::edit(&control, create)).expect("scene");
        }

        let snapshot = now(load(&control)).expect("load");
        assert_eq!(snapshot.scenes.len(), 2);
        assert_eq!(snapshot.transitions, TransitionsInfo::default());
        assert_eq!(snapshot.program, Some(SceneId::new("a")));

        let preview = Action::StudioSetPreview {
            scene_id: SceneId::new("b"),
        };
        now(super::action(&control, preview)).expect("preview");
        let snapshot = now(load(&control)).expect("load");
        assert_eq!(snapshot.preview, Some(SceneId::new("b")));
        assert_eq!(switch_blocker(&snapshot), None);

        let cut = cut_preset(&snapshot.transitions);
        let switch = switch_action(&snapshot, cut).expect("a switch");
        now(super::action(&control, switch)).expect("switch");
        let back = swap_action(snapshot.program.clone(), &SceneId::new("b")).expect("a swap");
        now(super::action(&control, back)).expect("swap");

        let snapshot = now(load(&control)).expect("load");
        assert_eq!(snapshot.program, Some(SceneId::new("b")));
        assert_eq!(snapshot.preview, Some(SceneId::new("a")));
    }

    #[test]
    fn kinds_fall_back_to_the_built_in_ones() {
        let engine = MockEngine::new();
        let control = connected(&engine);
        let kinds = now(transition_kinds(&control));
        let ids: Vec<_> = kinds.iter().map(|k| k.kind_id.as_str()).collect();
        assert_eq!(ids, ["cut", "fade", "move"]);
    }

    #[test]
    fn the_switch_needs_another_scene_on_preview() {
        let mut snapshot = studio();
        assert_eq!(
            switch_action(&snapshot, None),
            Some(Action::SceneSwitch {
                scene_id: SceneId::new("b"),
                preset_id: None,
            })
        );
        snapshot.preview = snapshot.program.clone();
        assert!(switch_blocker(&snapshot).is_some());
        assert_eq!(switch_action(&snapshot, None), None);
        snapshot.preview = None;
        assert_eq!(
            switch_blocker(&snapshot),
            Some("Put a scene on preview first.")
        );
    }

    #[test]
    fn swapping_puts_the_old_program_on_preview() {
        let b = SceneId::new("b");
        assert_eq!(
            swap_action(Some(SceneId::new("a")), &b),
            Some(Action::StudioSetPreview {
                scene_id: SceneId::new("a")
            })
        );
        assert_eq!(swap_action(Some(b.clone()), &b), None);
        assert_eq!(swap_action(None, &b), None);
    }

    /// The label follows the engine's order: chosen, the new scene's in, the old
    /// scene's out, the default.
    #[test]
    fn the_expected_preset_follows_the_engine_s_order() {
        let mut snapshot = studio();
        let name = |snapshot: &Snapshot, chosen: Option<&str>| {
            let chosen = chosen.map(TransitionPresetId::new);
            expected_preset(snapshot, chosen.as_ref())
                .map(|(preset, reason)| (preset.preset_id.to_string(), reason))
        };
        assert_eq!(name(&snapshot, None), Some(("cut".into(), Reason::Default)));
        snapshot.scenes[0].transition_out = Some(TransitionPresetId::new("out"));
        assert_eq!(
            name(&snapshot, None),
            Some(("out".into(), Reason::SceneOut))
        );
        snapshot.scenes[1].transition_in = Some(TransitionPresetId::new("in"));
        assert_eq!(name(&snapshot, None), Some(("in".into(), Reason::SceneIn)));
        assert_eq!(
            name(&snapshot, Some("out")),
            Some(("out".into(), Reason::Chosen))
        );
        // A name no preset has is skipped.
        assert_eq!(
            name(&snapshot, Some("gone")),
            Some(("in".into(), Reason::SceneIn))
        );
        assert_eq!(
            expected_label(&snapshot, None),
            "Uses Fade in (the preview scene's transition in)."
        );
    }

    #[test]
    fn cut_uses_a_cut_preset_preferring_the_default() {
        let mut transitions = TransitionsInfo::default();
        assert_eq!(
            cut_preset(&transitions),
            Some(TransitionPresetId::new("cut"))
        );
        let mut snap = initial_preset();
        snap.preset_id = TransitionPresetId::new("snap");
        transitions.presets.insert(0, snap);
        assert_eq!(
            cut_preset(&transitions),
            Some(TransitionPresetId::new("cut"))
        );
        transitions.default = TransitionPresetId::new("snap");
        assert_eq!(
            cut_preset(&transitions),
            Some(TransitionPresetId::new("snap"))
        );
        transitions.presets = vec![fade("f")];
        assert_eq!(cut_preset(&transitions), None);
    }

    #[test]
    fn preset_ids_come_from_the_name_and_stay_unique() {
        let taken = [
            TransitionPresetId::new("slow-fade"),
            TransitionPresetId::new("slow-fade-2"),
        ];
        assert_eq!(next_preset_id(&[], "  Slow  Fade! ").as_str(), "slow-fade");
        assert_eq!(next_preset_id(&taken, "Slow fade").as_str(), "slow-fade-3");
        assert_eq!(next_preset_id(&[], "!!!").as_str(), "transition");
    }

    #[test]
    fn a_new_preset_is_a_fade_at_its_defaults() {
        let kinds = builtin_transition_kinds();
        let draft = Draft::new(&kinds);
        assert_eq!(draft.kind, FADE);
        assert_eq!(draft.settings.get("stagger_ms"), Some(&json!(0)));
        assert_eq!(draft.settings.get("unmatched"), Some(&json!("fade")));
        assert!(parse_duration("abc").is_err());
        assert_eq!(parse_duration(" 0 "), Ok(0));
    }

    /// An update names only what changed; a kind change patches against the new kind's
    /// defaults.
    #[test]
    fn an_update_names_only_what_changed() {
        let kinds = builtin_transition_kinds();
        let before = fade("f");
        let mut draft = Draft::from_preset(&before);
        assert_eq!(update_edit(&before, &draft, &kinds), Ok(None));

        draft.duration = "800".into();
        draft.settings.insert("stagger_ms", json!(40));
        let Ok(Some(Edit::TransitionUpdate {
            name,
            kind,
            duration_ms,
            easing,
            patch,
            ..
        })) = update_edit(&before, &draft, &kinds)
        else {
            panic!("an update");
        };
        assert_eq!(
            (name, kind, duration_ms, easing),
            (None, None, Some(800), None)
        );
        assert_eq!(
            patch,
            [("stagger_ms", json!(40))]
                .into_iter()
                .collect::<Settings>()
        );

        let mut draft = Draft::from_preset(&before);
        draft.set_kind(&kinds, MOVE);
        draft.settings.insert("unmatched", json!("slide"));
        let Ok(Some(Edit::TransitionUpdate { kind, patch, .. })) =
            update_edit(&before, &draft, &kinds)
        else {
            panic!("an update");
        };
        assert_eq!(kind.as_deref(), Some(MOVE));
        assert_eq!(
            patch,
            [("unmatched", json!("slide"))]
                .into_iter()
                .collect::<Settings>()
        );
    }

    #[test]
    fn param_fields_are_checked_against_the_descriptor() {
        let kinds = builtin_transition_kinds();
        let fade = kinds.iter().find(|k| k.kind_id == FADE).expect("fade");
        let unmatched = fade.param("unmatched").expect("unmatched");
        let stagger = fade.param("stagger_ms").expect("stagger");
        assert_eq!(param_value(unmatched, "scale"), Ok(json!("scale")));
        assert!(param_value(unmatched, "spin").is_err());
        assert_eq!(param_value(stagger, "120"), Ok(json!(120)));
        assert!(param_value(stagger, "-1").is_err());
        assert!(param_value(stagger, "10001").is_err());
        assert!(param_value(stagger, "1.5").is_err());
    }

    #[test]
    fn easings_read_as_text() {
        assert_eq!(parse_easing("spring"), Some(Easing::Spring));
        assert_eq!(parse_easing("bounce"), None);
    }

    #[test]
    fn the_picker_and_the_menus_name_each_preset_s_kind_and_length() {
        let kinds = builtin_transition_kinds();
        let snapshot = studio();
        let t = &snapshot.transitions;
        assert_eq!(
            picker_label(&t.presets[0], t, &kinds),
            "Cut — Cut (default)"
        );
        assert_eq!(
            picker_label(&t.presets[1], t, &kinds),
            "Fade in — Fade · 500 ms"
        );
        assert_eq!(preset_hint(&t.presets[0], &kinds), "Cut");
        assert_eq!(preset_hint(&t.presets[1], &kinds), "Fade 500 ms");
    }

    /// The submenu starts with the default, checked with no override, and checks the
    /// override; a name no preset has counts as none.
    #[test]
    fn the_override_submenu_checks_the_scene_s_setting() {
        let kinds = builtin_transition_kinds();
        let t = studio().transitions;
        let (opener, lines) = override_lines(&t, None, &kinds);
        assert_eq!(opener, "Default");
        assert_eq!(lines[0].label, "Default (Cut)");
        assert!(lines[0].checked && lines[0].preset.is_none());
        assert_eq!(lines.len(), 4);
        let fade = TransitionPresetId::new("in");
        let (opener, lines) = override_lines(&t, Some(&fade), &kinds);
        assert_eq!(opener, "Fade in");
        let checked: Vec<_> = lines
            .iter()
            .filter(|l| l.checked)
            .map(|l| l.label.as_str())
            .collect();
        assert_eq!(checked, ["Fade in"]);
        assert_eq!(lines[2].hint.as_deref(), Some("Fade 500 ms"));
        let gone = TransitionPresetId::new("gone");
        assert!(override_lines(&t, Some(&gone), &kinds).1[0].checked);
    }

    #[test]
    fn overrides_are_counted_per_preset() {
        let mut snapshot = studio();
        let fade = TransitionPresetId::new("in");
        snapshot.scenes[0].transition_in = Some(fade.clone());
        snapshot.scenes[1].transition_in = Some(fade.clone());
        snapshot.scenes[1].transition_out = Some(fade.clone());
        assert_eq!(override_counts(&snapshot.scenes, &fade), (2, 1));
        assert_eq!(override_note(&snapshot, &fade), "Overrides: 2 in, 1 out.");
        assert_eq!(
            override_note(&snapshot, &TransitionPresetId::new("cut")),
            "The default: used unless a scene overrides it. Overrides: 0 in, 0 out."
        );
    }

    #[test]
    fn the_editor_starts_on_the_choice_then_the_scene_s_in_then_the_default() {
        let mut snapshot = studio();
        let pick = |s: &Snapshot, chosen: Option<&str>, scene: Option<&str>| {
            let chosen = chosen.map(TransitionPresetId::new);
            let scene = scene.map(SceneId::new);
            initial_pick(s, chosen.as_ref(), scene.as_ref()).map(|p| p.to_string())
        };
        assert_eq!(pick(&snapshot, None, Some("b")).as_deref(), Some("cut"));
        snapshot.scenes[1].transition_in = Some(TransitionPresetId::new("in"));
        assert_eq!(pick(&snapshot, None, Some("b")).as_deref(), Some("in"));
        assert_eq!(
            pick(&snapshot, Some("out"), Some("b")).as_deref(),
            Some("out")
        );
        assert_eq!(pick(&snapshot, Some("gone"), None).as_deref(), Some("cut"));
    }

    #[test]
    fn a_new_preset_is_a_half_second_fade_with_a_free_id() {
        let kinds = builtin_transition_kinds();
        let mut t = TransitionsInfo::default();
        let mut taken = fade("new-preset");
        taken.name = "New preset".into();
        t.presets.push(taken);
        let (id, edit) = new_preset_edit(&t, &kinds);
        assert_eq!(id.as_str(), "new-preset-2");
        let Edit::TransitionCreate {
            name,
            kind,
            duration_ms,
            easing,
            settings,
            ..
        } = edit
        else {
            panic!("a create");
        };
        assert_eq!(
            (name.as_str(), kind.as_str(), duration_ms, easing),
            ("New preset", FADE, 500, Easing::EaseInOut)
        );
        assert_eq!(settings.get("unmatched"), Some(&json!("fade")));
    }
}
