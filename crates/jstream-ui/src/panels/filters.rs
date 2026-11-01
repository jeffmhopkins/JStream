//! Filters (spec-panels.md 5, spec-workspace.md 6.2.6 to 6.4): the selected item's
//! source's video and audio chains (ADR-0026) in the Filters panel and the Properties
//! dialog, a scene's chain and the selected item's own effects (ADR-0065) in the Filters
//! panel, the program output's and each output's chains (ADR-0080) in the Filters panel
//! once an FX button picks them, and the Add filter and Filter settings dialogs over them. A row turns its
//! filter on or off, shows a summary of its settings, opens its settings, drags to
//! another place in its chain and removes it. A filter with three settings or fewer
//! opens in place under its row; one with more opens the Filter settings dialog, whose
//! changes apply at once and which Cancel puts back. Settings forms come from the kind's
//! descriptor (ADR-0025) through the Inspector's form (`properties.rs`).
//!
//! `kind.list` names the engine's filter kinds; the prototype's catalogue gives the
//! groups and hints of the ones it knows. The catalogue, the summary and the choice of
//! in place or dialog are plain functions, tested natively.
//!
//! Docs: docs/UI-FEATURES.md#filters, docs/UI-FEATURES.md#dialogs

use crate::app::SessionContext;
use crate::dialog::Modal;
use crate::dock::PanelId;
use crate::icons::Icon;
use crate::panels::add_source::{has_picture, type_label};
use crate::panels::properties::{Form, draft_patch, value_of};
use crate::panels::tree::{Reorder, Side, dropped_at};
use crate::store::{Collection, Studio, apply};
use crate::workspace::DockState;
use jstream_control::protocol::{
    Edit, FilterChain, FilterId, FilterInfo, FilterOwner, ItemId, KindCategory, KindDescriptor,
    KindSummary, MAX_CHAIN_FILTERS, OutputKind, ParamType, Reference, SceneId, Settings, SourceId,
    SourceInfo, SourceKind,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

/// The most settings a filter has to open in place under its row (F7).
pub const EASY_PARAMS: usize = 3;

/// The prototype's catalogue entry for a filter kind it knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Known {
    chain: FilterChain,
    group: &'static str,
    name: &'static str,
    hint: &'static str,
}

/// The video groups, in the catalogue's order (spec-workspace.md 6.4.7).
const VIDEO_GROUPS: [&str; 7] = [
    "Color and keying",
    "Detail",
    "Style",
    "Motion and timing",
    "Uses another scene",
    "Effect packs",
    "Other",
];

/// The audio groups, in the catalogue's order.
const AUDIO_GROUPS: [&str; 7] = [
    "Levels and dynamics",
    "Cleanup",
    "Tone",
    "Timing",
    "Mastering",
    "Plugins",
    "Other",
];

/// The kinds the prototype's catalogue has, by the engine's kind ID, in its order. The
/// engine's kinds the prototype lacks (pixelate, aspect ratio, image mask) join the
/// group they fit, and its wishes the engine lacks are left out.
const KNOWN: [(&str, Known); 23] = {
    use FilterChain::{Audio, Video};
    const fn k(
        chain: FilterChain,
        group: &'static str,
        name: &'static str,
        hint: &'static str,
    ) -> Known {
        Known {
            chain,
            group,
            name,
            hint,
        }
    }
    const CK: &str = "Color and keying";
    const LD: &str = "Levels and dynamics";
    [
        (
            "color_correction",
            k(
                Video,
                CK,
                "Color correction",
                "Gamma, contrast, saturation, hue",
            ),
        ),
        (
            "apply_lut",
            k(Video, CK, "Apply LUT", "A .cube or .png colour look"),
        ),
        (
            "chroma_key",
            k(Video, CK, "Chroma key", "Remove a green or blue screen"),
        ),
        (
            "color_key",
            k(Video, CK, "Color key", "Remove any one colour"),
        ),
        (
            "luma_key",
            k(Video, CK, "Luma key", "Remove dark or bright areas"),
        ),
        (
            "image_mask",
            k(Video, CK, "Image mask", "Cut out by an image’s shape"),
        ),
        ("sharpen", k(Video, "Detail", "Sharpen", "Crisper detail")),
        (
            "blur",
            k(Video, "Detail", "Blur", "Gaussian, whole image or an area"),
        ),
        (
            "pixelate",
            k(
                Video,
                "Detail",
                "Pixelate",
                "Big square pixels, whole image or an area",
            ),
        ),
        (
            "rounded_corners",
            k(
                Video,
                "Style",
                "Rounded corners",
                "Round the corners, optional border",
            ),
        ),
        (
            "drop_shadow",
            k(Video, "Style", "Drop shadow", "Soft shadow behind"),
        ),
        (
            "outline",
            k(Video, "Style", "Outline", "A stroke around the shape"),
        ),
        (
            "glow",
            k(Video, "Style", "Glow", "Soft light around the shape"),
        ),
        (
            "vignette",
            k(Video, "Style", "Vignette", "Darken or tint the edges"),
        ),
        (
            "aspect_ratio",
            k(
                Video,
                "Style",
                "Aspect ratio",
                "Crop, pad or stretch to a shape",
            ),
        ),
        (
            "scroll",
            k(
                Video,
                "Motion and timing",
                "Scroll",
                "Continuously scroll the image",
            ),
        ),
        (
            "video_delay",
            k(
                Video,
                "Motion and timing",
                "Video delay",
                "Hold the picture back to match audio",
            ),
        ),
        (
            "scene_mask",
            k(
                Video,
                "Uses another scene",
                "Scene mask",
                "Cut out by another scene’s shape",
            ),
        ),
        ("gain", k(Audio, LD, "Gain", "Turn it up or down")),
        (
            "compressor",
            k(Audio, LD, "Compressor", "Even out loud and quiet"),
        ),
        ("limiter", k(Audio, LD, "Limiter", "Never go over a level")),
        (
            "noise_gate",
            k(Audio, LD, "Noise gate", "Silence below a level"),
        ),
        (
            "delay",
            k(
                Audio,
                "Timing",
                "Audio delay",
                "Hold the sound back to match video",
            ),
        ),
    ]
};

fn known(kind: &str) -> Option<(usize, Known)> {
    KNOWN
        .iter()
        .position(|(id, _)| *id == kind)
        .map(|at| (at, KNOWN[at].1))
}

/// A filter kind to offer in the Add filter dialog.
#[derive(Clone, Debug, PartialEq)]
pub struct FilterType {
    /// The kind's ID.
    pub kind: String,
    /// The chain it goes on.
    pub chain: FilterChain,
    /// Its group in the catalogue.
    pub group: &'static str,
    /// Its name.
    pub name: String,
    /// What it does, in a few words; empty when the prototype doesn't know it.
    pub hint: &'static str,
}

/// The chain a filter kind's category puts it on, if it is a filter kind.
fn chain_of(category: KindCategory) -> Option<FilterChain> {
    match category {
        KindCategory::VideoFilter => Some(FilterChain::Video),
        KindCategory::AudioFilter => Some(FilterChain::Audio),
        _ => None,
    }
}

/// The prefix of a CLAP plugin's kind, `clap/<plugin id>` (ADR-0035): one kind per
/// audio effect the engine's scan found.
const CLAP_PREFIX: &str = "clap/";

/// The filter type `kind_id` names, from `kind.list`'s summary when it has one: the
/// prototype's name, group and hint when it knows the kind, a CLAP plugin's kind
/// (`clap/<plugin id>`) under Plugins by the plugin's name, an effect pack's kind
/// (`pack/effect`) under Effect packs, and anything else under Other by its engine name.
fn filter_type(kind_id: &str, chain: FilterChain, engine_name: Option<&str>) -> FilterType {
    let fallback = engine_name.unwrap_or(kind_id).to_owned();
    match known(kind_id) {
        Some((_, k)) => FilterType {
            kind: kind_id.to_owned(),
            chain: k.chain,
            group: k.group,
            name: k.name.to_owned(),
            hint: k.hint,
        },
        None if chain == FilterChain::Audio && kind_id.starts_with(CLAP_PREFIX) => FilterType {
            kind: kind_id.to_owned(),
            chain,
            group: "Plugins",
            name: fallback,
            hint: "A CLAP audio effect",
        },
        None => FilterType {
            kind: kind_id.to_owned(),
            chain,
            group: if kind_id.contains('/') {
                "Effect packs"
            } else {
                "Other"
            },
            name: fallback,
            hint: if kind_id.contains('/') {
                "From an effect pack"
            } else {
                ""
            },
        },
    }
}

/// A filter's name: the prototype's for a kind it knows, else `kind.list`'s, else its
/// kind ID.
pub fn filter_name(kinds: &[KindSummary], kind_id: &str) -> String {
    if let Some((_, k)) = known(kind_id) {
        return k.name.to_owned();
    }
    kinds
        .iter()
        .find(|k| k.kind_id == kind_id && chain_of(k.category).is_some())
        .map_or_else(|| kind_id.to_owned(), |k| k.name.clone())
}

/// The filter kinds to offer for an owner with these chains: every filter kind
/// `kind.list` reported for them, video first, in the catalogue's group order and the
/// prototype's order within a group, then by name.
pub fn catalogue(kinds: &[KindSummary], video: bool, audio: bool) -> Vec<FilterType> {
    let mut types: Vec<FilterType> = kinds
        .iter()
        .filter_map(|k| {
            let chain = chain_of(k.category)?;
            let wanted = match chain {
                FilterChain::Video => video,
                FilterChain::Audio => audio,
            };
            wanted.then(|| filter_type(&k.kind_id, chain, Some(&k.name)))
        })
        .collect();
    let rank = |t: &FilterType| {
        let groups: &[&str] = match t.chain {
            FilterChain::Video => &VIDEO_GROUPS,
            FilterChain::Audio => &AUDIO_GROUPS,
        };
        (
            t.chain == FilterChain::Audio,
            groups
                .iter()
                .position(|g| *g == t.group)
                .unwrap_or(groups.len()),
            known(&t.kind).map_or(KNOWN.len(), |(at, _)| at),
        )
    };
    types.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name)));
    types
}

/// Whether a type matches the dialog's search: its name, hint or group, in any case.
pub fn type_matches(t: &FilterType, search: &str) -> bool {
    let search = search.trim().to_lowercase();
    search.is_empty()
        || [t.name.as_str(), t.hint, t.group]
            .iter()
            .any(|s| s.to_lowercase().contains(&search))
}

/// How a filter's settings open from its row (F7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opens {
    /// It has no settings.
    Nothing,
    /// In place, under its row: it has [`EASY_PARAMS`] settings or fewer.
    InPlace,
    /// In the Filter settings dialog.
    Dialog,
}

