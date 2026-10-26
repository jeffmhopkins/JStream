//! The Inspector: what is selected, and how it is placed. For a selected item, a header
//! (name, kind, how many more are selected, favourite) over foldable sections: its box
//! (X, Y, W, H, the size as a % of its source's and rotation, with a W/H link), its
//! crop, its blending (mode and
//! opacity) and its source's settings ([`Properties`]). With no item selected, the
//! selected scene's header over its object count, the scenes that use it as a source,
//! and its transition overrides. Every field sends its edit when it changes, so the
//! canvas and the Inspector show the same committed numbers.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#inspector

use super::Properties;
use super::number::NumberField;
use super::studio;
use crate::app::SessionContext;
use crate::hotkeys;
use crate::icons::{Icon, named};
use crate::panels::labels::{LabelChips, LabelPopover, TagButton, TagPopover, Tagged, carried};
use crate::panels::sources::icon_of;
use crate::store::{Collection, Studio};
use jstream_control::protocol::{
    BlendMode, Chord, Crop, Edit, Finite32, Glide, ItemEasing, ItemGeometry, ItemId, ItemInfo,
    ItemTransition, LabelInfo, MAX_DELAY_MS, PlacementForm, SceneId, SourceId, SourceKind,
    TransitionEffect, TransitionPresetId, TransitionsInfo,
};
use leptos::prelude::*;
use std::collections::HashMap;

/// One of the box's number fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Param {
    /// The left edge, in canvas pixels.
    X,
    /// The top edge.
    Y,
    /// The width.
    W,
    /// The height.
    H,
    /// The rotation, in degrees clockwise.
    Rotation,
}

/// The farthest a box's edge may be placed from the canvas origin, in pixels.
pub const MAX_POSITION: f32 = 10_000.0;
/// The largest width or height the fields accept, in pixels.
pub const MAX_SIZE: f32 = 10_000.0;
/// The smallest width or height the fields accept, in pixels.
pub const MIN_SIZE: f32 = 1.0;
/// The largest crop of one side the fields accept, in source pixels.
pub const MAX_CROP: f32 = 4_000.0;
/// The smallest size, in % of the source's, the W % and H % fields accept.
pub const MIN_PERCENT: f32 = 0.1;
/// The largest size, in % of the source's, the W % and H % fields accept.
pub const MAX_PERCENT: f32 = 10_000.0;

/// The box's width (`across`) or height as a % of its source's own size.
pub fn percent(geometry: &ItemGeometry, source: (u32, u32), across: bool) -> Option<f32> {
    let (side, own) = if across {
        (geometry.size.width.get(), source.0)
    } else {
        (geometry.size.height.get(), source.1)
    };
    (own > 0).then(|| side / own as f32 * 100.0)
}

/// The box with its width (`across`) or height set to `value` % of its source's size
/// (kept to 0.1–10000 %), as the W or H field would set it, `linked` or not.
pub fn with_percent(
    geometry: &ItemGeometry,
    source: (u32, u32),
    across: bool,
    value: f32,
    linked: bool,
) -> Option<ItemGeometry> {
    let value = value.clamp(MIN_PERCENT, MAX_PERCENT) / 100.0;
    if across {
        with_param(geometry, Param::W, value * source.0 as f32, linked)
    } else {
        with_param(geometry, Param::H, value * source.1 as f32, linked)
    }
}

/// The box's top-left corner, as the X and Y fields show it: the corner of the
/// unrotated box, as the canvas's size readout uses.
pub fn top_left(geometry: &ItemGeometry) -> (f32, f32) {
    (
        geometry.center.x.get() - geometry.size.width.get() / 2.0,
        geometry.center.y.get() - geometry.size.height.get() / 2.0,
    )
}

/// What a field shows for `geometry`.
pub fn param_value(geometry: &ItemGeometry, param: Param) -> f32 {
    let (x, y) = top_left(geometry);
    match param {
        Param::X => x,
        Param::Y => y,
        Param::W => geometry.size.width.get(),
        Param::H => geometry.size.height.get(),
        Param::Rotation => geometry.rotation_deg.get(),
    }
}

/// The field's value kept to its range.
pub fn clamp(param: Param, value: f32) -> f32 {
    match param {
        Param::X | Param::Y => value.clamp(-MAX_POSITION, MAX_POSITION),
        Param::W | Param::H => value.clamp(MIN_SIZE, MAX_SIZE),
        Param::Rotation => value.clamp(-360.0, 360.0),
    }
}

/// Rounds to two decimals, as the linked side is kept.
fn round2(value: f32) -> f32 {
    (value * 100.0).round() / 100.0
}

/// The box with one field set to `value` (kept to its range). The top-left corner stays
/// put when the size changes, and with `linked` a new width or height changes the
/// other side to keep the aspect ratio. `None` when the result isn't a valid box.
pub fn with_param(
    geometry: &ItemGeometry,
    param: Param,
    value: f32,
    linked: bool,
) -> Option<ItemGeometry> {
    let value = clamp(param, value);
    let (x, y) = top_left(geometry);
    let (w0, h0) = (geometry.size.width.get(), geometry.size.height.get());
    let rotation = geometry.rotation_deg.get();
    let (x, y, w, h, rotation) = match param {
        Param::X => (value, y, w0, h0, rotation),
        Param::Y => (x, value, w0, h0, rotation),
        Param::W if linked => (x, y, value, linked_side(value * h0 / w0), rotation),
        Param::H if linked => (x, y, linked_side(value * w0 / h0), value, rotation),
        Param::W => (x, y, value, h0, rotation),
        Param::H => (x, y, w0, value, rotation),
        Param::Rotation => (x, y, w0, h0, value),
    };
    crate::canvas::transform::with_box(geometry, x + w / 2.0, y + h / 2.0, w, h, rotation)
}

/// The other side of a linked resize, kept within the sizes a field may send.
fn linked_side(value: f32) -> f32 {
    round2(value).clamp(MIN_SIZE, MAX_SIZE)
}

/// One side of a crop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// Cut from the left.
    Left,
    /// Cut from the right.
    Right,
    /// Cut from the top.
    Top,
    /// Cut from the bottom.
    Bottom,
}

/// How much `crop` cuts from `side`.
pub fn crop_side(crop: &Crop, side: Side) -> u32 {
    match side {
        Side::Left => crop.left,
        Side::Right => crop.right,
        Side::Top => crop.top,
        Side::Bottom => crop.bottom,
    }
}

/// The crop with `side` set to `value`, rounded and kept to 0..=4000.
pub fn with_side(crop: &Crop, side: Side, value: f32) -> Crop {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "kept to 0..=4000 and rounded first, so the cast is exact"
    )]
    let value = value.round().clamp(0.0, MAX_CROP) as u32;
    let mut crop = *crop;
    match side {
        Side::Left => crop.left = value,
        Side::Right => crop.right = value,
        Side::Top => crop.top = value,
        Side::Bottom => crop.bottom = value,
    }
    crop
}

