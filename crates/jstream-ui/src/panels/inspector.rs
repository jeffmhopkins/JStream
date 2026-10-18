//! The Inspector: what is selected, and how it is placed. For a selected item, a header
//! (name, kind, how many more are selected, favourite) over foldable sections: its box
//! (X, Y, W, H and rotation, with a W/H link), its crop, its blending (mode and
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
use crate::store::{Collection, Studio};
use jstream_control::protocol::{
    BlendMode, Crop, Edit, Finite32, ItemGeometry, ItemId, ItemInfo, PlacementForm, SceneId,
    SourceKind, TransitionPresetId, TransitionsInfo,
};
use leptos::prelude::*;
use leptos::task::spawn_local;

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
        Param::W if linked => (x, y, value, round2(value * h0 / w0).max(MIN_SIZE), rotation),
        Param::H if linked => (x, y, round2(value * w0 / h0).max(MIN_SIZE), value, rotation),
        Param::W => (x, y, value, h0, rotation),
        Param::H => (x, y, w0, value, rotation),
        Param::Rotation => (x, y, w0, h0, value),
    };
    crate::canvas::transform::with_box(geometry, x + w / 2.0, y + h / 2.0, w, h, rotation)
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

/// The scenes that show `scene_id` as a source, by name, in display order.
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
        .filter(|scene| scene.items.iter().any(shows_it))
        .map(|scene| scene.name.clone())
        .collect()
}

/// Whether the item's source is placed in more than one scene, so the item is one of
/// several references to it and its name reads in italics.
pub fn is_shared(collection: &Collection, item: &ItemInfo) -> bool {
    collection
        .scenes
        .iter()
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
    if let Some((scene, item)) = selection.item.as_ref().and_then(|id| collection.item(id)) {
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

/// A foldable section: an uppercase title, a hint shown while folded, an optional
/// control at the right of its title row, and its body.
#[component]
fn Fold(
    title: &'static str,
    open: bool,
    #[prop(optional, into)] hint: Signal<String>,
    #[prop(optional)] extra: Option<AnyView>,
    children: Children,
) -> impl IntoView {
    let folded = RwSignal::new(!open);
    let hint_text = move || {
        let hint = hint.get();
        (folded.get() && !hint.is_empty()).then(|| view! { <span class="fold-hint">{hint}</span> })
    };
    view! {
        <section class="fold" class:folded=move || folded.get()>
            <div class="fold-head">
                <button
                    class="fold-toggle"
                    aria-expanded=move || (!folded.get()).to_string()
                    on:click=move |_| folded.update(|f| *f = !*f)
                >
                    <span class="caret" aria-hidden="true">{move || if folded.get() { "▸" } else { "▾" }}</span>
                    {title}
                    {hint_text}
                </button>
                {extra}
            </div>
            <div class="fold-body" hidden=move || folded.get()>{children()}</div>
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
}

/// The selected item's header: its name (in italics when its source is placed in other
/// scenes too), its kind, "+N" for the others selected, and the favourite star.
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
    view! {
        <div class="inspector-head">
            <span class="source-title" class:reference=move || head.with(|h| h.shared)
                title=move || head.with(|h| h.name.clone())>
                {move || head.with(|h| h.name.clone())}
            </span>
            <span class="kind-tag">{tag}</span>
            {move || (more() > 0).then(|| view! { <span class="more-tag">"+" {more()}</span> })}
            {favourite_button(move || head.with(|h| h.favourite), toggle)}
        </div>
    }
}

/// The item's box, crop and blending.
#[component]
fn Placement(scene: SceneId, item: ItemId, linked: RwSignal<bool>) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    // The item as the collection has it now.
    let current = {
        let item = item.clone();
        Memo::new(move |_| {
            studio.collection.with(|c| {
                c.as_ref()
                    .and_then(|c| c.item(&item))
                    .map(|(_, i)| i.clone())
            })
        })
    };
    let geometry = Memo::new(move |_| {
        current.with(|i| {
            i.as_ref()
                .filter(|i| i.placement == PlacementForm::Geometry)
                .and_then(|i| i.geometry)
        })
    });
    let send = move |edit: Edit| {
        if let Some(s) = session.get() {
            studio.apply(&s, vec![edit]);
        }
    };
    let ids = StoredValue::new((item, scene));

    let field = move |param: Param| {
        let value =
            Signal::derive(move || geometry.get().map(|g| f64::from(param_value(&g, param))));
        let commit = move |v: f64| {
            let Some(g) = geometry.get_untracked() else {
                return;
            };
            let Some(placed) = with_param(&g, param, to_f32(v), linked.get_untracked()) else {
                return;
            };
            if placed != g {
                let (item_id, scene) = ids.get_value();
                send(Edit::ItemSetGeometry {
                    item_id,
                    scene_id: Some(scene),
                    geometry: placed,
                });
            }
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

    let crop = Memo::new(move |_| current.with(|i| i.as_ref().map(|i| i.crop)));
    let crop_field = move |side: Side, label: &'static str| {
        let value = Signal::derive(move || crop.get().map(|c| f64::from(crop_side(&c, side))));
        let commit = move |v: f64| {
            let Some(c) = crop.get_untracked() else {
                return;
            };
            let next = with_side(&c, side, to_f32(v));
            if next != c {
                let (item_id, scene) = ids.get_value();
                send(Edit::ItemSetCrop {
                    item_id,
                    scene_id: Some(scene),
                    crop: next,
                });
            }
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
            return;
        };
        let (item_id, scene) = ids.get_value();
        send(Edit::ItemSetOpacity {
            item_id,
            scene_id: Some(scene),
            opacity,
        });
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
                {field(Param::Rotation)}
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
            <div class="field-grid">
                <label class="select-field">
                    <span class="field-label">"Mode"</span>
                    <select
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
    }
}

/// The selected scene's header and sections.
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

    // The presets, read with the scenes (`state.get`) whenever the collection changes.
    let transitions = RwSignal::new(None::<TransitionsInfo>);
    let rev = Memo::new(move |_| studio.collection.with(|c| c.as_ref().map(|c| c.rev)));
    Effect::new(move |_| {
        rev.track();
        let Some(s) = session.get() else { return };
        spawn_local(async move {
            if let Ok(snapshot) = studio::load(&*s.control).await {
                transitions.set(Some(snapshot.transitions));
            }
        });
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

    view! {
        <div class="inspector-head">
            <span class="source-title" title=name>{name}</span>
            <span class="kind-tag">"Scene"</span>
            {favourite_button(favourite, toggle)}
        </div>
        <Fold title="Scene" open=true>
            <div class="key-value"><span class="key">"Objects"</span><span>{objects}</span></div>
            <div class="key-value"><span class="key">"Used as a source in"</span><span>{used}</span></div>
        </Fold>
        <Fold title="Transitions" open=true hint=hint>
            {select(true, "Transition in")}
            {select(false, "Transition out")}
            <p class="note">"If both scenes override, the next scene's “in” wins."</p>
        </Fold>
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
    fn every_blend_mode_has_a_distinct_name() {
        let names: Vec<_> = BlendMode::ALL.into_iter().map(blend_name).collect();
        assert_eq!(names[0], "Normal");
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len());
    }
}
