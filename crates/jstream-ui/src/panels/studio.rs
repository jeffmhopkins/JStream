//! The studio-mode panel: the scene on preview, the scene on program, the transition
//! that takes preview to program, and the collection's transition presets with each
//! scene's overrides ([ADR-0029](../../../../docs/adr/0029-transitions.md)).
//!
//! The engine has no studio-mode switch: preview is always there (`studio.set_preview`)
//! and `scene.switch` puts a scene on program with a transition. The panel shows which
//! preset a switch will use, following the engine's order (the one the switch names, the
//! new scene's `transition_in`, the old scene's `transition_out`, the default), so the
//! label under the button matches what the engine does.
//!
//! Docs: crates/jstream-ui/README.md#design

use crate::app::SessionContext;
use crate::panels::outputs::{action, edit};
use crate::session::Session;
use futures_util::StreamExt;
use jstream_control::protocol::{
    Action, CUT, Call, CallResult, Easing, Edit, Event, KindCategory, KindDescriptor,
    ParamDescriptor, ParamType, Query, QueryResult, Rev, SceneId, SceneRole, Settings,
    TransitionPresetId, TransitionPresetInfo, TransitionsInfo, builtin_transition_kinds,
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

/// Why a preset can't be deleted now, or `None` when it can.
pub fn delete_blocker(
    preset: &TransitionPresetInfo,
    transitions: &TransitionsInfo,
) -> Option<&'static str> {
    (preset.preset_id == transitions.default)
        .then_some("It is the default. Make another preset the default first.")
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

/// What a preset does, in a line: its kind, and for every kind but a cut, its duration
/// and easing.
pub fn describe_preset(preset: &TransitionPresetInfo, kinds: &[KindDescriptor]) -> String {
    let kind = kind_name(kinds, &preset.kind);
    if preset.kind == CUT {
        kind
    } else {
        format!(
            "{kind}, {} ms, {}",
            preset.duration_ms,
            easing_label(preset.easing).to_lowercase()
        )
    }
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

/// The `transition.create` edit for a new preset.
pub fn create_edit(preset_id: TransitionPresetId, draft: &Draft) -> Result<Edit, String> {
    Ok(Edit::TransitionCreate {
        preset_id,
        name: parse_name(&draft.name)?,
        kind: draft.kind.clone(),
        duration_ms: parse_duration(&draft.duration)?,
        easing: draft.easing,
        settings: draft.settings.clone(),
    })
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
        spawn_local(async move {
            let loaded = load(&*session.control).await;
            if self.loads.try_get_value() != Some(mine) {
                return;
            }
            match loaded {
                Ok(snapshot) => {
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
        let moved = self
            .snapshot
            .try_update(|snapshot| match snapshot {
                Some(snapshot) => {
                    let moved = snapshot.program != program;
                    snapshot.program.clone_from(&program);
                    snapshot.preview = preview;
                    moved
                }
                None => false,
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
    let choose = move |event: leptos::ev::Event| {
        let value = event_target_value(&event);
        panel
            .chosen
            .set((!value.is_empty()).then(|| TransitionPresetId::new(value)));
    };

    view! {
        <section class="studio" aria-label="Studio mode">
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
            <details class="presets">
                <summary>"Transition presets"</summary>
                <ul class="preset-list">
                    <For each=presets key=|preset| preset.preset_id.clone() let:preset>
                        <PresetRow panel=panel preset_id=preset.preset_id />
                    </For>
                </ul>
                <PresetForm panel=panel editing=None close=RwSignal::new(false) />
                <SceneOverrides panel=panel />
            </details>
        </section>
    }
    .into_any()
}

/// One preset: what it does, whether it is the default, and its buttons. Its form
/// opens in place.
#[component]
fn PresetRow(panel: Panel, preset_id: TransitionPresetId) -> impl IntoView {
    let editing = RwSignal::new(false);
    let preset = {
        let preset_id = preset_id.clone();
        Memo::new(move |_| {
            panel
                .snapshot
                .with(|s| s.as_ref().and_then(|s| s.preset(&preset_id).cloned()))
        })
    };
    let id = StoredValue::new(preset_id);
    let summary = move || {
        preset.with(|preset| {
            let preset = preset.as_ref()?;
            Some(panel.kinds.with(|kinds| describe_preset(preset, kinds)))
        })
    };
    let name = move || preset.with(|preset| preset.as_ref().map(|p| p.name.clone()));
    let is_default = move || {
        panel.snapshot.with(|snapshot| {
            snapshot
                .as_ref()
                .is_some_and(|s| s.transitions.default == id.get_value())
        })
    };
    let blocker = move || {
        panel.snapshot.with(|snapshot| {
            let snapshot = snapshot.as_ref()?;
            delete_blocker(snapshot.preset(&id.get_value())?, &snapshot.transitions)
        })
    };
    let make_default = move |_| {
        let preset_id = id.get_value();
        panel.run(move |session| async move {
            edit(&*session.control, Edit::TransitionSetDefault { preset_id }).await
        });
    };
    let delete = move |_| {
        let preset_id = id.get_value();
        panel.run(move |session| async move {
            edit(&*session.control, Edit::TransitionDelete { preset_id }).await
        });
    };
    view! {
        <li class="preset">
            <div class="preset-head">
                <strong>{name}</strong>
                <Show when=is_default>
                    <span class="badge">"Default"</span>
                </Show>
            </div>
            <p class="hint">{summary}</p>
            <Show
                when=move || editing.get()
                fallback=move || {
                    view! {
                        <div class="buttons row-hover-tools">
                            <button on:click=move |_| editing.set(true)>"Edit"</button>
                            <button disabled=is_default on:click=make_default>
                                "Make default"
                            </button>
                            <button
                                disabled=move || blocker().is_some()
                                title=move || blocker().unwrap_or_default()
                                on:click=delete
                            >
                                "Delete"
                            </button>
                        </div>
                    }
                }
            >
                <PresetForm panel=panel editing=Some(id.get_value()) close=editing />
            </Show>
        </li>
    }
}

/// The form that adds a preset (`editing` is `None`) or changes one. Saving or
/// cancelling a change sets `close` to `false`.
#[component]
fn PresetForm(
    panel: Panel,
    editing: Option<TransitionPresetId>,
    close: RwSignal<bool>,
) -> impl IntoView {
    let fresh = editing
        .as_ref()
        .and_then(|id| {
            panel.snapshot.with_untracked(|s| {
                s.as_ref()
                    .and_then(|s| s.preset(id))
                    .map(Draft::from_preset)
            })
        })
        .unwrap_or_else(|| panel.kinds.with_untracked(|kinds| Draft::new(kinds)));
    let draft = RwSignal::new(fresh);
    let field_problem = RwSignal::new(None::<String>);
    // The parameters whose field holds a value the form refused.
    let invalid = RwSignal::new(BTreeSet::<String>::new());
    let is_new = editing.is_none();
    let editing = StoredValue::new(editing);

    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if invalid.with_untracked(|ids| !ids.is_empty()) {
            field_problem.set(Some("Fix the values the form refused first.".into()));
            return;
        }
        let current = draft.get_untracked();
        let kinds = panel.kinds.get_untracked();
        let built = match editing.get_value() {
            None => {
                let preset_id = panel.snapshot.with_untracked(|s| {
                    let existing: Vec<_> = s
                        .as_ref()
                        .map(|s| {
                            s.transitions
                                .presets
                                .iter()
                                .map(|p| p.preset_id.clone())
                                .collect()
                        })
                        .unwrap_or_default();
                    next_preset_id(existing.iter(), &current.name)
                });
                create_edit(preset_id, &current).map(Some)
            }
            Some(id) => {
                let Some(before) = panel
                    .snapshot
                    .with_untracked(|s| s.as_ref().and_then(|s| s.preset(&id).cloned()))
                else {
                    close.set(false);
                    return;
                };
                update_edit(&before, &current, &kinds)
            }
        };
        let change = match built {
            Ok(change) => change,
            Err(problem) => {
                field_problem.set(Some(problem));
                return;
            }
        };
        field_problem.set(None);
        let Some(change) = change else {
            close.set(false);
            return;
        };
        panel.run(move |session| async move {
            edit(&*session.control, change).await?;
            if is_new {
                draft.try_set(panel.kinds.with_untracked(|kinds| Draft::new(kinds)));
            }
            close.try_set(false);
            Ok(())
        });
    };
    let choose_kind = move |event: leptos::ev::Event| {
        let kind_id = event_target_value(&event);
        panel
            .kinds
            .with_untracked(|kinds| draft.update(|draft| draft.set_kind(kinds, &kind_id)));
        invalid.set(BTreeSet::new());
        field_problem.set(None);
    };
    let choose_easing = move |event: leptos::ev::Event| {
        if let Some(easing) = parse_easing(&event_target_value(&event)) {
            draft.update(|draft| draft.easing = easing);
        }
    };
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
    let name = Signal::derive(move || draft.with(|d| d.name.clone()));
    let duration = Signal::derive(move || draft.with(|d| d.duration.clone()));

    view! {
        <form class="preset-form" on:submit=submit>
            {is_new.then(|| view! { <h3>"Add a preset"</h3> })}
            <label>
                "Name"
                <input
                    type="text"
                    prop:value=name
                    on:input=move |event| {
                        let value = event_target_value(&event);
                        draft.update(|d| d.name = value);
                    }
                />
            </label>
            <label>
                "Kind"
                <select on:change=choose_kind>
                    <For each=move || panel.kinds.get() key=|kind| kind.kind_id.clone() let:kind>
                        {
                            let kind_id = kind.kind_id.clone();
                            view! {
                                <option
                                    value=kind.kind_id.clone()
                                    prop:selected=move || draft.with(|d| d.kind == kind_id)
                                >
                                    {kind.name}
                                </option>
                            }
                        }
                    </For>
                </select>
            </label>
            <label>
                "Duration (ms)"
                <input
                    type="number"
                    min="0"
                    max=jstream_control::protocol::MAX_DURATION_MS.to_string()
                    step="50"
                    prop:value=duration
                    on:input=move |event| {
                        let value = event_target_value(&event);
                        draft.update(|d| d.duration = value);
                    }
                />
            </label>
            <label>
                "Easing"
                <select on:change=choose_easing>
                    {EASINGS
                        .into_iter()
                        .map(|easing| {
                            view! {
                                <option
                                    value=easing_value(easing)
                                    prop:selected=move || draft.with(|d| d.easing == easing)
                                >
                                    {easing_label(easing)}
                                </option>
                            }
                        })
                        .collect_view()}
                </select>
            </label>
            <For each=params key=|param| param.id.clone() let:param>
                <ParamField param=param draft=draft problem=field_problem invalid=invalid />
            </For>
            {move || field_problem.get().map(|problem| view! { <p class="problem" role="alert">{problem}</p> })}
            <div class="buttons">
                <button type="submit" class="primary">
                    {if is_new { "Add" } else { "Save" }}
                </button>
                {(!is_new).then(|| view! {
                    <button type="button" on:click=move |_| close.set(false)>"Cancel"</button>
                })}
            </div>
        </form>
    }
}

/// One of the kind's parameters in the preset form. Enum parameters are a list, booleans
/// a checkbox, numbers and text a field; others are shown read-only.
#[component]
fn ParamField(
    param: ParamDescriptor,
    draft: RwSignal<Draft>,
    problem: RwSignal<Option<String>>,
    invalid: RwSignal<BTreeSet<String>>,
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
        Draft, Reason, SceneRow, Snapshot, create_edit, cut_preset, default_settings,
        delete_blocker, describe_preset, expected_label, expected_preset, load, next_preset_id,
        param_value, parse_duration, parse_easing, swap_action, switch_action, switch_blocker,
        transition_kinds, update_edit,
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
    fn the_default_preset_can_t_be_deleted() {
        let snapshot = studio();
        let cut = &snapshot.transitions.presets[0];
        assert!(delete_blocker(cut, &snapshot.transitions).is_some());
        assert_eq!(delete_blocker(&fade("in"), &snapshot.transitions), None);
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
        assert!(
            create_edit(TransitionPresetId::new("x"), &draft).is_err(),
            "no name"
        );

        let mut draft = draft;
        draft.name = " Quick ".into();
        draft.duration = "250".into();
        let created = create_edit(TransitionPresetId::new("quick"), &draft).expect("valid");
        let Edit::TransitionCreate {
            name,
            kind,
            duration_ms,
            settings,
            ..
        } = created
        else {
            panic!("not a create: {created:?}");
        };
        assert_eq!(
            (name.as_str(), kind.as_str(), duration_ms),
            ("Quick", FADE, 250)
        );
        assert_eq!(settings.get("unmatched"), Some(&json!("fade")));

        draft.duration = "60001".into();
        assert!(create_edit(TransitionPresetId::new("quick"), &draft).is_err());
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
    fn presets_and_easings_read_as_text() {
        let kinds = builtin_transition_kinds();
        assert_eq!(describe_preset(&initial_preset(), &kinds), "Cut");
        assert_eq!(
            describe_preset(&fade("f"), &kinds),
            "Fade, 500 ms, ease out"
        );
        assert_eq!(parse_easing("spring"), Some(Easing::Spring));
        assert_eq!(parse_easing("bounce"), None);
    }
}