/// One of the 3D section's fields (spec-panels.md I9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth3 {
    /// Tilt about the horizontal axis, in degrees.
    Tilt,
    /// Turn about the vertical axis, in degrees.
    Turn,
    /// Offset towards the viewer, in pixels.
    Depth,
    /// The viewer's distance from the canvas, in pixels.
    Perspective,
}

/// The perspective the prototype draws 3D with when an item sets none, in pixels; the
/// first 3D change sets it, since without one the engine draws 3D orthographically, where
/// Depth has no effect (ADR-0050).
pub const DEFAULT_PERSPECTIVE: f32 = 1200.0;

impl Depth3 {
    /// The field's range.
    pub fn range(self) -> (f32, f32) {
        match self {
            Depth3::Tilt | Depth3::Turn => (-180.0, 180.0),
            Depth3::Depth => (-5000.0, 5000.0),
            Depth3::Perspective => (100.0, 20_000.0),
        }
    }
}

/// What a 3D field shows for `geometry`; Perspective shows [`DEFAULT_PERSPECTIVE`] while
/// the item has none.
pub fn depth_value(geometry: &ItemGeometry, field: Depth3) -> f32 {
    match field {
        Depth3::Tilt => geometry.tilt_deg.get(),
        Depth3::Turn => geometry.turn_deg.get(),
        Depth3::Depth => geometry.depth_px.get(),
        Depth3::Perspective => geometry
            .perspective_px
            .map_or(DEFAULT_PERSPECTIVE, Finite32::get),
    }
}

/// Whether the item is drawn in 3D: the folded section's "in use" hint.
pub fn in_3d(geometry: &ItemGeometry) -> bool {
    geometry.tilt_deg.get() != 0.0
        || geometry.turn_deg.get() != 0.0
        || geometry.depth_px.get() != 0.0
}

/// The box with one 3D field set to `value`, kept to its range. A change that puts the
/// item in 3D while it has no perspective gives it [`DEFAULT_PERSPECTIVE`].
pub fn with_depth(geometry: &ItemGeometry, field: Depth3, value: f32) -> Option<ItemGeometry> {
    let (min, max) = field.range();
    let value = Finite32::new(value.clamp(min, max))?;
    let mut next = *geometry;
    match field {
        Depth3::Tilt => next.tilt_deg = value,
        Depth3::Turn => next.turn_deg = value,
        Depth3::Depth => next.depth_px = value,
        Depth3::Perspective => next.perspective_px = Some(value),
    }
    if next.perspective_px.is_none() && in_3d(&next) {
        next.perspective_px = Finite32::new(DEFAULT_PERSPECTIVE);
    }
    Some(next)
}

/// A blend mode's name, as the Mode select lists it.
pub fn blend_name(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Additive => "Additive",
        BlendMode::Multiply => "Multiply",
        BlendMode::Screen => "Screen",
        BlendMode::Overlay => "Overlay",
        BlendMode::Darken => "Darken",
        BlendMode::Lighten => "Lighten",
        BlendMode::Difference => "Difference",
        BlendMode::Exclusion => "Exclusion",
        _ => "Normal",
    }
}

/// A field's value as the box's `f32`. The fields keep values within ±100000, well
/// inside `f32`'s range.
fn to_f32(value: f64) -> f32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "field values are clamped to ±100000, well inside f32"
    )]
    let value = value as f32;
    value
}

/// The scenes that show `scene_id` as a source, by name, in display order: a keyframe
/// scene shows its parent's items, so it is left out.
pub fn used_in(collection: &Collection, scene_id: &SceneId) -> Vec<String> {
    let shows_it = |item: &ItemInfo| {
        matches!(
            collection.source(&item.source_id).map(|s| &s.kind),
            Some(SourceKind::Scene { scene_id: shown }) if shown == scene_id
        )
    };
    collection
        .scenes
        .iter()
        .filter(|scene| scene.variant_of.is_none())
        .filter(|scene| scene.items.iter().any(shows_it))
        .map(|scene| scene.name.clone())
        .collect()
}

/// Whether the item's source is placed in more than one scene, so the item is one of
/// several references to it and its name reads in italics. A keyframe scene shows its
/// parent's items and places none of its own, so it doesn't count.
pub fn is_shared(collection: &Collection, item: &ItemInfo) -> bool {
    collection
        .scenes
        .iter()
        .filter(|scene| scene.variant_of.is_none())
        .filter(|scene| scene.items.iter().any(|i| i.source_id == item.source_id))
        .count()
        > 1
}

/// The header's kind tag for an item's source: its kind's name once the descriptor is
/// read, else its kind's ID; "Scene" for a nested scene.
pub fn kind_tag(studio: Studio, kind: &SourceKind) -> String {
    match kind {
        SourceKind::Builtin { kind } => studio
            .descriptors
            .with(|d| d.get(kind).map(|k| k.name.clone()))
            .unwrap_or_else(|| kind.clone()),
        SourceKind::Scene { .. } => "Scene".into(),
        _ => "Unknown".into(),
    }
}

/// The Transition in and out options: "Default (<the default's name>)" for no override,
/// then every preset, as `(value, label)` with the preset's ID as the value.
pub fn transition_options(transitions: &TransitionsInfo) -> Vec<(String, String)> {
    let default = transitions
        .presets
        .iter()
        .find(|p| p.preset_id == transitions.default)
        .map_or_else(|| transitions.default.to_string(), |p| p.name.clone());
    std::iter::once((String::new(), format!("Default ({default})")))
        .chain(
            transitions
                .presets
                .iter()
                .map(|p| (p.preset_id.to_string(), p.name.clone())),
        )
        .collect()
}

/// The longest delay the Scene switches field offers, in milliseconds (the engine takes
/// up to [`MAX_DELAY_MS`]).
pub const SWITCH_DELAY_MAX: u32 = 3_000;

/// The item easings, as `(value, label)`, in the order the select lists them.
pub const ITEM_EASINGS: [(ItemEasing, &str, &str); 5] = [
    (ItemEasing::Inherit, "inherit", "Inherit"),
    (ItemEasing::EaseInOut, "ease_in_out", "Ease in-out"),
    (ItemEasing::EaseOut, "ease_out", "Ease out"),
    (ItemEasing::Linear, "linear", "Linear"),
    (ItemEasing::Spring, "spring", "Spring"),
];

/// Whether an item may glide, as `(value, label)`.
pub const GLIDES: [(Glide, &str, &str); 2] = [
    (Glide::Auto, "auto", "Auto"),
    (Glide::Never, "never", "Never"),
];

/// The appear and disappear effects, as `(value, label)`.
pub const EFFECTS: [(TransitionEffect, &str, &str); 5] = [
    (TransitionEffect::Inherit, "inherit", "Inherit"),
    (TransitionEffect::Fade, "fade", "Fade"),
    (TransitionEffect::Scale, "scale", "Scale"),
    (TransitionEffect::Slide, "slide", "Slide"),
    (TransitionEffect::None, "none", "None"),
];