/// How the settings of a filter of this kind open.
pub fn opens(kind: &KindDescriptor) -> Opens {
    match kind.params.len() {
        0 => Opens::Nothing,
        n if n <= EASY_PARAMS => Opens::InPlace,
        _ => Opens::Dialog,
    }
}

/// What a row says about a filter's settings (F6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Summary {
    /// Nothing: it has no settings, or its kind isn't described yet.
    Empty,
    /// A value, or "Label value · Label value", or "defaults".
    Text(String),
    /// The scene a scene-input filter uses, by name.
    Scene(String),
    /// A scene-input filter with no scene chosen.
    NoScene,
}

/// The number of decimals a number of this step shows: as many as the step has, and up
/// to two with no step.
fn decimals(step: Option<f64>) -> usize {
    match step {
        Some(step) if step > 0.0 && step < 1.0 => {
            let text = format!("{step}");
            text.split('.').nth(1).map_or(0, str::len).min(4)
        }
        Some(_) => 0,
        None => 2,
    }
}

/// A number as a summary shows it: rounded to its step's decimals, with a true minus
/// sign, then its unit (`%` and `°` attached, others after a space).
pub fn number_text(value: &Value, step: Option<f64>, integer: bool, unit: Option<&str>) -> String {
    let n = value.as_f64().unwrap_or_default();
    let text = if integer {
        format!("{}", n.round())
    } else {
        let d = decimals(step);
        let fixed = format!("{n:.d$}");
        if step.is_none() && fixed.contains('.') {
            fixed.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            fixed
        }
    };
    // `-0` reads as 0.
    let text = if text
        .trim_start_matches('-')
        .chars()
        .all(|c| c == '0' || c == '.')
    {
        text.trim_start_matches('-').to_owned()
    } else {
        text.replacen('-', "−", 1)
    };
    match unit {
        Some(u @ ("%" | "°")) => format!("{text}{u}"),
        Some(u) if !u.is_empty() => format!("{text} {u}"),
        _ => text,
    }
}

/// A numeric parameter's value as a summary shows it, or `None` for another type.
fn param_number(
    param: &jstream_control::protocol::ParamDescriptor,
    value: &Value,
) -> Option<String> {
    match &param.ty {
        ParamType::Int { step, unit, .. } => Some(number_text(
            value,
            step.map(|s| s as f64),
            true,
            unit.as_deref(),
        )),
        ParamType::Float { step, unit, .. } => {
            Some(number_text(value, *step, false, unit.as_deref()))
        }
        _ => None,
    }
}

/// The summary of a filter with these settings (F6): a scene-input filter's scene; a
/// filter whose one setting is a number, that value; otherwise up to two numbers changed
/// from their defaults, or "defaults".
pub fn summary(
    kind: &KindDescriptor,
    settings: &Settings,
    scene_name: impl Fn(&SceneId) -> Option<String>,
) -> Summary {
    if let Some(scene) = kind.params.iter().find(|p| p.ty == ParamType::Scene) {
        return match Reference::from_value(value_of(scene, settings)) {
            Some(Reference::Scene { scene_id }) => {
                Summary::Scene(scene_name(&scene_id).unwrap_or_else(|| scene_id.to_string()))
            }
            _ => Summary::NoScene,
        };
    }
    match kind.params.as_slice() {
        [] => Summary::Empty,
        [one] => match param_number(one, value_of(one, settings)) {
            Some(text) => Summary::Text(text),
            None => Summary::Text(changed_or_defaults(kind, settings)),
        },
        _ => Summary::Text(changed_or_defaults(kind, settings)),
    }
}

fn changed_or_defaults(kind: &KindDescriptor, settings: &Settings) -> String {
    let changed: Vec<String> = kind
        .params
        .iter()
        .filter(|p| *value_of(p, settings) != p.default)
        .filter_map(|p| {
            param_number(p, value_of(p, settings)).map(|text| format!("{} {text}", p.label))
        })
        .take(2)
        .collect();
    if changed.is_empty() {
        "defaults".to_owned()
    } else {
        changed.join(" · ")
    }
}

/// The scenes a scene-input filter on `source_id` may use (F8): every scene but the one
/// being edited and those that place the source, whose picture would then contain
/// itself. The engine refuses any other cycle.
pub fn scene_choices(
    collection: &Collection,
    editing: Option<&SceneId>,
    source_id: &SourceId,
) -> Vec<(SceneId, String)> {
    collection
        .listed_scenes()
        .filter(|s| Some(&s.scene_id) != editing)
        .filter(|s| !s.items.iter().any(|i| i.source_id == *source_id))
        .map(|s| (s.scene_id.clone(), s.name.clone()))
        .collect()
}

/// The chains a source has (ADR-0026): video unless its kind is audio-only, audio when
/// it has a mixer channel and isn't a nested scene.
pub fn chains_of(source: &SourceInfo) -> (bool, bool) {
    match &source.kind {
        SourceKind::Builtin { kind } => (has_picture(kind), source.audio.is_some()),
        SourceKind::Scene { .. } => (true, false),
        _ => (true, source.audio.is_some()),
    }
}

/// The type icon and name of a source, for the panel's header.
fn source_icon(source: &SourceInfo) -> &'static str {
    match &source.kind {
        SourceKind::Builtin { kind } => type_label(kind).map_or("image", |(_, icon)| icon),
        _ => "layers",
    }
}

/// Whether an output carries sound, and so has an audio chain: a stream and a recording
/// do, the virtual camera doesn't.
pub fn output_has_audio(kind: &OutputKind) -> bool {
    matches!(kind, OutputKind::Rtmp { .. } | OutputKind::Record { .. })
}

/// The chains `owner` shows, as (video, audio). A source's follow its kind
/// ([`chains_of`]); a scene has a video chain, and so does an item whose source has a
/// picture: its effects (ADR-0065). The program output and an output show their
/// audio chain only, since the engine refuses their video chains so far, and an output
/// without sound (the virtual camera) has none.
pub fn owner_chains(collection: &Collection, owner: &FilterOwner) -> (bool, bool) {
    match owner {
        FilterOwner::Source { source_id } => collection
            .source(source_id)
            .map_or((false, false), chains_of),
        FilterOwner::Master => (false, true),
        FilterOwner::Output { output_id } => (
            false,
            collection
                .output(output_id)
                .is_some_and(|o| output_has_audio(&o.kind)),
        ),
        FilterOwner::Scene { .. } => (true, false),
        FilterOwner::Item { item_id } => (
            collection
                .item(item_id)
                .and_then(|(_, item)| collection.source(&item.source_id))
                .is_some_and(|source| chains_of(source).0),
            false,
        ),
        _ => (false, false),
    }
}

/// What the panel and the dialogs call `owner`: a source's or a scene's name, an item's
/// source's name, "Program output" for the master stage, and an output's ID, as the
/// Outputs panel shows it.
pub fn owner_name(collection: &Collection, owner: &FilterOwner) -> String {
    match owner {
        FilterOwner::Source { source_id } => collection.source_name(source_id),
        FilterOwner::Master => "Program output".into(),
        FilterOwner::Output { output_id } => output_id.to_string(),
        FilterOwner::Scene { scene_id } => collection
            .scene(scene_id)
            .map_or_else(|| scene_id.to_string(), |s| s.name.clone()),
        FilterOwner::Item { item_id } => collection.item(item_id).map_or_else(
            || item_id.to_string(),
            |(_, item)| collection.source_name(&item.source_id),
        ),
        _ => String::new(),
    }
}

/// The note under the panel's title (F2): where `owner`'s chains apply.
pub fn owner_note(collection: &Collection, owner: &FilterOwner) -> &'static str {
    match owner {
        FilterOwner::Master => "Master chain: applies to everything leaving JStream.",
        FilterOwner::Output { .. } if owner_chains(collection, owner).1 => {
            "Only this output, after the program filters."
        }
        FilterOwner::Output { .. } => "Only this output, after the program filters. No sound.",
        FilterOwner::Scene { .. } => "Scene filters apply to the whole scene, top to bottom.",
        FilterOwner::Item { .. } => "Only this item, after its crop, top to bottom.",
        _ => "Applies everywhere this source appears, top to bottom.",
    }
}

/// The Add filter dialog's foot: where a filter added to `owner` applies.
fn owner_foot(owner: &FilterOwner) -> &'static str {
    match owner {
        FilterOwner::Master => "Applies to everything leaving JStream.",
        FilterOwner::Output { .. } => "Applies to this output only, after the program filters.",
        FilterOwner::Scene { .. } => "Applies to the whole scene.",
        FilterOwner::Item { .. } => "Applies to this item only, after its crop.",
        _ => "Applies everywhere this source appears.",
    }
}

/// The Add filter dialog's subtitle for `owner`, which `name` names: "For “Backdrop”", or
/// for an item's effects "For this item of “Backdrop”", as its source is shared.
fn add_sub(owner: &FilterOwner, name: &str) -> String {
    match owner {
        FilterOwner::Item { .. } => format!("For this item of “{name}”"),
        _ => format!("For “{name}”"),
    }
}

/// The panel header's icon for `owner`.
fn owner_icon(collection: &Collection, owner: &FilterOwner) -> &'static str {
    match owner {
        FilterOwner::Source { source_id } => {
            collection.source(source_id).map_or("image", source_icon)
        }
        FilterOwner::Master | FilterOwner::Output { .. } => "out",
        _ => "layers",
    }
}

/// The source a filter's settings form leaves out of its source choices: the owner's
/// own, and none (an ID no source has) for any other owner.
fn own_source(owner: &FilterOwner) -> SourceId {
    owner
        .source_id()
        .cloned()
        .unwrap_or_else(|| SourceId::new(""))
}

/// The owner of the chain `scene_id` shows (FL-06): the scene's own, or for a keyframe
/// scene its parent's, since a keyframe has no filters of its own (ADR-0065 §3).
pub fn scene_owner(collection: &Collection, scene_id: &SceneId) -> FilterOwner {
    let scene_id = collection
        .parent_of(scene_id)
        .map_or(scene_id, |parent| &parent.scene_id)
        .clone();
    FilterOwner::Scene { scene_id }
}

/// Whether `scene_id`'s picture holds `inner`'s: it is `inner`, a keyframe of a scene that
/// holds it, or it places a nested scene that does.
fn holds_scene(collection: &Collection, scene_id: &SceneId, inner: &SceneId) -> bool {
    fn walk(c: &Collection, at: &SceneId, inner: &SceneId, seen: &mut HashSet<SceneId>) -> bool {
        if at == inner {
            return true;
        }
        if !seen.insert(at.clone()) {
            return false;
        }
        let Some(scene) = c.scene(at) else {
            return false;
        };
        if let Some(parent) = &scene.variant_of
            && walk(c, parent, inner, seen)
        {
            return true;
        }
        scene.items.iter().any(|item| {
            matches!(
                c.source(&item.source_id).map(|s| &s.kind),
                Some(SourceKind::Scene { scene_id }) if walk(c, scene_id, inner, seen)
            )
        })
    }
    walk(collection, scene_id, inner, &mut HashSet::new())
}

/// The scenes a scene-input filter on `owner` may use (F8). For a scene's chain, every
/// scene whose picture doesn't hold the scene itself; for an item's effects, every scene
/// whose picture doesn't hold the item's scene, since the reference is an edge from that
/// scene (ADR-0065 §3); for any other owner, [`scene_choices`]'.
pub fn owner_scene_choices(
    collection: &Collection,
    owner: &FilterOwner,
    editing: Option<&SceneId>,
) -> Vec<(SceneId, String)> {
    match owner {
        FilterOwner::Scene { scene_id } => collection
            .listed_scenes()
            .filter(|s| !holds_scene(collection, &s.scene_id, scene_id))
            .map(|s| (s.scene_id.clone(), s.name.clone()))
            .collect(),
        FilterOwner::Item { item_id } => {
            let Some((home, _)) = collection.item(item_id) else {
                return Vec::new();
            };
            collection
                .listed_scenes()
                .filter(|s| !holds_scene(collection, &s.scene_id, &home.scene_id))
                .map(|s| (s.scene_id.clone(), s.name.clone()))
                .collect()
        }
        _ => scene_choices(collection, editing, &own_source(owner)),
    }
}

/// Shows `owner`'s chains in the Filters panel, as a scene's Edit filters… does.
pub fn show_filters(dialogs: Option<FilterDialogs>, dock: Option<DockState>, owner: FilterOwner) {
    if let Some(dialogs) = dialogs {
        dialogs.pick(owner);
    }
    if let Some(dock) = dock {
        dock.0.update(|d| d.show(PanelId::Filters));
    }
}

/// A filter of an owner, by its ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The chain's owner.
    pub owner: FilterOwner,
    /// The filter.
    pub filter_id: FilterId,
}

/// The filter dialogs' state as context: the owner the Add filter dialog adds to, the
/// filter the Filter settings dialog edits, a filter just added, which its row opens in
/// place, and the owner an FX button picked for the Filters panel.
#[derive(Clone, Copy)]
pub struct FilterDialogs {
    add: RwSignal<Option<FilterOwner>>,
    edit: RwSignal<Option<Target>>,
    fresh: RwSignal<Option<FilterId>>,
    picked: RwSignal<Option<FilterOwner>>,
}

impl FilterDialogs {
    /// Both dialogs closed, and nothing picked.
    pub fn new() -> FilterDialogs {
        FilterDialogs {
            add: RwSignal::new(None),
            edit: RwSignal::new(None),
            fresh: RwSignal::new(None),
            picked: RwSignal::new(None),
        }
    }

    /// Opens the Add filter dialog for `owner`'s chains.
    pub fn add(&self, owner: FilterOwner) {
        self.add.set(Some(owner));
    }

    /// Opens the Filter settings dialog on a filter.
    pub fn edit(&self, owner: FilterOwner, filter_id: FilterId) {
        self.edit.set(Some(Target { owner, filter_id }));
    }

    /// Has the Filters panel show `owner`'s chains (the program output's or an
    /// output's, from its FX button, or a scene's, from its Edit filters…) until another
    /// item or scene is selected or [`unpick`] runs.
    ///
    /// [`unpick`]: FilterDialogs::unpick
    pub fn pick(&self, owner: FilterOwner) {
        self.picked.set(Some(owner));
    }

    /// Has the Filters panel show the selection's chains again: the selected item's
    /// source's, or with no item the selected scene's.
    pub fn unpick(&self) {
        self.picked.set(None);
    }

    /// The owner an FX button picked, read reactively.
    pub fn picked(&self) -> Option<FilterOwner> {
        self.picked.get()
    }
}

impl Default for FilterDialogs {
    fn default() -> FilterDialogs {
        FilterDialogs::new()
    }
}

/// An owner's filter, and the chain it is on, read reactively.
fn filter_of(studio: Studio, target: &Target) -> Option<(FilterChain, FilterInfo)> {
    studio.collection.with(|c| {
        let chains = c.as_ref()?.chains(&target.owner)?;
        [FilterChain::Video, FilterChain::Audio]
            .into_iter()
            .find_map(|chain| {
                chains
                    .chain(chain)
                    .iter()
                    .find(|f| f.filter_id == target.filter_id)
                    .map(|f| (chain, f.clone()))
            })
    })
}

/// Reads the descriptor of each kind in `kinds` not yet read, once each: the mock
/// engine and an engine without a kind describe none, and a refusal isn't news.
fn describe_all(
    studio: Studio,
    session: SessionContext,
    kinds: Vec<String>,
    tried: StoredValue<HashSet<String>>,
) {
    let Some(s) = session.get() else {
        return;
    };
    for kind in kinds {
        let fresh = tried
            .try_update_value(|t| t.insert(kind.clone()))
            .unwrap_or(false);
        let known = studio.descriptors.with_untracked(|d| d.contains_key(&kind));
        if fresh && !known {
            let s = s.clone();
            spawn_local(async move {
                let _ = studio.descriptor(&s, &kind).await;
            });
        }
    }
}

/// The Filters panel (spec-panels.md 5): the chains an FX button or a scene's Edit
/// filters… picked (FL-07, FL-06), else the selected item's source's with the item's own
/// effects under them (FL-09), else the selected scene's (FL-06).
#[component]
pub fn Filters() -> impl IntoView {
    let studio = expect_context::<Studio>();
    let dialogs = expect_context::<FilterDialogs>();
    let selected = Memo::new(move |_| studio.selection.with(|s| (s.scene.clone(), s.item.clone())));
    // Selecting another item or scene shows its chains again.
    Effect::new(move |before: Option<(Option<SceneId>, Option<ItemId>)>| {
        let now = selected.get();
        if before.is_some_and(|before| before != now) {
            dialogs.unpick();
        }
        now
    });
    // The owner shown, with its header: icon, name and note.
    let shown = Memo::new(move |_| {
        let picked = dialogs.picked();
        let (scene, item) = selected.get();
        studio.collection.with(|c| {
            let c = c.as_ref()?;
            let owner = match picked.filter(|p| c.chains(p).is_some()) {
                Some(owner) => owner,
                None => match item.as_ref().and_then(|item| c.item(item)) {
                    Some((_, item)) => {
                        c.source(&item.source_id)?;
                        FilterOwner::Source {
                            source_id: item.source_id.clone(),
                        }
                    }
                    None => {
                        let owner = scene_owner(c, scene.as_ref()?);
                        c.chains(&owner)?;
                        owner
                    }
                },
            };
            let head = (
                owner_icon(c, &owner),
                owner_name(c, &owner),
                owner_note(c, &owner),
            );
            Some((owner_chains(c, &owner), head, owner))
        })
    });
    let chains =
        Memo::new(move |_| shown.with(|s| s.as_ref().map(|(chains, _, o)| (o.clone(), *chains))));
    // The selected item's own effects (ADR-0065), under its source's chains, while the
    // panel shows that source's and the item has a picture.
    let effects = Memo::new(move |_| {
        let source = shown.with(|s| matches!(s, Some((_, _, FilterOwner::Source { .. }))));
        let (_, item) = selected.get();
        let owner = FilterOwner::Item { item_id: item? };
        let video = studio
            .collection
            .with(|c| c.as_ref().is_some_and(|c| owner_chains(c, &owner).0));
        (source && video).then_some(owner)
    });
    view! {
        <section class="panel filters-panel">
            {move || match shown.with(|s| s.as_ref().map(|(_, head, _)| head.clone())) {
                None => view! {
                    <div class="filters-head">
                        <Icon name="layers" size=18 />
                        <div class="filters-titles">
                            <span class="filters-title">"Nothing selected"</span>
                        </div>
                    </div>
                    <p class="placeholder">"Select a source or a scene."</p>
                }
                .into_any(),
                Some((icon, name, note)) => view! {
                    <div class="filters-head">
                        <Icon name=icon size=18 />
                        <div class="filters-titles">
                            <span class="filters-title">{name}</span>
                            <span class="filters-note">{note}</span>
                        </div>
                    </div>
                }
                .into_any(),
            }}
            {move || {
                chains.get().map(|(owner, (video, audio))| {
                    let stage = matches!(owner, FilterOwner::Master | FilterOwner::Output { .. });
                    let add = {
                        let owner = owner.clone();
                        move |_| dialogs.add(owner.clone())
                    };
                    view! {
                        <Chains owner=owner video=video audio=audio in_place=true />
                        {stage.then(|| view! {
                            <p class="filters-later">"Video filters on outputs aren’t available yet."</p>
                        })}
                        {(video || audio).then(|| view! {
                            <button type="button" class="filters-add" on:click=add>"+ Add filter…"</button>
                        })}
                    }
                })
            }}
            {move || {
                effects.get().map(|owner| {
                    let add = {
                        let owner = owner.clone();
                        move |_| dialogs.add(owner.clone())
                    };
                    view! {
                        <div class="filters-effects">
                            <div class="filters-titles">
                                <span class="filters-subtitle">"This item’s effects"</span>
                                <span class="filters-effects-note">
                                    {studio.collection.with_untracked(|c| {
                                        c.as_ref().map(|c| owner_note(c, &owner)).unwrap_or_default()
                                    })}
                                </span>
                            </div>
                            <Chains owner=owner.clone() video=true audio=false in_place=true />
                            <button type="button" class="filters-add" on:click=add>"+ Add effect…"</button>
                        </div>
                    }
                })
            }}
        </section>
    }
}