/// The `(value, label)` pairs of one of the lists above.
pub fn option_pairs<T>(
    list: &[(T, &'static str, &'static str)],
) -> Vec<(&'static str, &'static str)> {
    list.iter()
        .map(|(_, value, name)| (*value, *name))
        .collect()
}

/// One field of an item's scene-switch settings, with the value its select gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwitchField {
    /// The easing.
    Easing,
    /// Whether it glides to other scenes.
    Glide,
    /// How it appears.
    Appear,
    /// How it disappears.
    Disappear,
}

/// The option value a field shows for `transition`.
pub fn switch_value(transition: &ItemTransition, field: SwitchField) -> &'static str {
    let find = |list: &[(TransitionEffect, &'static str, &str)], e: TransitionEffect| {
        list.iter()
            .find(|(v, ..)| *v == e)
            .map_or("inherit", |(_, n, _)| *n)
    };
    match field {
        SwitchField::Easing => ITEM_EASINGS
            .iter()
            .find(|(v, ..)| *v == transition.easing)
            .map_or("inherit", |(_, n, _)| n),
        SwitchField::Glide => GLIDES
            .iter()
            .find(|(v, ..)| *v == transition.glide)
            .map_or("auto", |(_, n, _)| n),
        SwitchField::Appear => find(&EFFECTS, transition.appear),
        SwitchField::Disappear => find(&EFFECTS, transition.disappear),
    }
}

/// `transition` with `field` set to the option `value`, or `None` for a value the field
/// has no option for.
pub fn with_switch(
    transition: &ItemTransition,
    field: SwitchField,
    value: &str,
) -> Option<ItemTransition> {
    let mut next = *transition;
    let effect = || {
        EFFECTS
            .iter()
            .find(|(_, n, _)| *n == value)
            .map(|(v, ..)| *v)
    };
    match field {
        SwitchField::Easing => {
            next.easing = ITEM_EASINGS.iter().find(|(_, n, _)| *n == value)?.0;
        }
        SwitchField::Glide => next.glide = GLIDES.iter().find(|(_, n, _)| *n == value)?.0,
        SwitchField::Appear => next.appear = effect()?,
        SwitchField::Disappear => next.disappear = effect()?,
    }
    Some(next)
}

/// `transition` with a delay of `ms`, rounded and kept between 0 and the field's
/// maximum.
pub fn with_delay(transition: &ItemTransition, ms: f64) -> ItemTransition {
    let mut next = *transition;
    next.delay_ms = ms
        .round()
        .clamp(0.0, f64::from(SWITCH_DELAY_MAX.min(MAX_DELAY_MS))) as u32;
    next
}

/// What the Inspector shows: only which thing, so an edit to it doesn't rebuild the
/// panel (and take the focus from the field being typed in).
#[derive(Clone, Debug, PartialEq)]
enum Shown {
    /// Nothing: no collection, or no scene in it.
    Nothing,
    /// The primary selected item, and the scene it is in.
    Item(SceneId, ItemId),
    /// The selected scene, with no item selected.
    Scene(SceneId),
}

fn shown(collection: Option<&Collection>, selection: &crate::store::Selection) -> Shown {
    let Some(collection) = collection else {
        return Shown::Nothing;
    };
    if let Some((scene, item)) = selection
        .item
        .as_ref()
        .and_then(|id| collection.item_in(selection.scene.as_ref(), id))
    {
        return Shown::Item(scene.scene_id.clone(), item.item_id.clone());
    }
    selection
        .scene
        .as_ref()
        .filter(|id| collection.scene(id).is_some())
        .map_or(Shown::Nothing, |id| Shown::Scene(id.clone()))
}

/// The Inspector panel.
#[component]
pub fn Inspector() -> impl IntoView {
    let studio = expect_context::<Studio>();
    let selection_shown = Memo::new(move |_| {
        let selection = studio.selection.get();
        studio.collection.with(|c| shown(c.as_ref(), &selection))
    });
    // Whether a new width or height keeps the aspect ratio. On by default, as in the
    // prototype, and kept while the Inspector is shown.
    let linked = RwSignal::new(true);

    view! {
        <h2>"Inspector"</h2>
        {move || match selection_shown.get() {
            Shown::Nothing => view! { <p class="placeholder">"Nothing to inspect."</p> }.into_any(),
            Shown::Item(scene, item) => view! {
                <div class="inspector">
                    <ItemHeader item=item.clone() />
                    <Placement scene=scene item=item linked=linked />
                    <Fold title="Properties" open=true>
                        <Properties />
                    </Fold>
                </div>
            }
            .into_any(),
            Shown::Scene(scene) => view! {
                <div class="inspector">
                    <SceneSections scene=scene />
                </div>
            }
            .into_any(),
        }}
    }
}

/// The browser storage key of the remembered folds.
const FOLDS_KEY: &str = "jstream.folds";

/// Which sections are folded, by title, remembered across panels and reloads (G11): a
/// section folded in one Inspector stays folded in the next. Provided as context by the
/// shell; kept in the browser's storage, so it lasts as long as the page when storage is
/// off.
#[derive(Clone, Copy)]
pub struct Folds(RwSignal<HashMap<String, bool>>);

impl Folds {
    /// The folds the browser kept, or none.
    pub fn restore() -> Folds {
        let kept = window()
            .local_storage()
            .ok()
            .flatten()
            .and_then(|storage| storage.get_item(FOLDS_KEY).ok().flatten())
            .map(|text| parse_folds(&text))
            .unwrap_or_default();
        Folds(RwSignal::new(kept))
    }

    /// Whether the section `title` is folded: as it was last left, else `default`.
    fn folded(&self, title: &str, default: bool) -> bool {
        self.0.with(|f| f.get(title).copied().unwrap_or(default))
    }

    /// Remembers the section `title` as folded or not.
    fn set(&self, title: &str, folded: bool) {
        self.0.update(|f| {
            f.insert(title.to_owned(), folded);
        });
        if let Ok(Some(storage)) = window().local_storage() {
            let _ = storage.set_item(FOLDS_KEY, &self.0.with_untracked(folds_json));
        }
    }
}

/// The remembered folds as stored: an object of titles to whether they are folded.
pub fn folds_json(folds: &HashMap<String, bool>) -> String {
    serde_json::to_string(folds).unwrap_or_default()
}

/// Stored folds read back; anything else reads as none.
pub fn parse_folds(text: &str) -> HashMap<String, bool> {
    serde_json::from_str(text).unwrap_or_default()
}