/// An owner's chains: a section per chain with its count and rows, each change sent at
/// once. With `in_place` (the Filters panel) a filter with few settings opens under its
/// row and a click opens the settings; in the Properties dialog a double-click does.
#[component]
pub fn Chains(
    /// The chains' owner.
    owner: FilterOwner,
    /// Whether it has a video chain.
    video: bool,
    /// Whether it has an audio chain.
    audio: bool,
    /// Whether rows open their settings in place.
    #[prop(optional)]
    in_place: bool,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let dialogs = expect_context::<FilterDialogs>();
    let chains = {
        let owner = owner.clone();
        Memo::new(move |_| {
            studio.collection.with(|c| {
                c.as_ref()
                    .and_then(|c| c.chains(&owner))
                    .map(Cow::into_owned)
                    .unwrap_or_default()
            })
        })
    };
    let tried = StoredValue::new(HashSet::<String>::new());
    Effect::new(move |_| {
        let kinds: HashSet<String> = chains.with(|c| {
            c.video
                .iter()
                .chain(&c.audio)
                .map(|f| f.kind.clone())
                .collect()
        });
        describe_all(studio, session, kinds.into_iter().collect(), tried);
    });
    // The filter open in place, and a just-added one, which opens once it shows.
    let expanded = RwSignal::new(None::<FilterId>);
    if in_place {
        Effect::new(move |_| {
            let Some(fresh) = dialogs.fresh.get() else {
                return;
            };
            let here =
                chains.with(|c| c.video.iter().chain(&c.audio).any(|f| f.filter_id == fresh));
            if here {
                expanded.set(Some(fresh));
                dialogs.fresh.set(None);
            }
        });
    }
    let reorder = Reorder::new();
    let drop = {
        let owner = owner.clone();
        move |moved: String, target: String, side: Side| {
            let (moved, target) = (FilterId::new(moved), FilterId::new(target));
            let index = chains.with_untracked(|c| {
                [&c.video, &c.audio].into_iter().find_map(|chain| {
                    let order: Vec<&FilterId> = chain.iter().map(|f| &f.filter_id).collect();
                    dropped_at(&order, &&moved, &&target, side)
                })
            });
            if let (Some(index), Some(s)) = (index, session.get()) {
                studio.apply(
                    &s,
                    vec![Edit::FilterReorder {
                        owner: owner.clone(),
                        filter_id: moved,
                        index,
                    }],
                );
            }
        }
    };
    let effects = matches!(owner, FilterOwner::Item { .. });
    let chain = move |which: FilterChain| {
        let (title, icon, empty) = chain_head(effects, which);
        let list = Memo::new(move |_| chains.with(|c| c.chain(which).to_vec()));
        let count = move || count_text(effects, list.with(Vec::len));
        let owner = owner.clone();
        let drop = drop.clone();
        view! {
            <div class="filter-chain" data-chain=which.as_str()>
                <div class="filter-chain-head">
                    <Icon name=icon size=14 />
                    <span class="filter-chain-title">{title}</span>
                    <span class="filter-chain-count">{count}</span>
                </div>
                <Show
                    when=move || list.with(|l| !l.is_empty())
                    fallback=move || view! { <p class="filter-empty">{empty}</p> }
                >
                    <ul
                        class="filter-rows"
                        aria-label=if effects { "Item effects".to_owned() } else { format!("{title} filters") }
                        class:reordering=move || reorder.dragged.with(Option::is_some)
                        on:pointermove=move |event| reorder.moved(&event)
                        on:pointerup={
                            let drop = drop.clone();
                            move |_| reorder.up(drop.clone())
                        }
                        on:pointercancel=move |_| reorder.cancel()
                    >
                        <For
                            each=move || list.get()
                            key=|f| (f.filter_id.clone(), f.enabled, f.kind.clone())
                            children={
                                let owner = owner.clone();
                                move |f: FilterInfo| view! {
                                    <Row
                                        owner=owner.clone()
                                        filter=f
                                        in_place=in_place
                                        expanded=expanded
                                        reorder=reorder
                                    />
                                }
                            }
                        />
                    </ul>
                </Show>
            </div>
        }
    };
    view! {
        <div class="filter-chains" class:in-place=in_place>
            {video.then(|| chain(FilterChain::Video))}
            {audio.then(|| chain(FilterChain::Audio))}
        </div>
    }
}

/// A chain's heading, icon and empty text: an item's effects (ADR-0065) are "Effects",
/// any other owner's chains "Video" and "Audio".
fn chain_head(effects: bool, which: FilterChain) -> (&'static str, &'static str, &'static str) {
    match (effects, which) {
        (true, _) => ("Effects", "monitor", "No effects."),
        (false, FilterChain::Video) => ("Video", "monitor", "No video filters."),
        (false, FilterChain::Audio) => ("Audio", "speaker", "No audio filters."),
    }
}