/// A foldable section: an uppercase title, a hint shown while folded, an optional
/// control at the right of its title row, and its body. `open` is how it starts the
/// first time; after that it is as it was last left ([`Folds`]).
#[component]
pub(crate) fn Fold(
    title: &'static str,
    open: bool,
    #[prop(optional, into)] hint: Signal<String>,
    #[prop(optional)] extra: Option<AnyView>,
    children: Children,
) -> impl IntoView {
    let kept = use_context::<Folds>();
    let local = RwSignal::new(!open);
    let folded = move || kept.map_or_else(|| local.get(), |k| k.folded(title, !open));
    let toggle = move |_| {
        let next = !folded();
        match kept {
            Some(k) => k.set(title, next),
            None => local.set(next),
        }
    };
    let hint_text = move || {
        let hint = hint.get();
        (folded() && !hint.is_empty()).then(|| view! { <span class="fold-hint">{hint}</span> })
    };
    view! {
        <section class="fold" class:folded=folded>
            <div class="fold-head">
                <button
                    class="fold-toggle"
                    aria-expanded=move || (!folded()).to_string()
                    on:click=toggle
                >
                    <span class="caret" aria-hidden="true">{move || if folded() { "▸" } else { "▾" }}</span>
                    {title}
                    {hint_text}
                </button>
                {extra}
            </div>
            <div class="fold-body" hidden=folded>{children()}</div>
        </section>
    }
}

/// A star that marks a favourite.
fn star(on: bool) -> impl IntoView {
    view! {
        <svg viewBox="0 0 16 16" width="15" height="15" aria-hidden="true">
            <path
                d="M8 1.5l1.9 4.1 4.5.5-3.4 3 1 4.4L8 11.3 4 13.5l1-4.4-3.4-3 4.5-.5z"
                fill=if on { "#f5b83d" } else { "none" }
                stroke=if on { "#f5b83d" } else { "currentColor" }
                stroke-width="1.3"
                stroke-linejoin="round"
            />
        </svg>
    }
}

/// The favourite button of a header.
fn favourite_button(
    on: impl Fn() -> bool + Copy + Send + Sync + 'static,
    toggle: impl Fn() + Send + Sync + 'static,
) -> impl IntoView {
    let label = move || {
        if on() {
            "Remove from favourites"
        } else {
            "Add to favourites"
        }
    };
    view! {
        <button class="icon-button favourite" class:on=on title=label aria-label=label
            aria-pressed=move || on().to_string() on:click=move |_| toggle()>
            {move || star(on())}
        </button>
    }
}

/// What an item's header shows.
#[derive(Clone, Debug, Default, PartialEq)]
struct Head {
    name: String,
    kind: Option<SourceKind>,
    shared: bool,
    favourite: bool,
    source_id: Option<SourceId>,
    labels: Vec<LabelInfo>,
}

/// The selected item's header (I1): its type icon, its name (in italics when its source
/// is placed in other scenes too), its kind, "+N" for the others selected, its labels'
/// chips, the tag button whose popover opens under the header, and the favourite star.
#[component]
fn ItemHeader(item: ItemId) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let head = {
        let item = item.clone();
        Memo::new(move |_| {
            studio
                .collection
                .with(|c| {
                    let c = c.as_ref()?;
                    let (_, info) = c.item(&item)?;
                    let source = c.source(&info.source_id);
                    Some(Head {
                        name: c.source_name(&info.source_id),
                        kind: source.map(|s| s.kind.clone()),
                        shared: is_shared(c, info),
                        favourite: source.is_some_and(|s| s.meta.favourite),
                        source_id: Some(info.source_id.clone()),
                        labels: source
                            .map(|s| carried(c, &s.meta.labels))
                            .unwrap_or_default(),
                    })
                })
                .unwrap_or_default()
        })
    };
    let more = move || {
        studio
            .selection
            .with(|s| s.items().count().saturating_sub(1))
    };
    let tag = move || {
        head.with(|h| h.kind.as_ref().map(|k| kind_tag(studio, k)))
            .unwrap_or_default()
    };
    let toggle = move || {
        let Some(s) = session.get() else { return };
        let edit = studio.collection.with_untracked(|c| {
            let c = c.as_ref()?;
            let (_, info) = c.item(&item)?;
            let source = c.source(&info.source_id)?;
            let mut meta = source.meta.clone();
            meta.favourite = !meta.favourite;
            Some(Edit::SourceSetMeta {
                source_id: source.source_id.clone(),
                meta,
            })
        });
        if let Some(edit) = edit {
            studio.apply(&s, vec![edit]);
        }
    };
    let icon = move || head.with(|h| icon_of(h.kind.as_ref()));
    let labels = Signal::derive(move || head.with(|h| h.labels.clone()));
    // Only a new source or name rebuilds the tag button, so putting a label on keeps
    // its popover open.
    let picked =
        Memo::new(move |_| head.with(|h| h.source_id.clone().map(|id| (id, h.name.clone()))));
    let popover = TagPopover::new();
    let picker = move || {
        picked
            .get()
            .map(|(_, name)| view! { <TagButton popover=popover name=name always=true /> })
    };
    let pop = move || {
        picked.get().map(|(source_id, _)| {
            view! { <LabelPopover popover=popover target=Tagged::Source(source_id) /> }
        })
    };
    view! {
        <div class="inspector-head">
            {move || view! { <span class="head-icon"><Icon name=icon() size=15 /></span> }}
            <span class="source-title" class:reference=move || head.with(|h| h.shared)
                title=move || head.with(|h| h.name.clone())>
                {move || head.with(|h| h.name.clone())}
            </span>
            <span class="kind-tag">{tag}</span>
            {move || (more() > 0).then(|| view! { <span class="more-tag">"+" {more()}</span> })}
            <LabelChips labels=labels />
            {picker}
            {favourite_button(move || head.with(|h| h.favourite), toggle)}
        </div>
        {pop}
    }
}

/// The item's box, crop and blending.
#[component]
fn Placement(scene: SceneId, item: ItemId, linked: RwSignal<bool>) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    // The item as the collection has it now, as the scene shows it: in a keyframe
    // scene, with its overrides.
    let current = {
        let item = item.clone();
        let scene = scene.clone();
        Memo::new(move |_| {
            studio.collection.with(|c| {
                c.as_ref()
                    .and_then(|c| c.item_in(Some(&scene), &item))
                    .map(|(_, i)| i.clone())
            })
        })
    };
    // The keyframe scene's parent, when the scene is one: there only placement,
    // visibility and opacity can change (ADR-0049).
    let keyframe_of = {
        let scene = scene.clone();
        Memo::new(move |_| {
            studio
                .collection
                .with(|c| c.as_ref()?.parent_of(&scene).map(|p| p.name.clone()))
        })
    };
    let geometry = Memo::new(move |_| {
        current.with(|i| {
            i.as_ref()
                .filter(|i| i.placement == PlacementForm::Geometry)
                .and_then(|i| i.geometry)
        })
    });
    // The source's own size, while the engine knows it.
    let source_size = Memo::new(move |_| {
        let source_id = current.with(|i| i.as_ref().map(|i| i.source_id.clone()))?;
        studio.collection.with(|c| {
            studio
                .sizes
                .with(|sizes| c.as_ref()?.source_size(sizes, &source_id))
        })
    });
    let send = move |edit: Edit| {
        if let Some(s) = session.get() {
            studio.apply(&s, vec![edit]);
        }
    };
    let ids = StoredValue::new((item, scene));

    // W % or H %: the size as a % of the source's own, blank while that isn't known.
    let percent_field = move |across: bool| {
        let value = Signal::derive(move || {
            let size = source_size.get()?;
            percent(&geometry.get()?, size, across).map(f64::from)
        });
        let commit = move |v: f64| {
            let (Some(g), Some(size)) = (geometry.get_untracked(), source_size.get_untracked())
            else {
                return false;
            };
            let Some(placed) = with_percent(&g, size, across, to_f32(v), linked.get_untracked())
            else {
                return false;
            };
            if placed == g {
                return false;
            }
            let (item_id, scene) = ids.get_value();
            send(Edit::ItemSetGeometry {
                item_id,
                scene_id: Some(scene),
                geometry: placed,
            });
            true
        };
        let label = if across { "W %" } else { "H %" };
        view! {
            <NumberField label unit="%" value step=1.0 decimals=1 min=f64::from(MIN_PERCENT)
                max=f64::from(MAX_PERCENT) send=commit />
        }
    };

    let field = move |param: Param| {
        let value =
            Signal::derive(move || geometry.get().map(|g| f64::from(param_value(&g, param))));
        let commit = move |v: f64| {
            let Some(g) = geometry.get_untracked() else {
                return false;
            };
            let Some(placed) = with_param(&g, param, to_f32(v), linked.get_untracked()) else {
                return false;
            };
            if placed == g {
                return false;
            }
            let (item_id, scene) = ids.get_value();
            send(Edit::ItemSetGeometry {
                item_id,
                scene_id: Some(scene),
                geometry: placed,
            });
            true
        };
        let (label, step, decimals, fixed, unit) = match param {
            Param::X => ("X", 1.0, 2, false, "px"),
            Param::Y => ("Y", 1.0, 2, false, "px"),
            Param::W => ("W", 1.0, 2, false, "px"),
            Param::H => ("H", 1.0, 2, false, "px"),
            Param::Rotation => ("Rotation", 0.5, 1, true, "°"),
        };
        let (min, max) = match param {
            Param::X | Param::Y => (-MAX_POSITION, MAX_POSITION),
            Param::W | Param::H => (MIN_SIZE, MAX_SIZE),
            Param::Rotation => (-360.0, 360.0),
        };
        view! {
            <NumberField label unit value step decimals fixed min=f64::from(min)
                max=f64::from(max) send=commit />
        }
    };

    let link_button = view! {
        <button
            class="link-toggle"
            class:on=move || linked.get()
            aria-pressed=move || linked.get().to_string()
            title=move || if linked.get() {
                "Width and height are linked (keeps the aspect ratio). Click to unlink."
            } else {
                "Width and height are independent. Click to link them."
            }
            on:click=move |_| linked.update(|l| *l = !*l)
        >
            <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true" fill="none"
                stroke="currentColor" stroke-width="1.5" stroke-linecap="round">
                <path d="M7 4.5l1-1a2.5 2.5 0 013.5 3.5l-1 1M9 11.5l-1 1A2.5 2.5 0 014.5 9l1-1" />
                // The chain's middle link: gone when the sides are independent.
                <path d="M6.5 9.5l3-3" visibility=move || if linked.get() { "visible" } else { "hidden" } />
            </svg>
            "W/H"
        </button>
    }
    .into_any();

    let depth_field = move |which: Depth3| {
        let value =
            Signal::derive(move || geometry.get().map(|g| f64::from(depth_value(&g, which))));
        let commit = move |v: f64| {
            let Some(g) = geometry.get_untracked() else {
                return false;
            };
            let Some(placed) = with_depth(&g, which, to_f32(v)).filter(|p| *p != g) else {
                return false;
            };
            let (item_id, scene) = ids.get_value();
            send(Edit::ItemSetGeometry {
                item_id,
                scene_id: Some(scene),
                geometry: placed,
            });
            true
        };
        let (label, unit, step, decimals) = match which {
            Depth3::Tilt => ("Tilt (X)", "°", 0.5, 1),
            Depth3::Turn => ("Turn (Y)", "°", 0.5, 1),
            Depth3::Depth => ("Depth (Z)", "px", 1.0, 0),
            Depth3::Perspective => ("Perspective", "px", 10.0, 0),
        };
        let (min, max) = which.range();
        view! {
            <NumberField label unit value step decimals min=f64::from(min)
                max=f64::from(max) send=commit />
        }
    };
    let depth_hint = Signal::derive(move || {
        if geometry.get().is_some_and(|g| in_3d(&g)) {
            "in use".to_owned()
        } else {
            String::new()
        }
    });

    let crop = Memo::new(move |_| current.with(|i| i.as_ref().map(|i| i.crop)));
    let crop_field = move |side: Side, label: &'static str| {
        let value = Signal::derive(move || crop.get().map(|c| f64::from(crop_side(&c, side))));
        let commit = move |v: f64| {
            let Some(c) = crop.get_untracked() else {
                return false;
            };
            let next = with_side(&c, side, to_f32(v));
            if next == c {
                return false;
            }
            let (item_id, scene) = ids.get_value();
            send(Edit::ItemSetCrop {
                item_id,
                scene_id: Some(scene),
                crop: next,
            });
            true
        };
        view! {
            <NumberField label unit="px" value step=1.0 decimals=0 min=0.0
                max=f64::from(MAX_CROP) send=commit />
        }
    };
    let crop_hint = Signal::derive(move || {
        if crop.get().is_some_and(|c| c != Crop::NONE) {
            "cropped".to_owned()
        } else {
            String::new()
        }
    });

    let blend = Memo::new(move |_| current.with(|i| i.as_ref().map(|i| i.blend)));
    let opacity = Signal::derive(move || {
        current.with(|i| i.as_ref().map(|i| f64::from(i.opacity.get()) * 100.0))
    });
    let send_opacity = move |v: f64| {
        let Some(opacity) = Finite32::new(to_f32(v.clamp(0.0, 100.0) / 100.0)) else {
            return false;
        };
        if current.with_untracked(|i| i.as_ref().is_some_and(|i| i.opacity == opacity)) {
            return false;
        }
        let (item_id, scene) = ids.get_value();
        send(Edit::ItemSetOpacity {
            item_id,
            scene_id: Some(scene),
            opacity,
        });
        true
    };
    let send_blend = move |name: String| {
        let Some(mode) = BlendMode::ALL.into_iter().find(|m| blend_name(*m) == name) else {
            return;
        };
        if Some(mode) != blend.get_untracked() {
            send(Edit::ItemSetBlend {
                item_id: ids.get_value().0,
                blend: mode,
            });
        }
    };
    // How the item moves when the program switches scenes (`item.set_transition`).
    let transition = Memo::new(move |_| current.with(|i| i.as_ref().map(|i| i.transition)));
    let send_transition = move |next: ItemTransition| {
        if transition.get_untracked() == Some(next) {
            return false;
        }
        send(Edit::ItemSetTransition {
            item_id: ids.get_value().0,
            transition: next,
        });
        true
    };
    let delay = Signal::derive(move || transition.get().map(|t| f64::from(t.delay_ms)));
    let send_delay = move |ms: f64| {
        transition
            .get_untracked()
            .is_some_and(|t| send_transition(with_delay(&t, ms)))
    };
    let switch_select = move |field: SwitchField,
                              label: &'static str,
                              options: Vec<(&'static str, &'static str)>| {
        view! {
            <label class="select-field">
                <span class="field-label">{label}</span>
                <select
                    aria-label=label
                    prop:value=move || transition.get().map_or("", |t| switch_value(&t, field))
                    on:change=move |event| {
                        let value = event_target_value(&event);
                        if let Some(next) = transition
                            .get_untracked()
                            .and_then(|t| with_switch(&t, field, &value))
                        {
                            send_transition(next);
                        }
                    }
                >
                    {options
                        .into_iter()
                        .map(|(value, name)| view! { <option value=value>{name}</option> })
                        .collect_view()}
                </select>
            </label>
        }
    };
    let switch_hint = Signal::derive(move || {
        if transition.get().is_some_and(|t| !t.is_default()) {
            "customised".to_owned()
        } else {
            String::new()
        }
    });
    let blend_hint = Signal::derive(move || {
        blend
            .get()
            .filter(|m| *m != BlendMode::Normal)
            .map(|m| blend_name(m).to_owned())
            .unwrap_or_default()
    });

    view! {
        <Fold title="Parameters" open=true extra=link_button>
            <Show when=move || geometry.get().is_none()>
                <p class="note">
                    "This item is still placed in the old form, whose box depends on its \
                     source's size, so its box can't be edited here or on the canvas."
                </p>
            </Show>
            <div class="field-grid">
                {field(Param::X)}
                {field(Param::Y)}
                {field(Param::W)}
                {field(Param::H)}
                {percent_field(true)}
                {percent_field(false)}
                {field(Param::Rotation)}
            </div>
        </Fold>
        <Fold title="3D" open=false hint=depth_hint>
            <div class="field-grid">
                {depth_field(Depth3::Tilt)}
                {depth_field(Depth3::Turn)}
                {depth_field(Depth3::Depth)}
                {depth_field(Depth3::Perspective)}
            </div>
        </Fold>
        <Fold title="Crop" open=true hint=crop_hint>
            <div class="field-grid">
                {crop_field(Side::Left, "Left")}
                {crop_field(Side::Right, "Right")}
                {crop_field(Side::Top, "Top")}
                {crop_field(Side::Bottom, "Bottom")}
            </div>
        </Fold>
        <Fold title="Blending" open=true hint=blend_hint>
            {move || keyframe_of.get().map(|parent| view! {
                <p class="note keyframe-note">
                    "Keyframe of “" {parent} "”: the blend mode, filters and properties follow \
                     it. Only placement, visibility and opacity change here."
                </p>
            })}
            <div class="field-grid">
                <label class="select-field">
                    <span class="field-label">"Mode"</span>
                    <select
                        disabled=move || keyframe_of.with(Option::is_some)
                        prop:value=move || blend.get().map_or("Normal", blend_name)
                        on:change=move |event| send_blend(event_target_value(&event))
                    >
                        {BlendMode::ALL
                            .into_iter()
                            .map(|mode| {
                                let name = blend_name(mode);
                                // The engine draws only Normal so far and refuses the others.
                                view! {
                                    <option value=name disabled=mode != BlendMode::Normal>
                                        {name}
                                    </option>
                                }
                            })
                            .collect_view()}
                    </select>
                </label>
                <NumberField label="Opacity" unit="%" value=opacity step=1.0 decimals=0
                    min=0.0 max=100.0 send=send_opacity />
            </div>
        </Fold>
        // A keyframe changes only placement, visibility and opacity (ADR-0049), so its
        // items' switches are the parent's and aren't shown here.
        <Show when=move || keyframe_of.with(Option::is_none)>
            <Fold title="Scene switches" open=false hint=switch_hint>
                <div class="field-grid">
                    <NumberField label="Delay" unit="ms" value=delay step=10.0 decimals=0 min=0.0
                        max=f64::from(SWITCH_DELAY_MAX) send=send_delay />
                    {switch_select(SwitchField::Easing, "Easing", option_pairs(&ITEM_EASINGS))}
                    {switch_select(SwitchField::Glide, "Glide to other scenes", option_pairs(&GLIDES))}
                    {switch_select(SwitchField::Appear, "Appear", option_pairs(&EFFECTS))}
                    {switch_select(SwitchField::Disappear, "Disappear", option_pairs(&EFFECTS))}
                </div>
            </Fold>
        </Show>
    }
}