/// A chain's count: "1 filter" or "N filters", or for an item's effects "1 effect" or
/// "N effects".
fn count_text(effects: bool, n: usize) -> String {
    let word = if effects { "effect" } else { "filter" };
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// One filter's row (F5), and its settings under it while open in place (F8).
#[component]
fn Row(
    owner: FilterOwner,
    filter: FilterInfo,
    in_place: bool,
    expanded: RwSignal<Option<FilterId>>,
    reorder: Reorder,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let dialogs = expect_context::<FilterDialogs>();
    let name = studio
        .kinds
        .with_untracked(|k| filter_name(k.as_deref().unwrap_or_default(), &filter.kind));
    let filter_id = filter.filter_id.clone();
    let enabled = filter.enabled;
    let target = Target {
        owner: owner.clone(),
        filter_id: filter_id.clone(),
    };
    let descriptor = {
        let kind = filter.kind.clone();
        Memo::new(move |_| studio.descriptors.with(|d| d.get(&kind).cloned()))
    };
    let settings = {
        let target = target.clone();
        Signal::derive(move || {
            filter_of(studio, &target)
                .map(|(_, f)| f.settings)
                .unwrap_or_default()
        })
    };
    let summary = Memo::new(move |_| {
        let Some(kind) = descriptor.get() else {
            return Summary::Empty;
        };
        settings.with(|s| {
            summary(&kind, s, |id| {
                studio
                    .collection
                    .with(|c| c.as_ref().and_then(|c| c.scene(id)).map(|s| s.name.clone()))
            })
        })
    });
    let how = move || descriptor.with(|d| d.as_ref().map_or(Opens::Nothing, opens));
    let send = move |edit: Edit| {
        if let Some(s) = session.get() {
            studio.apply(&s, vec![edit]);
        }
    };
    let toggle = {
        let owner = owner.clone();
        let filter_id = filter_id.clone();
        move |event: leptos::ev::MouseEvent| {
            event.stop_propagation();
            send(Edit::FilterSetEnabled {
                owner: owner.clone(),
                filter_id: filter_id.clone(),
                enabled: !enabled,
            });
        }
    };
    let remove = {
        let owner = owner.clone();
        let filter_id = filter_id.clone();
        move |event: leptos::ev::MouseEvent| {
            event.stop_propagation();
            send(Edit::FilterRemove {
                owner: owner.clone(),
                filter_id: filter_id.clone(),
            });
        }
    };
    let open_dialog = {
        let target = target.clone();
        move || dialogs.edit(target.owner.clone(), target.filter_id.clone())
    };
    let is_open = {
        let filter_id = filter_id.clone();
        move || in_place && expanded.with(|e| e.as_ref() == Some(&filter_id))
    };
    let on_click = {
        let filter_id = filter_id.clone();
        let open_dialog = open_dialog.clone();
        move |_| {
            if !in_place {
                return;
            }
            match how() {
                Opens::InPlace => expanded.update(|e| {
                    *e = if e.as_ref() == Some(&filter_id) {
                        None
                    } else {
                        Some(filter_id.clone())
                    };
                }),
                Opens::Dialog => open_dialog(),
                Opens::Nothing => {}
            }
        }
    };
    let on_dblclick = {
        let open_dialog = open_dialog.clone();
        move |_| open_dialog()
    };
    let title = move || match (in_place, how()) {
        (true, Opens::InPlace) => "Click to adjust here · double-click for the full dialog",
        (true, Opens::Dialog) => "Click to open its settings",
        _ => "Double-click for its settings · drag to reorder",
    };
    let caret = {
        let is_open = is_open.clone();
        move || match (in_place, how()) {
            (true, Opens::InPlace) => Some(if is_open() { "▾" } else { "▸" }),
            (true, Opens::Dialog) => Some("⤢"),
            _ => None,
        }
    };
    let key = filter_id.to_string();
    let drop_side = {
        let key = key.clone();
        move |side: Side| {
            let key = key.clone();
            move || {
                reorder
                    .target
                    .with(|t| t.as_ref().is_some_and(|(id, s)| *id == key && *s == side))
            }
        }
    };
    let dragging = {
        let key = key.clone();
        move || reorder.dragged.with(|d| d.as_deref() == Some(key.as_str()))
    };
    let press = {
        let key = key.clone();
        move |event: leptos::ev::PointerEvent| reorder.down(&key, &event)
    };
    let eye = if enabled {
        format!("Turn off {name}")
    } else {
        format!("Turn on {name}")
    };
    let remove_label = format!("Remove {name}");
    let summary_view = move || match summary.get() {
        Summary::Empty => ().into_any(),
        Summary::Text(text) => view! { <span class="filter-summary">{text}</span> }.into_any(),
        Summary::Scene(scene) => {
            view! { <span class="filter-summary scene">{scene}</span> }.into_any()
        }
        Summary::NoScene => {
            view! { <span class="filter-summary no-scene">"no scene"</span> }.into_any()
        }
    };
    let editor = {
        let is_open = is_open.clone();
        let owner = owner.clone();
        let filter_id = filter_id.clone();
        move || {
            if !is_open() {
                return None;
            }
            let kind = descriptor.get()?;
            Some(view! {
                <Editor
                    kind=kind
                    owner=owner.clone()
                    filter_id=filter_id.clone()
                    settings=settings
                />
            })
        }
    };
    view! {
        <li
            class="filter"
            class:off=!enabled
            class:open=is_open
            class:drop-before=drop_side(Side::Before)
            class:drop-after=drop_side(Side::After)
            class:dragging=dragging
            data-tree-id=key
        >
            <div
                class="filter-row"
                title=title
                on:pointerdown=press
                on:click=on_click
                on:dblclick=on_dblclick
            >
                <button
                    type="button"
                    class="filter-eye"
                    aria-label=eye
                    aria-pressed=enabled.to_string()
                    on:click=toggle
                    on:dblclick=|event| event.stop_propagation()
                >
                    <Icon name=if enabled { "eye" } else { "eye-off" } size=14 />
                </button>
                <span class="filter-name">{name.clone()}</span>
                {summary_view}
                {move || caret().map(|c| view! { <span class="filter-caret" aria-hidden="true">{c}</span> })}
                <button
                    type="button"
                    class="hov filter-remove"
                    title="Remove"
                    aria-label=remove_label
                    on:click=remove
                    on:dblclick=|event| event.stop_propagation()
                >
                    "×"
                </button>
            </div>
            {editor}
        </li>
    }
}

/// A filter's settings, sent one at a time as `filter.set_settings` patches, with each
/// refusal under its field, or in the problem bar once the row has closed.
#[component]
fn Editor(
    kind: KindDescriptor,
    owner: FilterOwner,
    filter_id: FilterId,
    settings: Signal<Settings>,
) -> impl IntoView {
    let session = expect_context::<SessionContext>();
    let studio = expect_context::<Studio>();
    let errors = RwSignal::new(HashMap::<String, String>::new());
    let source_id = own_source(&owner);
    let owner = StoredValue::new(owner);
    let filter_id = StoredValue::new(filter_id);
    let commit = move |_: SourceId, param: String, value: Value| {
        let Some(s) = session.get() else {
            return;
        };
        errors.update(|e| _ = e.remove(&param));
        let mut patch = Settings::new();
        patch.insert(param.clone(), value);
        let edit = Edit::FilterSetSettings {
            owner: owner.get_value(),
            filter_id: filter_id.get_value(),
            patch,
        };
        // Once the row has closed, the problem bar shows a refusal.
        spawn_local(async move {
            if let Err(error) = apply(&s, vec![edit], None).await
                && errors
                    .try_update(|e| _ = e.insert(param, error.to_string()))
                    .is_none()
            {
                studio.report(&error);
            }
        });
    };
    view! {
        <div class="filter-editor">
            <Form kind=kind source_id=source_id settings=settings errors=errors commit=commit />
        </div>
    }
}

/// The patch that puts a filter's settings back to `base`, what the dialog opened on:
/// each value `now` changed, and each parameter in `touched`, which the dialog sent and
/// whose edit may not have reached `now` yet. A parameter `base` lacks goes back to
/// `null`, its default.
pub fn restore_patch(now: &Settings, base: &Settings, touched: &HashSet<String>) -> Settings {
    let mut patch = draft_patch(now, base);
    for id in touched {
        patch.insert(id.clone(), base.get(id).cloned().unwrap_or(Value::Null));
    }
    patch
}

/// The Filter settings dialog (spec-workspace.md 6.3), drawn while it is open.
#[component]
pub fn FilterSettings() -> impl IntoView {
    let dialogs = expect_context::<FilterDialogs>();
    let studio = expect_context::<Studio>();
    let close = move || dialogs.edit.set(None);
    move || {
        dialogs.edit.get().map(|target| {
            // A filter removed elsewhere closes the dialog.
            let present = {
                let target = target.clone();
                Memo::new(move |_| filter_of(studio, &target).is_some())
            };
            move || {
                if present.get() {
                    Some(view! { <SettingsBody target=target.clone() close=close /> })
                } else {
                    request_animation_frame(close);
                    None
                }
            }
        })
    }
}

#[component]
fn SettingsBody(target: Target, close: impl Fn() + Copy + Send + Sync + 'static) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let Some((chain, filter)) = untrack(|| filter_of(studio, &target)) else {
        request_animation_frame(close);
        return ().into_any();
    };
    let owner = target.owner.clone();
    // What the dialog opened on: Cancel puts it back.
    let opened = StoredValue::new((filter.settings.clone(), filter.enabled));
    // What the dialog has sent, so Cancel and Defaults cover an edit still on its way
    // back from the engine.
    let touched = StoredValue::new(HashSet::<String>::new());
    let switched = StoredValue::new(false);
    let problem = RwSignal::new(None::<String>);
    let errors = RwSignal::new(HashMap::<String, String>::new());
    let kind_id = filter.kind.clone();
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
        Memo::new(move |_| studio.descriptors.with(|d| d.get(&kind_id).cloned()))
    };
    let current = {
        let target = target.clone();
        Signal::derive(move || filter_of(studio, &target).map(|(_, f)| f))
    };
    let settings = Signal::derive(move || {
        current.with(|f| f.as_ref().map(|f| f.settings.clone()).unwrap_or_default())
    });
    let enabled = Signal::derive(move || current.with(|f| f.as_ref().is_some_and(|f| f.enabled)));
    let name = studio
        .kinds
        .with_untracked(|k| filter_name(k.as_deref().unwrap_or_default(), &kind_id));
    let hint = filter_type(&kind_id, chain, None).hint;
    let owner_name = studio.collection.with_untracked(|c| {
        c.as_ref()
            .map(|c| owner_name(c, &target.owner))
            .unwrap_or_default()
    });
    let (icon, sub) = match chain {
        FilterChain::Video if matches!(target.owner, FilterOwner::Item { .. }) => {
            ("monitor", format!("Effect on “{owner_name}”"))
        }
        FilterChain::Video => ("monitor", format!("Video filter on “{owner_name}”")),
        FilterChain::Audio => ("speaker", format!("Audio filter on “{owner_name}”")),
    };

    let send = move |edits: Vec<Edit>, then: Option<Box<dyn FnOnce()>>| {
        let Some(s) = session.get() else {
            return;
        };
        spawn_local(async move {
            match apply(&s, edits, None).await {
                Ok(_) => {
                    if let Some(then) = then {
                        then();
                    }
                }
                // Once the dialog has closed, the problem bar shows it.
                Err(error) => {
                    if problem.try_set(Some(error.to_string())).is_some() {
                        studio.report(&error);
                    }
                }
            }
        });
    };
    let filter_id = StoredValue::new(target.filter_id.clone());
    let owner = StoredValue::new(owner);
    let switch = move |_| {
        switched.set_value(true);
        send(
            vec![Edit::FilterSetEnabled {
                owner: owner.get_value(),
                filter_id: filter_id.get_value(),
                enabled: !enabled.get_untracked(),
            }],
            None,
        );
    };
    let commit = move |_: SourceId, param: String, value: Value| {
        errors.update(|e| _ = e.remove(&param));
        touched.update_value(|t| _ = t.insert(param.clone()));
        let mut patch = Settings::new();
        patch.insert(param.clone(), value);
        let edit = Edit::FilterSetSettings {
            owner: owner.get_value(),
            filter_id: filter_id.get_value(),
            patch,
        };
        let Some(s) = session.get() else {
            return;
        };
        spawn_local(async move {
            if let Err(error) = apply(&s, vec![edit], None).await
                && errors
                    .try_update(|e| _ = e.insert(param, error.to_string()))
                    .is_none()
            {
                studio.report(&error);
            }
        });
    };
    // Cancel, Escape and the backdrop put back what the dialog opened on.
    let cancel = move || {
        let (base, was_on) = opened.get_value();
        let mut edits = Vec::new();
        let patch =
            touched.with_value(|t| settings.with_untracked(|now| restore_patch(now, &base, t)));
        if !patch.is_empty() {
            edits.push(Edit::FilterSetSettings {
                owner: owner.get_value(),
                filter_id: filter_id.get_value(),
                patch,
            });
        }
        if switched.get_value() || enabled.get_untracked() != was_on {
            edits.push(Edit::FilterSetEnabled {
                owner: owner.get_value(),
                filter_id: filter_id.get_value(),
                enabled: was_on,
            });
        }
        if !edits.is_empty() {
            send(edits, None);
        }
        close();
    };
    // Defaults: every setting back to its default, but the scene a scene-input filter
    // uses.
    let defaults = move |_| {
        let Some(kind) = descriptor.get_untracked() else {
            return;
        };
        errors.set(HashMap::new());
        let mut patch = Settings::new();
        settings.with_untracked(|now| {
            for p in kind.params.iter().filter(|p| p.ty != ParamType::Scene) {
                if *value_of(p, now) != p.default || touched.with_value(|t| t.contains(&p.id)) {
                    touched.update_value(|t| _ = t.insert(p.id.clone()));
                    patch.insert(p.id.clone(), Value::Null);
                }
            }
        });
        if !patch.is_empty() {
            send(
                vec![Edit::FilterSetSettings {
                    owner: owner.get_value(),
                    filter_id: filter_id.get_value(),
                    patch,
                }],
                None,
            );
        }
    };
    let remove = move |_| {
        send(
            vec![Edit::FilterRemove {
                owner: owner.get_value(),
                filter_id: filter_id.get_value(),
            }],
            Some(Box::new(close)),
        );
    };
    let form = {
        let source_id = own_source(&target.owner);
        move || {
            match descriptor.get() {
            None if problem.with(Option::is_some) => ().into_any(),
            None => view! { <p class="placeholder">"Loading…"</p> }.into_any(),
            Some(kind) if kind.params.is_empty() => {
                view! { <p class="muted">"This filter has no settings."</p> }.into_any()
            }
            Some(kind) => view! {
                <Form kind=kind source_id=source_id.clone() settings=settings errors=errors commit=commit />
            }
            .into_any(),
        }
        }
    };
    view! {
        <Modal label="Filter settings" class="filter-dialog" cancel=cancel>
            <div class="modal-header">
                <div class="props-head">
                    <Icon name=icon size=18 />
                    <div class="modal-titles">
                        <h2 class="modal-title">{name}</h2>
                        <span class="modal-sub">{sub}</span>
                    </div>
                </div>
                <div class="filter-dialog-tools">
                    <button
                        type="button"
                        role="switch"
                        class="switch"
                        class:on=move || enabled.get()
                        aria-label="Filter on"
                        aria-checked=move || enabled.get().to_string()
                        on:click=switch
                    >
                        <span class="switch-knob"></span>
                    </button>
                    <button type="button" class="modal-close" aria-label="Close" on:click=move |_| cancel()>
                        "×"
                    </button>
                </div>
            </div>
            <div class="filter-dialog-body">
                {(!hint.is_empty()).then(|| view! { <p class="filter-hint">{hint}</p> })}
                {form}
                {move || problem.get().map(|e| view! { <p class="problem">{e}</p> })}
            </div>
            <div class="modal-footer">
                <button type="button" class="secondary danger props-defaults" on:click=remove>"Remove"</button>
                <button type="button" class="secondary" on:click=defaults>"Defaults"</button>
                <button type="button" class="secondary" on:click=move |_| cancel()>"Cancel"</button>
                <button type="button" class="primary" data-primary on:click=move |_| close()>"OK"</button>
            </div>
        </Modal>
    }
    .into_any()
}

/// The Add filter dialog (spec-workspace.md 6.4), drawn while it is open.
#[component]
pub fn AddFilter() -> impl IntoView {
    let dialogs = expect_context::<FilterDialogs>();
    let studio = expect_context::<Studio>();
    let close = move || dialogs.add.set(None);
    move || {
        dialogs.add.get().and_then(|owner| {
            let present = studio
                .collection
                .with_untracked(|c| c.as_ref().and_then(|c| c.chains(&owner)).is_some());
            match present {
                true => Some(view! { <AddBody owner=owner close=close /> }),
                false => {
                    request_animation_frame(close);
                    None
                }
            }
        })
    }
}