/// The selected scene's header (its icon, name, kind, its labels' chips, the tag button
/// whose popover opens under the header, and the favourite star) and sections.
#[component]
fn SceneSections(scene: SceneId) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let id = StoredValue::new(scene);
    let info = Memo::new(move |_| {
        studio
            .collection
            .with(|c| c.as_ref().and_then(|c| c.scene(&id.get_value()).cloned()))
    });
    let name = move || info.with(|s| s.as_ref().map(|s| s.name.clone()).unwrap_or_default());
    let labels = Signal::derive(move || {
        studio.collection.with(|c| {
            c.as_ref()
                .zip(info.with(|s| s.as_ref().map(|s| s.meta.labels.clone())))
                .map(|(c, ids)| carried(c, &ids))
                .unwrap_or_default()
        })
    });
    let popover = TagPopover::new();
    let favourite = move || info.with(|s| s.as_ref().is_some_and(|s| s.meta.favourite));
    let toggle = move || {
        let Some(s) = session.get() else { return };
        let Some(scene) = info.get_untracked() else {
            return;
        };
        let mut meta = scene.meta.clone();
        meta.favourite = !meta.favourite;
        studio.apply(
            &s,
            vec![Edit::SceneSetMeta {
                scene_id: scene.scene_id,
                meta,
            }],
        );
    };
    let objects = move || info.with(|s| s.as_ref().map_or(0, |s| s.items.len()));
    let used = move || {
        let used = studio.collection.with(|c| {
            c.as_ref()
                .map(|c| used_in(c, &id.get_value()))
                .unwrap_or_default()
        });
        if used.is_empty() {
            "none".to_owned()
        } else {
            used.join(", ")
        }
    };

    // The presets, from the shared collection.
    let transitions = Memo::new(move |_| {
        studio
            .collection
            .with(|c| c.as_ref().map(|c| c.transitions.clone()))
    });
    let overrides = move || {
        info.with(|s| {
            s.as_ref()
                .map(|s| (s.transition_in.clone(), s.transition_out.clone()))
                .unwrap_or_default()
        })
    };
    let preset_name = move |id: &TransitionPresetId| {
        transitions
            .with(|t| {
                t.as_ref()
                    .and_then(|t| t.presets.iter().find(|p| &p.preset_id == id))
                    .map(|p| p.name.clone())
            })
            .unwrap_or_else(|| id.to_string())
    };
    let hint = Signal::derive(move || match overrides() {
        (Some(i), Some(o)) => format!("in {}, out {}", preset_name(&i), preset_name(&o)),
        (Some(i), None) => format!("in {}", preset_name(&i)),
        (None, Some(o)) => format!("out {}", preset_name(&o)),
        (None, None) => String::new(),
    });
    // Sets one direction and keeps the other.
    let set = move |into: bool, value: String| {
        let Some(s) = session.get() else { return };
        let chosen = (!value.is_empty()).then(|| TransitionPresetId::new(value));
        let (now_in, now_out) = overrides();
        let (transition_in, transition_out) = if into {
            (chosen, now_out)
        } else {
            (now_in, chosen)
        };
        studio.apply(
            &s,
            vec![studio::overrides_edit(
                id.get_value(),
                transition_in,
                transition_out,
            )],
        );
    };
    let select = move |into: bool, label: &'static str| {
        let current = move || {
            let (i, o) = overrides();
            if into { i } else { o }.map_or_else(String::new, |id| id.to_string())
        };
        view! {
            <div class="key-value">
                <span class="key">{label}</span>
                <select
                    aria-label=label
                    disabled=move || transitions.with(Option::is_none)
                    on:change=move |event| set(into, event_target_value(&event))
                >
                    {move || {
                        let now = current();
                        transitions
                            .with(|t| t.as_ref().map(transition_options))
                            .unwrap_or_else(|| vec![(now.clone(), now.clone())])
                            .into_iter()
                            .map(|(value, name)| {
                                let selected = value == now;
                                view! { <option value=value selected=selected>{name}</option> }
                            })
                            .collect_view()
                    }}
                </select>
            </div>
        }
    };

    // A keyframe scene's parent, for its kind tag.
    let keyframe_of = Memo::new(move |_| {
        studio.collection.with(|c| {
            c.as_ref()?
                .parent_of(&id.get_value())
                .map(|p| p.name.clone())
        })
    });
    let kind = move || {
        keyframe_of.get().map_or_else(
            || "Scene".to_owned(),
            |parent| format!("Keyframe of “{parent}”"),
        )
    };
    // A keyframe scene shows the keyframe diamond, as its row does.
    let icon = move || {
        if keyframe_of.with(Option::is_some) {
            return "keyframe";
        }
        info.with(|s| {
            s.as_ref()
                .and_then(|s| s.meta.icon.as_deref())
                .map_or("layers", |icon| named(icon, "layers"))
        })
    };

    view! {
        <div class="inspector-head">
            {move || view! { <span class="head-icon"><Icon name=icon() size=15 /></span> }}
            <span class="source-title" title=name>{name}</span>
            <span class="kind-tag" class:keyframe=move || keyframe_of.with(Option::is_some)>{kind}</span>
            <LabelChips labels=labels />
            <TagButton popover=popover name=name() always=true />
            {favourite_button(favourite, toggle)}
        </div>
        <LabelPopover popover=popover target=Tagged::Scene(id.get_value()) />
        <Fold title="Scene" open=true>
            <div class="key-value"><span class="key">"Objects"</span><span>{objects}</span></div>
            <div class="key-value"><span class="key">"Used as a source in"</span><span>{used}</span></div>
            <HotkeyRow scene=id.get_value() />
        </Fold>
        <Fold title="Transitions" open=true hint=hint>
            {select(true, "Transition in")}
            {select(false, "Transition out")}
            <p class="note">"If both scenes override, the next scene's “in” wins."</p>
        </Fold>
    }
}

/// A scene's Hotkey row: the chords that put it on program, each with a button that
/// unbinds it, and "Set key", which binds the next chord pressed (Escape cancels).
#[component]
fn HotkeyRow(scene: SceneId) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let id = StoredValue::new(scene);
    let chords = Memo::new(move |_| {
        studio.collection.with(|c| {
            c.as_ref()
                .map(|c| c.scene_chords(&id.get_value()))
                .unwrap_or_default()
        })
    });
    let capturing = RwSignal::new(false);
    let send = move |edits: Vec<Edit>| {
        if let (Some(s), false) = (session.get(), edits.is_empty()) {
            studio.apply(&s, edits);
        }
    };
    let on_key = move |event: leptos::ev::KeyboardEvent| {
        // Pause is the window's privacy key, so it is never bound and still toggles
        // privacy while a capture waits.
        if !capturing.get_untracked() || event.key() == "Pause" {
            return;
        }
        // The key is the capture's: no hotkey runs, and Tab doesn't move the focus.
        event.stop_propagation();
        event.prevent_default();
        if event.key() == "Escape" {
            capturing.set(false);
            return;
        }
        let Some(chord) = hotkeys::of_event(&event) else {
            return;
        };
        capturing.set(false);
        let edits = studio.collection.with_untracked(|c| {
            c.as_ref()
                .map(|c| hotkeys::bind_edits(c, &id.get_value(), &chord))
                .unwrap_or_default()
        });
        send(edits);
    };
    let chip = move |chord: Chord| {
        let shown = hotkeys::label(&chord);
        let title = format!("Unbind {shown}");
        view! {
            <span class="hotkey-chip">
                <kbd>{shown}</kbd>
                <button
                    type="button"
                    class="icon-button"
                    title=title.clone()
                    aria-label=title
                    on:click=move |_| send(vec![Edit::HotkeyClear { chord: chord.clone() }])
                >
                    "×"
                </button>
            </span>
        }
    };
    view! {
        <div class="key-value hotkey-row">
            <span class="key">"Hotkey"</span>
            <span class="hotkeys">
                {move || chords.with(Vec::is_empty).then_some(view! { <span class="none">"none"</span> })}
                <For each=move || chords.get() key=|c| c.clone() children=chip />
                <button
                    type="button"
                    class="set-key"
                    class:capturing=move || capturing.get()
                    on:click=move |_| capturing.update(|c| *c = !*c)
                    on:keydown=on_key
                    on:blur=move |_| capturing.set(false)
                >
                    {move || if capturing.get() { "Press a key…" } else { "Set key" }}
                </button>
            </span>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{Size, Vec2};

    fn geometry(cx: f32, cy: f32, w: f32, h: f32) -> ItemGeometry {
        ItemGeometry::try_new(
            Vec2::new(cx, cy).expect("finite"),
            Size::try_new(w, h).expect("positive"),
        )
        .expect("a valid box")
    }

    #[test]
    fn fields_show_the_top_left_corner_and_size() {
        let g = geometry(1614.0, 702.0, 460.0, 410.0);
        assert_eq!(param_value(&g, Param::X), 1384.0);
        assert_eq!(param_value(&g, Param::Y), 497.0);
        assert_eq!(param_value(&g, Param::W), 460.0);
        assert_eq!(param_value(&g, Param::H), 410.0);
        assert_eq!(param_value(&g, Param::Rotation), 0.0);
    }

    #[test]
    fn the_size_reads_and_sets_as_a_percent_of_the_sources() {
        let g = geometry(480.0, 270.0, 960.0, 540.0);
        assert_eq!(percent(&g, (1920, 1080), true), Some(50.0));
        assert_eq!(percent(&g, (1920, 1080), false), Some(50.0));
        assert_eq!(percent(&g, (0, 1080), true), None);
        // 25 % across, linked: the height follows; the top-left corner stays.
        let quarter = with_percent(&g, (1920, 1080), true, 25.0, true).expect("a box");
        assert_eq!(
            (quarter.size.width.get(), quarter.size.height.get()),
            (480.0, 270.0)
        );
        assert_eq!(top_left(&quarter), (0.0, 0.0));
        // Unlinked, H % changes the height alone.
        let tall = with_percent(&g, (1920, 1080), false, 100.0, false).expect("a box");
        assert_eq!(
            (tall.size.width.get(), tall.size.height.get()),
            (960.0, 1080.0)
        );
        // Kept to 0.1 %.
        let tiny = with_percent(&g, (1920, 1080), true, 0.0, false).expect("a box");
        assert!((tiny.size.width.get() - 1.92).abs() < 1e-4);
    }

    #[test]
    fn a_new_position_moves_the_box() {
        let g = geometry(100.0, 100.0, 40.0, 20.0);
        let moved = with_param(&g, Param::X, 0.0, true).expect("a box");
        assert_eq!(top_left(&moved), (0.0, 90.0));
        assert_eq!(moved.size, g.size);
        let moved = with_param(&g, Param::Y, -20000.0, true).expect("a box");
        assert_eq!(top_left(&moved).1, -MAX_POSITION);
    }

    #[test]
    fn a_new_size_keeps_the_top_left_corner() {
        let g = geometry(100.0, 100.0, 40.0, 20.0);
        let wider = with_param(&g, Param::W, 80.0, false).expect("a box");
        assert_eq!(top_left(&wider), (80.0, 90.0));
        assert_eq!(
            (wider.size.width.get(), wider.size.height.get()),
            (80.0, 20.0)
        );
    }

    #[test]
    fn linked_sides_keep_the_aspect_ratio() {
        let g = geometry(0.0, 0.0, 460.0, 410.0);
        let wider = with_param(&g, Param::W, 920.0, true).expect("a box");
        assert_eq!(wider.size.height.get(), 820.0);
        let taller = with_param(&g, Param::H, 100.0, true).expect("a box");
        assert_eq!(taller.size.width.get(), 112.2);
        // The linked side never goes below the minimum.
        let thin = geometry(0.0, 0.0, 1000.0, 2.0);
        let narrow = with_param(&thin, Param::W, 10.0, true).expect("a box");
        assert_eq!(narrow.size.height.get(), MIN_SIZE);
        // Nor above the maximum.
        let tall = geometry(0.0, 0.0, 10.0, 1000.0);
        let wide = with_param(&tall, Param::W, 5000.0, true).expect("a box");
        assert_eq!(wide.size.height.get(), MAX_SIZE);
    }

    #[test]
    fn sizes_and_rotations_stay_in_range() {
        let g = geometry(0.0, 0.0, 40.0, 20.0);
        let tiny = with_param(&g, Param::W, -5.0, false).expect("a box");
        assert_eq!(tiny.size.width.get(), MIN_SIZE);
        let turned = with_param(&g, Param::Rotation, 400.0, false).expect("a box");
        assert_eq!(turned.rotation_deg.get(), 360.0);
        assert_eq!(turned.center, g.center);
    }

    #[test]
    fn crop_sides_round_and_stay_in_range() {
        let crop = with_side(&Crop::NONE, Side::Left, 24.4);
        assert_eq!(crop_side(&crop, Side::Left), 24);
        let crop = with_side(&crop, Side::Bottom, 9000.0);
        assert_eq!(crop.bottom, 4000);
        let crop = with_side(&crop, Side::Top, -3.0);
        assert_eq!(crop.top, 0);
        assert_eq!(crop.left, 24);
    }

    #[test]
    fn a_first_3d_change_brings_a_perspective() {
        let g = geometry(100.0, 100.0, 40.0, 20.0);
        assert!(!in_3d(&g));
        assert_eq!(depth_value(&g, Depth3::Perspective), DEFAULT_PERSPECTIVE);
        let tilted = with_depth(&g, Depth3::Tilt, 30.0).expect("a box");
        assert!(in_3d(&tilted));
        assert_eq!(tilted.tilt_deg.get(), 30.0);
        assert_eq!(
            tilted.perspective_px.map(Finite32::get),
            Some(DEFAULT_PERSPECTIVE)
        );
        // A set perspective is kept, and every field stays in its range.
        let far = with_depth(&tilted, Depth3::Perspective, 50_000.0).expect("a box");
        assert_eq!(far.perspective_px.map(Finite32::get), Some(20_000.0));
        let turned = with_depth(&far, Depth3::Turn, -400.0).expect("a box");
        assert_eq!(depth_value(&turned, Depth3::Turn), -180.0);
        assert_eq!(turned.perspective_px, far.perspective_px);
        // Setting only the perspective leaves the item flat.
        let flat = with_depth(&g, Depth3::Perspective, 800.0).expect("a box");
        assert!(!in_3d(&flat));
        assert_eq!(flat.center, g.center);
    }

    #[test]
    fn folds_read_back_what_was_stored() {
        let mut folds = HashMap::new();
        folds.insert("3D".to_owned(), false);
        folds.insert("Crop".to_owned(), true);
        assert_eq!(parse_folds(&folds_json(&folds)), folds);
        assert!(parse_folds("not json").is_empty());
    }

    #[test]
    fn every_blend_mode_has_a_distinct_name() {
        let names: Vec<_> = BlendMode::ALL.into_iter().map(blend_name).collect();
        assert_eq!(names[0], "Normal");
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len());
    }

    #[test]
    fn scene_switch_fields_read_and_write_the_item_s_settings() {
        let t = ItemTransition::default();
        for field in [
            SwitchField::Easing,
            SwitchField::Appear,
            SwitchField::Disappear,
        ] {
            assert_eq!(switch_value(&t, field), "inherit");
        }
        assert_eq!(switch_value(&t, SwitchField::Glide), "auto");
        let next = with_switch(&t, SwitchField::Glide, "never").expect("an option");
        assert_eq!(next.glide, Glide::Never);
        let next = with_switch(&next, SwitchField::Appear, "slide").expect("an option");
        assert_eq!(next.appear, TransitionEffect::Slide);
        assert_eq!(switch_value(&next, SwitchField::Appear), "slide");
        let next = with_switch(&next, SwitchField::Easing, "spring").expect("an option");
        assert_eq!(next.easing, ItemEasing::Spring);
        assert_eq!(with_switch(&t, SwitchField::Disappear, "spin"), None);
        assert!(!next.is_default());
        assert_eq!(with_delay(&t, 152.4).delay_ms, 152);
        assert_eq!(with_delay(&t, 9000.0).delay_ms, SWITCH_DELAY_MAX);
        assert_eq!(with_delay(&t, -5.0).delay_ms, 0);
    }
}