#[component]
fn AddBody(owner: FilterOwner, close: impl Fn() + Copy + Send + Sync + 'static) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let dialogs = expect_context::<FilterDialogs>();
    let (video, audio, name) = studio.collection.with_untracked(|c| {
        c.as_ref().map_or((false, false, String::new()), |c| {
            let (video, audio) = owner_chains(c, &owner);
            (video, audio, owner_name(c, &owner))
        })
    });
    let types = Memo::new(move |_| {
        studio
            .kinds
            .with(|k| catalogue(k.as_deref().unwrap_or_default(), video, audio))
    });
    let search = RwSignal::new(String::new());
    let shown = Memo::new(move |_| {
        let search = search.get();
        types.with(|t| {
            t.iter()
                .filter(|t| type_matches(t, &search))
                .cloned()
                .collect::<Vec<_>>()
        })
    });
    // The type picked: the first one listed until the user picks another.
    let picked = RwSignal::new(None::<String>);
    let current = Memo::new(move |_| {
        let picked = picked.get();
        shown.with(|list| {
            picked
                .and_then(|p| list.iter().find(|t| t.kind == p).cloned())
                .or_else(|| list.first().cloned())
        })
    });
    let scene = RwSignal::new(None::<SceneId>);
    let problem = RwSignal::new(None::<String>);
    // Read the picked kind's descriptor, for its settings and its scene input.
    Effect::new(move |_| {
        let (Some(t), Some(s)) = (current.get(), session.get()) else {
            return;
        };
        spawn_local(async move {
            let _ = studio.descriptor(&s, &t.kind).await;
        });
    });
    let descriptor = Memo::new(move |_| {
        let kind = current.with(|t| t.as_ref().map(|t| t.kind.clone()))?;
        studio.descriptors.with(|d| d.get(&kind).cloned())
    });
    let scene_param = Memo::new(move |_| {
        descriptor.with(|d| {
            d.as_ref()
                .and_then(|d| d.params.iter().find(|p| p.ty == ParamType::Scene))
                .map(|p| p.id.clone())
        })
    });
    let foot = owner_foot(&owner);
    let sub = add_sub(&owner, &name);
    let none = match &owner {
        FilterOwner::Source { .. } => "The engine lists no filters for this source.",
        FilterOwner::Scene { .. } => "The engine lists no filters for this scene.",
        FilterOwner::Item { .. } => "The engine lists no effects for this item.",
        _ => "The engine lists no filters for this output.",
    };
    let owner = StoredValue::new(owner);
    // Whether the owner's `chain` holds all the filters it can.
    let full = move |chain: FilterChain| {
        studio.collection.with(|c| {
            c.as_ref()
                .and_then(|c| owner.with_value(|o| c.chains(o).map(Cow::into_owned)))
                .is_some_and(|f| f.chain(chain).len() >= MAX_CHAIN_FILTERS)
        })
    };
    let blocked = {
        move || match current.get() {
            None => true,
            Some(t) => full(t.chain) || (scene_param.get().is_some() && scene.get().is_none()),
        }
    };
    let add = {
        StoredValue::new(move |t: FilterType| {
            let Some(s) = session.get() else {
                return;
            };
            let filter_id = FilterId::new(studio.fresh_id(&s, "filter"));
            let mut settings = Settings::new();
            if let (Some(param), Some(scene_id)) =
                (scene_param.get_untracked(), scene.get_untracked())
            {
                settings.insert(param, Reference::Scene { scene_id }.to_value());
            }
            // Not described yet: the dialog, which loads the form itself.
            let many =
                descriptor.with_untracked(|d| d.as_ref().is_none_or(|d| opens(d) == Opens::Dialog));
            let edit = Edit::FilterAdd {
                owner: owner.get_value(),
                chain: t.chain,
                filter_id: filter_id.clone(),
                kind: t.kind,
                settings,
                enabled: None,
                index: None,
            };
            spawn_local(async move {
                match apply(&s, vec![edit], None).await {
                    Ok(_) => {
                        close();
                        if many {
                            dialogs.edit(owner.get_value(), filter_id);
                        } else {
                            dialogs.fresh.set(Some(filter_id));
                        }
                    }
                    Err(error) => _ = problem.try_set(Some(error.to_string())),
                }
            });
        })
    };
    let ok = {
        move |_| {
            if blocked() {
                return;
            }
            if let Some(t) = current.get_untracked() {
                add.with_value(|add| add(t));
            }
        }
    };

    let list = move || {
        let list = shown.get();
        if list.is_empty() {
            let text = if types.with(Vec::is_empty) {
                none
            } else {
                "No filters match."
            };
            return view! { <p class="add-filter-none">{text}</p> }.into_any();
        }
        let mut out = Vec::new();
        let mut last: Option<(FilterChain, &'static str)> = None;
        for t in list {
            if last.map(|(c, _)| c) != Some(t.chain) {
                let (title, icon) = match t.chain {
                    FilterChain::Video => ("Video", "monitor"),
                    FilterChain::Audio => ("Audio", "speaker"),
                };
                out.push(
                    view! { <div class="add-filter-chain"><Icon name=icon size=14 />{title}</div> }
                        .into_any(),
                );
            }
            if last != Some((t.chain, t.group)) {
                out.push(view! { <div class="add-filter-group">{t.group}</div> }.into_any());
            }
            last = Some((t.chain, t.group));
            let kind = t.kind.clone();
            let on = {
                let kind = kind.clone();
                move || current.with(|c| c.as_ref().is_some_and(|c| c.kind == kind))
            };
            let pick = {
                let kind = kind.clone();
                move |_| {
                    picked.set(Some(kind.clone()));
                    scene.set(None);
                    problem.set(None);
                }
            };
            // A double-click adds a type that needs no scene at once.
            let quick = {
                let t = t.clone();
                move |_| {
                    if scene_param.get_untracked().is_none() && !full(t.chain) {
                        add.with_value(|add| add(t.clone()));
                    }
                }
            };
            out.push(
                view! {
                    <button
                        type="button"
                        role="option"
                        class="menuitem add-filter-type"
                        class:on=on.clone()
                        aria-selected=move || on().to_string()
                        on:click=pick
                        on:dblclick=quick
                    >
                        {t.name.clone()}
                    </button>
                }
                .into_any(),
            );
        }
        out.collect_view().into_any()
    };

    let editing = studio.selection.with_untracked(|s| s.scene.clone());
    let detail = move || {
        let t = current.get()?;
        let chain_name = match t.chain {
            FilterChain::Video => "Video",
            FilterChain::Audio => "Audio",
        };
        let settings = descriptor.with(|d| {
            d.as_ref().map(|d| {
                if d.params.is_empty() {
                    "None".to_owned()
                } else {
                    d.params
                        .iter()
                        .map(|p| p.label.clone())
                        .collect::<Vec<_>>()
                        .join(" · ")
                }
            })
        });
        let choices = studio.collection.with(|c| {
            c.as_ref()
                .map(|c| owner.with_value(|o| owner_scene_choices(c, o, editing.as_ref())))
                .unwrap_or_default()
        });
        let scene_select = scene_param.get().map(|_| {
            let options = choices
                .into_iter()
                .map(|(id, name)| view! { <option value=id.to_string()>{name}</option> })
                .collect_view();
            view! {
                <label class="field add-filter-scene">
                    <span class="label">"Uses the scene"</span>
                    <select
                        prop:value=move || scene.get().map(|s| s.to_string()).unwrap_or_default()
                        on:change=move |event| {
                            let v = event_target_value(&event);
                            scene.set((!v.is_empty()).then(|| SceneId::new(v)));
                        }
                    >
                        <option value="">"Choose a scene…"</option>
                        {options}
                    </select>
                </label>
            }
        });
        let full_note = full(t.chain).then(|| {
            view! {
                <p class="add-note">
                    {format!("This chain is full: {MAX_CHAIN_FILTERS} filters at most.")}
                </p>
            }
        });
        Some(view! {
            <div class="add-filter-title">{t.name.clone()}</div>
            <div class="add-filter-kind">{format!("{chain_name} · {}", t.group)}</div>
            {(!t.hint.is_empty()).then(|| view! { <p class="add-filter-hint">{t.hint}</p> })}
            {scene_select}
            {settings.map(|s| view! {
                <div class="add-filter-settings">
                    <span class="add-existing-title">"Settings"</span>
                    <span>{s}</span>
                </div>
            })}
            {full_note}
        })
    };

    view! {
        <Modal label="Add filter" class="add-filter" cancel=close>
            <div class="modal-header">
                <div class="modal-titles">
                    <h2 class="modal-title">"Add filter"</h2>
                    <span class="modal-sub">{sub}</span>
                </div>
                <button type="button" class="modal-close" aria-label="Close" on:click=move |_| close()>
                    "×"
                </button>
            </div>
            <div class="add-body">
                <div class="add-filter-left">
                    <label class="tree-search add-filter-search">
                        <Icon name="search" size=14 />
                        <input type="text" aria-label="Search filters" placeholder="Search filters" bind:value=search />
                    </label>
                    <div class="add-types" role="listbox" aria-label="Filter type">{list}</div>
                </div>
                <div class="add-detail">
                    {detail}
                    {move || problem.get().map(|e| view! { <p class="problem">{e}</p> })}
                    <p class="add-note add-filter-foot">{foot}</p>
                </div>
            </div>
            <div class="modal-footer">
                <button type="button" class="secondary" on:click=move |_| close()>"Cancel"</button>
                <button type="button" class="primary" data-primary disabled=blocked on:click=ok>
                    "Add filter"
                </button>
            </div>
        </Modal>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{ParamDescriptor, SceneInfo};
    use serde_json::json;

    fn kind_summary(id: &str, name: &str, category: KindCategory) -> KindSummary {
        KindSummary {
            kind_id: id.into(),
            category,
            name: name.into(),
            version: 1,
        }
    }

    fn param(id: &str, label: &str, ty: ParamType, default: Value) -> ParamDescriptor {
        ParamDescriptor {
            id: id.into(),
            ty,
            default,
            label: label.into(),
            description: None,
            group: None,
            advanced: false,
            visible_if: None,
            live: true,
        }
    }

    fn float(min: f64, max: f64, step: Option<f64>, unit: Option<&str>) -> ParamType {
        ParamType::Float {
            min: Some(min),
            max: Some(max),
            step,
            unit: unit.map(str::to_owned),
        }
    }

    fn descriptor(params: Vec<ParamDescriptor>) -> KindDescriptor {
        KindDescriptor {
            kind_id: "f".into(),
            category: KindCategory::VideoFilter,
            name: "F".into(),
            version: 1,
            video: true,
            audio: false,
            max_outset_px: None,
            params,
        }
    }

    #[test]
    fn the_catalogue_follows_the_prototypes_groups_video_first() {
        use KindCategory::{AudioFilter, Source, VideoFilter};
        let kinds = vec![
            kind_summary("gain", "Gain", AudioFilter),
            kind_summary("delay", "Delay", AudioFilter),
            kind_summary("scroll", "Scroll", VideoFilter),
            kind_summary("sharpen", "Sharpen", VideoFilter),
            kind_summary("color_correction", "Color correction", VideoFilter),
            kind_summary("crt/scanlines", "Scanlines", VideoFilter),
            kind_summary("odd", "Odd one", VideoFilter),
            kind_summary("color", "Color", Source),
        ];
        let all = catalogue(&kinds, true, true);
        let shape: Vec<(&str, &str)> = all.iter().map(|t| (t.group, t.name.as_str())).collect();
        assert_eq!(
            shape,
            vec![
                ("Color and keying", "Color correction"),
                ("Detail", "Sharpen"),
                ("Motion and timing", "Scroll"),
                ("Effect packs", "Scanlines"),
                ("Other", "Odd one"),
                ("Levels and dynamics", "Gain"),
                ("Timing", "Audio delay"),
            ]
        );
        // Only the chains the source has.
        assert!(
            catalogue(&kinds, false, true)
                .iter()
                .all(|t| t.chain == FilterChain::Audio)
        );
        assert!(
            catalogue(&kinds, true, false)
                .iter()
                .all(|t| t.chain == FilterChain::Video)
        );
        // The prototype's name, else the engine's, else the kind ID.
        assert_eq!(filter_name(&kinds, "delay"), "Audio delay");
        assert_eq!(filter_name(&kinds, "odd"), "Odd one");
        assert_eq!(filter_name(&[], "sharpen"), "Sharpen");
        assert_eq!(filter_name(&[], "mystery"), "mystery");
        assert_eq!(
            filter_name(&kinds, "color"),
            "color",
            "a source kind isn't a filter"
        );
    }

    /// A CLAP plugin's kind is offered under Plugins, by the plugin's name, after
    /// the built-in audio kinds, for any owner with an audio chain (FL-08).
    #[test]
    fn clap_plugins_are_offered_under_plugins() {
        use KindCategory::AudioFilter;
        let kinds = vec![
            kind_summary("clap/com.tdr.nova", "TDR Nova", AudioFilter),
            kind_summary("gain", "Gain", AudioFilter),
            kind_summary(
                "clap/org.airwindows.consolidated",
                "Airwindows",
                AudioFilter,
            ),
        ];
        let all = catalogue(&kinds, false, true);
        let shape: Vec<(&str, &str, &str)> = all
            .iter()
            .map(|t| (t.group, t.name.as_str(), t.hint))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("Levels and dynamics", "Gain", "Turn it up or down"),
                ("Plugins", "Airwindows", "A CLAP audio effect"),
                ("Plugins", "TDR Nova", "A CLAP audio effect"),
            ]
        );
        assert!(type_matches(&all[2], "clap"), "found by its hint");
        assert!(type_matches(&all[2], "plugins"), "and by its group");
        assert_eq!(filter_name(&kinds, "clap/com.tdr.nova"), "TDR Nova");
    }

    #[test]
    fn the_search_matches_name_hint_and_group() {
        let t = filter_type("chroma_key", FilterChain::Video, None);
        assert!(type_matches(&t, ""));
        assert!(type_matches(&t, "CHROMA"));
        assert!(type_matches(&t, "green"));
        assert!(type_matches(&t, "keying"));
        assert!(!type_matches(&t, "blur"));
    }

    #[test]
    fn few_settings_open_in_place_and_more_in_the_dialog() {
        let p = |id: &str| param(id, id, ParamType::Bool, json!(false));
        assert_eq!(opens(&descriptor(vec![])), Opens::Nothing);
        assert_eq!(opens(&descriptor(vec![p("a")])), Opens::InPlace);
        assert_eq!(
            opens(&descriptor(vec![p("a"), p("b"), p("c")])),
            Opens::InPlace
        );
        assert_eq!(
            opens(&descriptor(vec![p("a"), p("b"), p("c"), p("d")])),
            Opens::Dialog
        );
    }

    #[test]
    fn numbers_show_their_steps_decimals_a_true_minus_and_their_unit() {
        assert_eq!(number_text(&json!(0.2), Some(0.05), false, None), "0.20");
        assert_eq!(
            number_text(&json!(-6), Some(0.5), false, Some("dB")),
            "−6.0 dB"
        );
        assert_eq!(number_text(&json!(40), Some(1.0), false, Some("%")), "40%");
        assert_eq!(number_text(&json!(-0.0), Some(0.5), false, None), "0.0");
        assert_eq!(
            number_text(&json!(1.25), None, false, Some("ms")),
            "1.25 ms"
        );
        assert_eq!(number_text(&json!(1.5), None, false, None), "1.5");
        assert_eq!(number_text(&json!(24), None, true, Some("px")), "24 px");
        assert_eq!(number_text(&json!(-180), None, true, Some("°")), "−180°");
    }

    #[test]
    fn a_summary_shows_one_value_changed_values_or_defaults() {
        let gain = descriptor(vec![param(
            "gain_db",
            "Gain",
            float(-96.0, 24.0, Some(0.5), Some("dB")),
            json!(0.0),
        )]);
        let mut s = Settings::new();
        assert_eq!(summary(&gain, &s, |_| None), Summary::Text("0.0 dB".into()));
        s.insert("gain_db", json!(-6.0));
        assert_eq!(
            summary(&gain, &s, |_| None),
            Summary::Text("−6.0 dB".into())
        );

        let shadow = descriptor(vec![
            param(
                "x",
                "Offset X",
                float(-200.0, 200.0, Some(1.0), Some("px")),
                json!(6.0),
            ),
            param(
                "y",
                "Offset Y",
                float(-200.0, 200.0, Some(1.0), Some("px")),
                json!(8.0),
            ),
            param(
                "blur",
                "Blur",
                float(0.0, 200.0, Some(1.0), Some("px")),
                json!(16.0),
            ),
            param(
                "color",
                "Color",
                ParamType::Color { alpha: false },
                json!("#0f1115"),
            ),
        ]);
        let mut s = Settings::new();
        assert_eq!(
            summary(&shadow, &s, |_| None),
            Summary::Text("defaults".into())
        );
        s.insert("color", json!("#ffffff"));
        assert_eq!(
            summary(&shadow, &s, |_| None),
            Summary::Text("defaults".into()),
            "only numbers are summarised"
        );
        s.insert("y", json!(-4.0));
        s.insert("blur", json!(30.0));
        s.insert("x", json!(6.0));
        assert_eq!(
            summary(&shadow, &s, |_| None),
            Summary::Text("Offset Y −4 px · Blur 30 px".into())
        );
        assert_eq!(summary(&descriptor(vec![]), &s, |_| None), Summary::Empty);
    }

    #[test]
    fn a_scene_input_filter_summarises_its_scene() {
        let mask = descriptor(vec![
            param("scene", "Scene", ParamType::Scene, Value::Null),
            param("invert", "Invert", ParamType::Bool, json!(false)),
        ]);
        let mut s = Settings::new();
        assert_eq!(summary(&mask, &s, |_| None), Summary::NoScene);
        s.insert(
            "scene",
            Reference::Scene {
                scene_id: SceneId::new("cam"),
            }
            .to_value(),
        );
        let name = |id: &SceneId| (id.as_str() == "cam").then(|| "Cam shape".to_owned());
        assert_eq!(summary(&mask, &s, name), Summary::Scene("Cam shape".into()));
        assert_eq!(summary(&mask, &s, |_| None), Summary::Scene("cam".into()));
    }

    #[test]
    fn a_source_has_the_chains_its_kind_makes() {
        let source = |kind: SourceKind, audio: bool| SourceInfo {
            source_id: SourceId::new("s"),
            name: "S".into(),
            kind,
            settings: Settings::new(),
            duplicated_from: None,
            audio: audio.then_some(jstream_control::protocol::SourceAudio {
                enabled: true,
                follow: jstream_control::protocol::AudioFollow::Always,
                gain_db: jstream_control::protocol::Finite32::ZERO,
                muted: false,
            }),
            filters: Default::default(),
            meta: Default::default(),
        };
        let builtin = |k: &str| SourceKind::Builtin { kind: k.into() };
        assert_eq!(chains_of(&source(builtin("color"), false)), (true, false));
        assert_eq!(chains_of(&source(builtin("media"), true)), (true, true));
        assert_eq!(
            chains_of(&source(builtin("audio_input"), true)),
            (false, true)
        );
        let nested = SourceKind::Scene {
            scene_id: SceneId::new("x"),
        };
        assert_eq!(chains_of(&source(nested, true)), (true, false));
    }

    #[test]
    fn scene_inputs_offer_scenes_that_dont_show_the_source() {
        use jstream_control::protocol::{
            Crop, Finite32, ItemInfo, ItemTransform, PlacementForm, SceneRole,
        };
        let item = |source: &str| ItemInfo {
            item_id: format!("i-{source}").as_str().into(),
            source_id: source.into(),
            placement: PlacementForm::Legacy,
            transform: ItemTransform::IDENTITY,
            geometry: None,
            crop: Crop::NONE,
            visible: true,
            effects: Vec::new(),
            opacity: Finite32::ONE,
            blend: Default::default(),
            transition: jstream_control::protocol::ItemTransition::default(),
        };
        let scene = |id: &str, items: Vec<ItemInfo>| SceneInfo {
            overrides: Vec::new(),
            filters: Default::default(),
            variant_of: None,
            scene_id: id.into(),
            name: id.to_uppercase(),
            role: SceneRole::Scene,
            folder: None,
            meta: Default::default(),
            items,
            transition_in: None,
            transition_out: None,
        };
        let collection = Collection {
            scenes: vec![
                scene("main", vec![item("cam")]),
                scene("shape", vec![]),
                scene("other", vec![item("cam")]),
                scene("logo", vec![item("logo")]),
            ],
            ..Collection::default()
        };
        let choices: Vec<String> = scene_choices(
            &collection,
            Some(&SceneId::new("logo")),
            &SourceId::new("cam"),
        )
        .into_iter()
        .map(|(_, name)| name)
        .collect();
        assert_eq!(choices, ["SHAPE"]);
    }

    /// A scene's chain (FL-06): a keyframe shows its parent's, and a scene input on it
    /// leaves out the scene and every scene whose picture holds it.
    #[test]
    fn a_scene_shows_its_own_chain_and_its_inputs_leave_out_what_holds_it() {
        use jstream_control::protocol::{
            Crop, Finite32, ItemInfo, ItemTransform, PlacementForm, SceneRole,
        };
        let item = |source: &str| ItemInfo {
            item_id: format!("i-{source}").as_str().into(),
            source_id: source.into(),
            placement: PlacementForm::Legacy,
            transform: ItemTransform::IDENTITY,
            geometry: None,
            crop: Crop::NONE,
            visible: true,
            effects: Vec::new(),
            opacity: Finite32::ONE,
            blend: Default::default(),
            transition: jstream_control::protocol::ItemTransition::default(),
        };
        let scene = |id: &str, items: Vec<ItemInfo>, variant_of: Option<&str>| SceneInfo {
            overrides: Vec::new(),
            filters: Default::default(),
            variant_of: variant_of.map(SceneId::new),
            scene_id: id.into(),
            name: id.to_uppercase(),
            role: SceneRole::Scene,
            folder: None,
            meta: Default::default(),
            items,
            transition_in: None,
            transition_out: None,
        };
        let nested = |id: &str, scene_id: &str| SourceInfo {
            source_id: SourceId::new(id),
            name: id.into(),
            kind: SourceKind::Scene {
                scene_id: SceneId::new(scene_id),
            },
            settings: Settings::new(),
            duplicated_from: None,
            audio: None,
            filters: Default::default(),
            meta: Default::default(),
        };
        // `outer` nests `middle`, which nests `cam`; `cam-close` is a keyframe of `cam`.
        let collection = Collection {
            scenes: vec![
                scene("cam", vec![item("webcam")], None),
                scene("cam-close", vec![], Some("cam")),
                scene("middle", vec![item("n-cam")], None),
                scene("outer", vec![item("n-middle")], None),
                scene("shape", vec![], None),
            ],
            sources: vec![nested("n-cam", "cam"), nested("n-middle", "middle")],
            ..Collection::default()
        };
        let cam = scene_owner(&collection, &SceneId::new("cam"));
        assert_eq!(
            scene_owner(&collection, &SceneId::new("cam-close")),
            cam,
            "a keyframe shows its parent's chain"
        );
        assert_eq!(owner_chains(&collection, &cam), (true, false));
        assert_eq!(owner_name(&collection, &cam), "CAM");
        assert_eq!(
            owner_note(&collection, &cam),
            "Scene filters apply to the whole scene, top to bottom."
        );
        assert!(collection.chains(&cam).is_some());

        let names = |owner: &FilterOwner| -> Vec<String> {
            owner_scene_choices(&collection, owner, None)
                .into_iter()
                .map(|(_, name)| name)
                .collect()
        };
        assert_eq!(names(&cam), ["SHAPE"]);
        let middle = scene_owner(&collection, &SceneId::new("middle"));
        assert_eq!(names(&middle), ["CAM", "CAM-CLOSE", "SHAPE"]);
        // A source's choices are still [`scene_choices`]'.
        let webcam = FilterOwner::Source {
            source_id: SourceId::new("webcam"),
        };
        assert_eq!(
            owner_scene_choices(&collection, &webcam, None),
            scene_choices(&collection, None, &SourceId::new("webcam"))
        );
    }

    /// An item's effects (FL-09, ADR-0065): its video chain, named by its source, with
    /// scene inputs that leave out every scene whose picture holds the item's scene.
    #[test]
    fn an_item_shows_its_effects_and_its_inputs_leave_out_what_holds_its_scene() {
        use jstream_control::protocol::{
            Crop, Finite32, ItemInfo, ItemTransform, PlacementForm, SceneRole,
        };
        let item = |source: &str, effects: Vec<FilterInfo>| ItemInfo {
            item_id: format!("i-{source}").as_str().into(),
            source_id: source.into(),
            placement: PlacementForm::Legacy,
            transform: ItemTransform::IDENTITY,
            geometry: None,
            crop: Crop::NONE,
            visible: true,
            effects,
            opacity: Finite32::ONE,
            blend: Default::default(),
            transition: jstream_control::protocol::ItemTransition::default(),
        };
        let scene = |id: &str, items: Vec<ItemInfo>| SceneInfo {
            overrides: Vec::new(),
            filters: Default::default(),
            variant_of: None,
            scene_id: id.into(),
            name: id.to_uppercase(),
            role: SceneRole::Scene,
            folder: None,
            meta: Default::default(),
            items,
            transition_in: None,
            transition_out: None,
        };
        let source = |id: &str, kind: SourceKind| SourceInfo {
            source_id: SourceId::new(id),
            name: format!("{id} source"),
            kind,
            settings: Settings::new(),
            duplicated_from: None,
            audio: None,
            filters: Default::default(),
            meta: Default::default(),
        };
        let vignette = FilterInfo {
            filter_id: FilterId::new("v"),
            kind: "vignette".into(),
            enabled: true,
            settings: Settings::new(),
        };
        // `outer` nests `middle`, which places the webcam with a vignette on it.
        let collection = Collection {
            scenes: vec![
                scene(
                    "middle",
                    vec![item("webcam", vec![vignette.clone()]), item("mic", vec![])],
                ),
                scene("outer", vec![item("n-middle", vec![])]),
                scene("shape", vec![]),
            ],
            sources: vec![
                source(
                    "webcam",
                    SourceKind::Builtin {
                        kind: "color".into(),
                    },
                ),
                source(
                    "mic",
                    SourceKind::Builtin {
                        kind: "audio_input".into(),
                    },
                ),
                source(
                    "n-middle",
                    SourceKind::Scene {
                        scene_id: SceneId::new("middle"),
                    },
                ),
            ],
            ..Collection::default()
        };
        let owner = |id: &str| FilterOwner::Item {
            item_id: ItemId::new(id),
        };
        let cam = owner("i-webcam");
        let chains = collection.chains(&cam).expect("the item's effects");
        assert_eq!(chains.video, [vignette]);
        assert!(chains.audio.is_empty());
        assert_eq!(owner_chains(&collection, &cam), (true, false));
        assert_eq!(owner_name(&collection, &cam), "webcam source");
        assert_eq!(
            owner_note(&collection, &cam),
            "Only this item, after its crop, top to bottom."
        );
        assert_eq!(
            owner_foot(&cam),
            "Applies to this item only, after its crop."
        );
        assert_eq!(
            add_sub(&cam, "webcam source"),
            "For this item of “webcam source”"
        );
        let names: Vec<String> = owner_scene_choices(&collection, &cam, None)
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        assert_eq!(names, ["SHAPE"], "middle is its scene, outer nests it");
        // An audio-only item has no picture, so no effects to show.
        assert_eq!(owner_chains(&collection, &owner("i-mic")), (false, false));
        // An item that's gone has none.
        assert!(collection.chains(&owner("gone")).is_none());
        assert_eq!(owner_chains(&collection, &owner("gone")), (false, false));
        assert!(owner_scene_choices(&collection, &owner("gone"), None).is_empty());
        // Its chain reads as effects.
        assert_eq!(
            chain_head(true, FilterChain::Video),
            ("Effects", "monitor", "No effects.")
        );
        assert_eq!(chain_head(false, FilterChain::Video).0, "Video");
        assert_eq!(count_text(true, 1), "1 effect");
        assert_eq!(count_text(true, 3), "3 effects");
        assert_eq!(count_text(false, 0), "0 filters");
    }

    /// The vignette is a Style filter with the prototype's kind of hint (#862).
    #[test]
    fn the_vignette_is_offered_under_style() {
        let kinds = [kind_summary(
            "vignette",
            "Vignette",
            KindCategory::VideoFilter,
        )];
        let all = catalogue(&kinds, true, false);
        assert_eq!(
            all,
            [FilterType {
                kind: "vignette".into(),
                chain: FilterChain::Video,
                group: "Style",
                name: "Vignette".into(),
                hint: "Darken or tint the edges",
            }]
        );
    }

    #[test]
    fn the_program_output_and_outputs_show_their_audio_chain_only() {
        use jstream_control::protocol::{OutputId, OutputInfo, RecordContainer};
        let output = |id: &str, kind: OutputKind| OutputInfo {
            output_id: id.into(),
            kind,
            encoder_profile: None,
            filters: Default::default(),
            video_mix: None,
            audio_buses: vec![jstream_protocol::BusId::master()],
        };
        let collection = Collection {
            outputs: vec![
                output(
                    "rec",
                    OutputKind::Record {
                        path: "/tmp/a.mp4".into(),
                        container: RecordContainer::Mp4,
                    },
                ),
                output("cam", OutputKind::VirtualCam),
            ],
            ..Collection::default()
        };
        let out = |id: &str| FilterOwner::Output {
            output_id: OutputId::new(id),
        };
        let master = FilterOwner::Master;
        assert_eq!(owner_chains(&collection, &master), (false, true));
        assert_eq!(owner_name(&collection, &master), "Program output");
        assert_eq!(
            owner_note(&collection, &master),
            "Master chain: applies to everything leaving JStream."
        );
        assert_eq!(owner_chains(&collection, &out("rec")), (false, true));
        assert_eq!(owner_name(&collection, &out("rec")), "rec");
        assert_eq!(
            owner_note(&collection, &out("rec")),
            "Only this output, after the program filters."
        );
        // The virtual camera has no sound, so no chain the engine takes.
        assert_eq!(owner_chains(&collection, &out("cam")), (false, false));
        assert_eq!(
            owner_note(&collection, &out("cam")),
            "Only this output, after the program filters. No sound."
        );
        assert_eq!(owner_chains(&collection, &out("gone")), (false, false));
        assert!(collection.chains(&out("gone")).is_none());
        assert!(collection.chains(&master).is_some());
        assert_eq!(own_source(&master), SourceId::new(""));
    }

    #[test]
    fn restore_covers_edits_still_in_flight() {
        let settings = |pairs: &[(&str, Value)]| -> Settings {
            pairs.iter().map(|(id, v)| (*id, v.clone())).collect()
        };
        let base = settings(&[("amount", json!(0.2)), ("scene", json!("s"))]);
        // The store already shows `amount`; `radius` was sent and hasn't come back.
        let now = settings(&[("amount", json!(0.5)), ("scene", json!("s"))]);
        let touched: HashSet<String> = ["amount".into(), "radius".into()].into();
        let patch = restore_patch(&now, &base, &touched);
        assert_eq!(
            patch,
            settings(&[("amount", json!(0.2)), ("radius", Value::Null)])
        );
        // Nothing sent and nothing changed: nothing to put back.
        assert!(restore_patch(&base, &base, &HashSet::new()).is_empty());
    }
}
