//! The right-click menu of the selected items, shared by the canvas and the Scene
//! objects panel (spec-workspace.md 5.2.8 to 5.2.12): Properties, the Audio check,
//! layer order, Center, Fit and Stretch to canvas, Reset rotation, Clear crop, Crop
//! (crop mode), the two duplicates and Remove from scene, and for a placement of a shared
//! or copied source the switch between a reference and a clone (decision register D3,
//! ADR-0023). Each choice sends one batch, so one undo step. In a keyframe scene only
//! geometry, crop, visibility and opacity change (ADR-0049), so what would change the
//! parent's items (layer order, the duplicates, Remove, the reference or clone switch,
//! Group selection, New group, Ungroup, Add source) is disabled, and the placement
//! choices write the keyframe's overrides.
//!
//! Docs: docs/UI-FEATURES.md#menus

use crate::app::SessionContext;
use crate::canvas::snap::Bounds;
use crate::canvas::transform::{nudge, with_box};
use crate::dock::PanelId;
use crate::menu::Entry;
use crate::panels::add_source::{AddSourceDialog, has_picture};
use crate::panels::filters::FilterDialogs;
use crate::panels::source_properties::{self, PropertiesDialog};
use crate::store::{Collection, Studio};
use crate::workspace::DockState;
use jstream_control::protocol::{
    Canvas, Crop, Edit, Finite32, FitMode, ItemGeometry, ItemId, ItemInfo, SceneId, SceneRole,
    Settings, Size, SourceId, SourceKind, Vec2,
};
use leptos::prelude::*;
use std::rc::Rc;

/// How far a duplicate sits from its original, right and down, in canvas pixels.
const DUPLICATE_OFFSET: f32 = 40.0;

/// Where the menu opened, which decides its last lines.
#[derive(Clone)]
pub enum Scope {
    /// On the canvas: the menu ends with Select all, which runs this.
    Canvas(Rc<dyn Fn()>),
    /// In the Scene objects panel: it ends with Add source and New group.
    Objects,
}

/// A move in the stacking order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    /// To the top of the scene.
    Front,
    /// One place up.
    Forward,
    /// One place down.
    Backward,
    /// To the bottom of the scene.
    Back,
}

/// The `item.reorder` edits, in the order they apply, that move the `selected` items
/// of a scene stacked as `order` (bottom first). The selected items keep their order
/// among themselves, and one step up or down stops at the edge, or at a selected item
/// that can't move.
pub fn restack(order: &[ItemId], selected: &[ItemId], layer: Layer) -> Vec<(ItemId, u32)> {
    let mut stack = order.to_vec();
    let mut moves = Vec::new();
    let mut put = |stack: &mut Vec<ItemId>, from: usize, to: usize| {
        if from != to {
            let id = stack.remove(from);
            stack.insert(to, id.clone());
            moves.push((id, u32::try_from(to).unwrap_or(u32::MAX)));
        }
    };
    let chosen = |id: &ItemId| selected.contains(id);
    match layer {
        Layer::Front => {
            let ids: Vec<ItemId> = order.iter().filter(|id| chosen(id)).cloned().collect();
            for id in ids {
                if let Some(from) = stack.iter().position(|s| *s == id) {
                    let top = stack.len() - 1;
                    put(&mut stack, from, top);
                }
            }
        }
        Layer::Back => {
            let ids: Vec<ItemId> = order
                .iter()
                .rev()
                .filter(|id| chosen(id))
                .cloned()
                .collect();
            for id in ids {
                if let Some(from) = stack.iter().position(|s| *s == id) {
                    put(&mut stack, from, 0);
                }
            }
        }
        Layer::Forward => {
            for at in (0..stack.len().saturating_sub(1)).rev() {
                if chosen(&stack[at]) && !chosen(&stack[at + 1]) {
                    put(&mut stack, at, at + 1);
                }
            }
        }
        Layer::Backward => {
            for at in 1..stack.len() {
                if chosen(&stack[at]) && !chosen(&stack[at - 1]) {
                    put(&mut stack, at, at - 1);
                }
            }
        }
    }
    moves
}

/// The boxes moved together so that their bounds sit in the middle of the canvas.
pub fn centered(placed: &[(ItemId, ItemGeometry)], canvas: Canvas) -> Vec<(ItemId, ItemGeometry)> {
    let Some(bounds) = Bounds::around(placed.iter().map(|(_, g)| g)) else {
        return Vec::new();
    };
    let dx = canvas.width as f32 * 0.5 - (bounds.left + bounds.right) * 0.5;
    let dy = canvas.height as f32 * 0.5 - (bounds.top + bounds.bottom) * 0.5;
    placed
        .iter()
        .filter_map(|(id, g)| Some((id.clone(), nudge(g, dx, dy)?)))
        .collect()
}

/// The box scaled to the largest that fits the canvas, keeping its shape, centred and
/// upright.
pub fn fitted(geometry: &ItemGeometry, canvas: Canvas) -> Option<ItemGeometry> {
    let (cw, ch) = (canvas.width as f32, canvas.height as f32);
    let (w, h) = (geometry.size.width.get(), geometry.size.height.get());
    let scale = (cw / w).min(ch / h);
    with_box(geometry, cw * 0.5, ch * 0.5, w * scale, h * scale, 0.0)
}

/// The box covering the canvas, upright.
pub fn stretched(geometry: &ItemGeometry, canvas: Canvas) -> Option<ItemGeometry> {
    let (cw, ch) = (canvas.width as f32, canvas.height as f32);
    with_box(geometry, cw * 0.5, ch * 0.5, cw, ch, 0.0)
}

/// The box with no rotation, tilt or turn.
pub fn unrotated(geometry: &ItemGeometry) -> ItemGeometry {
    ItemGeometry {
        rotation_deg: Finite32::ZERO,
        tilt_deg: Finite32::ZERO,
        turn_deg: Finite32::ZERO,
        ..*geometry
    }
}

/// Whether an item's placement is a reference to a shared source or a clone of one, which
/// the menu switches between (spec-workspace.md 9.3, D3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sharing {
    /// The source was copied from another, which still exists, so the item can go back
    /// to placing it.
    Clone {
        /// The source it was copied from.
        original: SourceId,
        /// That source's name.
        name: String,
        /// Whether nothing else places the copy, which then goes too.
        alone: bool,
    },
    /// The source is placed more than once, so the item can take a copy of its own.
    Reference,
    /// Neither: the only placement of a source of its own.
    Own,
}

impl Sharing {
    /// What the item's placement is.
    pub fn of(collection: &Collection, item: &ItemInfo) -> Sharing {
        let placements = collection
            .scenes
            .iter()
            .flat_map(|scene| &scene.items)
            .filter(|i| i.source_id == item.source_id)
            .count();
        let original = collection
            .source(&item.source_id)
            .and_then(|s| s.duplicated_from.as_ref())
            .and_then(|from| collection.source(from));
        match original {
            Some(original) => Sharing::Clone {
                original: original.source_id.clone(),
                name: original.name.clone(),
                alone: placements <= 1,
            },
            None if placements > 1 => Sharing::Reference,
            None => Sharing::Own,
        }
    }
}

/// The edits that turn `item_id`, placing `source_id` named `name`, into an independent
/// clone placing `copy`.
pub fn to_clone(item_id: &ItemId, source_id: &SourceId, name: &str, copy: SourceId) -> Vec<Edit> {
    vec![
        Edit::SourceDuplicate {
            source_id: source_id.clone(),
            new_source_id: copy.clone(),
            name: name.to_owned(),
        },
        Edit::ItemSetSource {
            item_id: item_id.clone(),
            source_id: copy,
        },
    ]
}

/// The edits that make the clone `item_id`, placing `source_id`, a reference to
/// `original` again, deleting the copy when nothing else places it.
pub fn to_reference(
    item_id: &ItemId,
    source_id: &SourceId,
    original: &SourceId,
    alone: bool,
) -> Vec<Edit> {
    let mut edits = vec![Edit::ItemSetSource {
        item_id: item_id.clone(),
        source_id: original.clone(),
    }];
    if alone {
        edits.push(Edit::SourceDelete {
            source_id: source_id.clone(),
        });
    }
    edits
}

/// "Group N", with the first N no scene is named (spec-workspace.md 9.5).
pub fn group_name(collection: &Collection) -> String {
    (1..)
        .map(|n| format!("Group {n}"))
        .find(|name| !collection.scenes.iter().any(|s| s.name == *name))
        .unwrap_or_else(|| "Group".into())
}

/// The IDs a new group takes: its scene, the `scene` source showing it, and its item.
#[derive(Clone, Debug)]
pub struct GroupIds {
    /// The group's scene.
    pub scene: SceneId,
    /// The source that places it.
    pub source: SourceId,
    /// Its item in the scene it is made in.
    pub item: ItemId,
}

/// The batch that groups `items` of `scene_id` (D4, spec-workspace.md 9.5): a new group
/// scene, a source showing it placed over the whole canvas where the topmost of them was
/// (on top with none), and each of them moved into it, keeping its ID, box and order. The
/// group shares the canvas, so the children keep their places on screen.
pub fn group_edits(
    collection: &Collection,
    scene_id: &SceneId,
    items: &[ItemId],
    ids: GroupIds,
) -> Vec<Edit> {
    let Some(scene) = collection.scene(scene_id) else {
        return Vec::new();
    };
    let order: Vec<&ItemId> = scene.items.iter().map(|i| &i.item_id).collect();
    let chosen: Vec<&ItemId> = order
        .iter()
        .copied()
        .filter(|id| items.contains(id))
        .collect();
    let name = group_name(collection);
    let mut edits = vec![
        Edit::SceneCreate {
            scene_id: ids.scene.clone(),
            name: name.clone(),
            role: Some(SceneRole::Group),
        },
        Edit::SourceCreate {
            source_id: ids.source.clone(),
            name,
            kind: SourceKind::Scene {
                scene_id: ids.scene.clone(),
            },
            settings: Settings::new(),
            duplicated_from: None,
        },
    ];
    for (index, item_id) in chosen.iter().enumerate() {
        edits.push(Edit::ItemMove {
            item_id: (*item_id).clone(),
            scene_id: ids.scene.clone(),
            index: u32::try_from(index).unwrap_or(u32::MAX),
        });
    }
    let canvas = collection.canvas;
    edits.push(Edit::ItemAdd {
        scene_id: scene_id.clone(),
        item_id: ids.item.clone(),
        source_id: ids.source,
        transform: None,
        geometry: ItemGeometry::covering_canvas(canvas.width, canvas.height).ok(),
        opacity: None,
        blend: None,
    });
    // Where the topmost chosen item was, counted among the items that stay.
    let left = order.len() - chosen.len();
    if let Some(top) = chosen.last() {
        let at = order
            .iter()
            .take_while(|id| *id != top)
            .filter(|id| !chosen.contains(id))
            .count();
        if at < left {
            edits.push(Edit::ItemReorder {
                item_id: ids.item,
                index: u32::try_from(at).unwrap_or(u32::MAX),
            });
        }
    }
    edits
}

/// Why a group can't be ungrouped from the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kept {
    /// Another item places the group too, and would lose its items.
    PlacedElsewhere,
    /// The group's placement can't be carried onto its items as their own boxes: the
    /// group item is cropped, faded, blended, filtered, mirrored or turned in 3D, the
    /// group scene has filters of its own, or, moved, it would show an item it clips,
    /// stretch one that is turned or fitted, or move one with no box.
    Look,
}

impl Kept {
    /// The menu's short reason.
    pub fn reason(self) -> &'static str {
        match self {
            Kept::PlacedElsewhere => "placed elsewhere",
            Kept::Look => "keeps its look",
        }
    }
}

/// How a group's placement maps a box in the group to one in its parent (ADR-0024 §1):
/// scaled about the canvas centre by `scale`, turned by `turn_deg` and centred on
/// `center`.
#[derive(Clone, Copy, Debug)]
struct Carry {
    canvas: (f32, f32),
    center: (f32, f32),
    scale: (f32, f32),
    turn_deg: f32,
}

impl Carry {
    /// The map for `group`'s placement, `None` when it is the default, which leaves
    /// every box as it is.
    fn of(group: &ItemInfo, canvas: Canvas) -> Result<Option<Carry>, Kept> {
        if group.crop != Crop::NONE
            || group.opacity != Finite32::ONE
            || group.blend != Default::default()
            || !group.effects.is_empty()
        {
            return Err(Kept::Look);
        }
        let Some(g) = group.geometry else {
            // The 0.3 form: the identity draws the group's canvas over the parent's.
            return if group.transform == jstream_control::protocol::ItemTransform::IDENTITY {
                Ok(None)
            } else {
                Err(Kept::Look)
            };
        };
        if ItemGeometry::covering_canvas(canvas.width, canvas.height).ok() == Some(g) {
            return Ok(None);
        }
        if !g.is_neutral_3d() || g.flip_h || g.flip_v {
            return Err(Kept::Look);
        }
        let (w, h) = (canvas.width as f32, canvas.height as f32);
        let (sx, sy) = (g.size.width.get() / w, g.size.height.get() / h);
        let uniform = (sx - sy).abs() <= 1e-4 * sx.max(sy);
        let scale = match g.fit_mode {
            FitMode::Stretch => (sx, sy),
            // The group's canvas keeps its shape, centred in the box.
            FitMode::Fit => (sx.min(sy), sx.min(sy)),
            // Fill crops a box of another shape, and a mode this client doesn't know may.
            _ if uniform => (sx, sy),
            _ => return Err(Kept::Look),
        };
        Ok(Some(Carry {
            canvas: (w, h),
            center: (g.center.x.get(), g.center.y.get()),
            scale,
            turn_deg: g.rotation_deg.get(),
        }))
    }

    /// `child`'s box in the parent.
    fn apply(self, child: &ItemInfo) -> Result<ItemGeometry, Kept> {
        let c = child.geometry.ok_or(Kept::Look)?;
        if !c.is_neutral_3d() {
            return Err(Kept::Look);
        }
        // The group draws its canvas as one picture, clipping what lies outside it, which
        // the item would show once it is drawn in the parent.
        if !within(c, self.canvas) {
            return Err(Kept::Look);
        }
        let (sx, sy) = self.scale;
        let uniform = (sx - sy).abs() <= 1e-4 * sx.max(sy);
        // A stretch of a fitted or filled item would change its content's shape.
        if !uniform && c.fit_mode != FitMode::Stretch {
            return Err(Kept::Look);
        }
        let own = c.rotation_deg.get().rem_euclid(180.0);
        // A stretch keeps a box a box only when the box isn't turned against it.
        let (bw, bh) = if uniform || own == 0.0 {
            (sx, sy)
        } else if own == 90.0 {
            (sy, sx)
        } else {
            return Err(Kept::Look);
        };
        let x = (c.center.x.get() - self.canvas.0 * 0.5) * sx;
        let y = (c.center.y.get() - self.canvas.1 * 0.5) * sy;
        // Clockwise with y down.
        let (sin, cos) = self.turn_deg.to_radians().sin_cos();
        let center = Vec2::new(
            self.center.0 + x * cos - y * sin,
            self.center.1 + x * sin + y * cos,
        )
        .ok_or(Kept::Look)?;
        let size = Size::try_new(c.size.width.get() * bw, c.size.height.get() * bh)
            .map_err(|_| Kept::Look)?;
        let turned = (c.rotation_deg.get() + self.turn_deg + 180.0).rem_euclid(360.0) - 180.0;
        let rotation_deg = Finite32::new(turned).ok_or(Kept::Look)?;
        Ok(ItemGeometry {
            center,
            size,
            rotation_deg,
            ..c
        })
    }
}

/// Whether the box `g`, flat, lies on the canvas `(width, height)`.
fn within(g: ItemGeometry, canvas: (f32, f32)) -> bool {
    let (half_w, half_h) = (g.size.width.get() * 0.5, g.size.height.get() * 0.5);
    let (sin, cos) = g.rotation_deg.get().to_radians().sin_cos();
    let (cx, cy) = (g.center.x.get(), g.center.y.get());
    let slack = 0.5;
    [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
        .iter()
        .all(|(u, v)| {
            let (x, y) = (u * half_w, v * half_h);
            let (px, py) = (cx + x * cos - y * sin, cy + x * sin + y * cos);
            (-slack..=canvas.0 + slack).contains(&px) && (-slack..=canvas.1 + slack).contains(&py)
        })
}

/// The batch that ungroups the nested scene `item_id` places: its items move into the
/// item's scene where it was, in their order, each keeping where it draws (ADR-0024 §1),
/// and hidden if the group was, and the item goes. A group scene that nothing else places goes too, with its unplaced
/// `scene` sources. `None` when the item doesn't place a scene; [`Kept`] when another
/// item places it too, or its placement can't be carried onto its items.
pub fn ungroup_edits(collection: &Collection, item_id: &ItemId) -> Option<Result<Vec<Edit>, Kept>> {
    let (parent, item) = collection.item(item_id)?;
    let SourceKind::Scene { scene_id: group } = &collection.source(&item.source_id)?.kind else {
        return None;
    };
    let nested = collection.scene(group)?;
    let at = parent.items.iter().position(|i| i.item_id == *item_id)?;
    let shows_group =
        |kind: &SourceKind| matches!(kind, SourceKind::Scene { scene_id } if scene_id == group);
    let sources: Vec<&SourceId> = collection
        .sources
        .iter()
        .filter(|s| shows_group(&s.kind))
        .map(|s| &s.source_id)
        .collect();
    let placed_elsewhere = collection
        .scenes
        .iter()
        .flat_map(|s| &s.items)
        .any(|i| i.item_id != *item_id && sources.contains(&&i.source_id));
    if placed_elsewhere {
        return Some(Err(Kept::PlacedElsewhere));
    }
    // The group's own filters run on its composite; deleting the scene would drop them.
    if !nested.filters.video.is_empty() {
        return Some(Err(Kept::Look));
    }
    let carry = match Carry::of(item, collection.canvas) {
        Ok(carry) => carry,
        Err(kept) => return Some(Err(kept)),
    };
    let mut boxes = Vec::new();
    if let Some(carry) = carry {
        for child in &nested.items {
            match carry.apply(child) {
                Ok(geometry) => boxes.push(Edit::ItemSetGeometry {
                    item_id: child.item_id.clone(),
                    scene_id: Some(parent.scene_id.clone()),
                    geometry,
                }),
                Err(kept) => return Some(Err(kept)),
            }
        }
    }
    let mut edits: Vec<Edit> = nested
        .items
        .iter()
        .enumerate()
        .map(|(k, child)| Edit::ItemMove {
            item_id: child.item_id.clone(),
            scene_id: parent.scene_id.clone(),
            index: u32::try_from(at + k).unwrap_or(u32::MAX),
        })
        .collect();
    edits.extend(boxes);
    // A hidden group hides everything in it.
    if !item.visible {
        edits.extend(
            nested
                .items
                .iter()
                .filter(|child| child.visible)
                .map(|child| Edit::ItemSetVisible {
                    item_id: child.item_id.clone(),
                    scene_id: Some(parent.scene_id.clone()),
                    visible: false,
                }),
        );
    }
    edits.push(Edit::ItemRemove {
        item_id: item_id.clone(),
    });
    if nested.role == SceneRole::Group {
        edits.extend(sources.into_iter().map(|source_id| Edit::SourceDelete {
            source_id: source_id.clone(),
        }));
        edits.push(Edit::SceneDelete {
            scene_id: group.clone(),
        });
    }
    Some(Ok(edits))
}

/// What the menu needs to know about the selection.
#[derive(Clone, Debug)]
struct Facts {
    /// The scene's items, bottom first.
    order: Vec<ItemId>,
    canvas: Canvas,
    /// The selected items, the primary one first.
    items: Vec<ItemId>,
    /// The selected items that have a box, with it.
    placed: Vec<(ItemId, ItemGeometry)>,
    /// Whether any selected item is cropped.
    cropped: bool,
    /// The primary item's source, its name and kind, and whether it has a picture.
    source_id: SourceId,
    name: String,
    kind: SourceKind,
    picture: bool,
    /// Whether the primary item's source plays its audio, if it has any.
    audio: Option<bool>,
    /// Whether the scene is a keyframe scene, which shows its parent's items.
    keyframe: bool,
    /// Whether the primary item is a reference or a clone.
    sharing: Sharing,
}

impl Facts {
    /// The facts for `items` of `scene_id`, keeping only the items in that scene; `None`
    /// when none of them is.
    fn of(collection: &Collection, scene_id: &SceneId, items: &[ItemId]) -> Option<Facts> {
        let scene = collection.scene(scene_id)?;
        let selected: Vec<_> = items
            .iter()
            .filter_map(|id| scene.items.iter().find(|i| i.item_id == *id))
            .collect();
        let primary = selected.first()?;
        let source = collection.source(&primary.source_id)?;
        let picture = match &source.kind {
            SourceKind::Builtin { kind } => has_picture(kind),
            _ => true,
        };
        Some(Facts {
            order: scene.items.iter().map(|i| i.item_id.clone()).collect(),
            canvas: collection.canvas,
            items: selected.iter().map(|i| i.item_id.clone()).collect(),
            placed: selected
                .iter()
                .filter_map(|i| Some((i.item_id.clone(), i.geometry?)))
                .collect(),
            cropped: selected.iter().any(|i| i.crop != Crop::NONE),
            source_id: source.source_id.clone(),
            name: source.name.clone(),
            kind: source.kind.clone(),
            picture,
            audio: source.audio.as_ref().map(|a| a.enabled),
            keyframe: scene.variant_of.is_some(),
            sharing: Sharing::of(collection, primary),
        })
    }

    /// Each placed item's box changed by `how`, given the canvas; an item it can't
    /// change stays out.
    fn each(
        &self,
        how: fn(&ItemGeometry, Canvas) -> Option<ItemGeometry>,
    ) -> Vec<(ItemId, ItemGeometry)> {
        self.placed
            .iter()
            .filter_map(|(id, g)| Some((id.clone(), how(g, self.canvas)?)))
            .collect()
    }
}

/// What the menu acts through, from the page's contexts.
#[derive(Clone, Copy)]
pub struct Context {
    studio: Studio,
    session: SessionContext,
    adder: Option<AddSourceDialog>,
    props: Option<PropertiesDialog>,
    dock: Option<DockState>,
    filters: Option<FilterDialogs>,
}

impl Context {
    /// The page's contexts, or `None` outside the shell.
    pub fn from_page() -> Option<Context> {
        Some(Context {
            studio: use_context()?,
            session: use_context()?,
            adder: use_context(),
            props: use_context(),
            dock: use_context(),
            filters: use_context(),
        })
    }

    /// Sends `edits` as one batch.
    fn send(self, edits: Vec<Edit>) {
        if edits.is_empty() {
            return;
        }
        if let Some(s) = self.session.get() {
            self.studio.apply(&s, edits);
        }
    }

    /// Selects the item alone and opens its source's Properties dialog: a reference's
    /// is its source's. A nested scene's properties are the scene's, which have no
    /// dialog yet (DL-03), so it shows in the Inspector.
    pub fn properties(self, item_id: ItemId) {
        let source = self.studio.collection.with_untracked(|c| {
            let c = c.as_ref()?;
            let (_, item) = c.item(&item_id)?;
            let source = c.source(&item.source_id)?;
            source_properties::shows(&source.kind).then(|| source.source_id.clone())
        });
        self.studio.select_item(Some(item_id));
        match (source, self.props) {
            (Some(source_id), Some(props)) => props.open(source_id),
            _ => {
                if let Some(dock) = self.dock {
                    dock.0.update(|d| d.show(PanelId::Inspector));
                }
            }
        }
    }

    /// Selects the item alone and shows the Filters panel, which shows its source's
    /// chains.
    pub fn filters(self, item_id: ItemId) {
        self.studio.select_item(Some(item_id));
        if let Some(filters) = self.filters {
            filters.unpick();
        }
        if let Some(dock) = self.dock {
            dock.0.update(|d| d.show(PanelId::Filters));
        }
    }

    /// Opens the Add source dialog on `scene_id`.
    fn add_source(self, scene_id: SceneId) {
        if let Some(adder) = self.adder {
            adder.open(scene_id, false);
        }
    }

    /// Duplicates the primary one of the selected `items` of `scene_id` as a reference or
    /// a clone, as the menu's Duplicate entries do (Ctrl+D, Ctrl+Shift+D). Nothing when
    /// several items are selected, or the primary one is a nested scene or has no
    /// picture, where the menu has no Duplicate entries.
    pub fn duplicate_selected(self, scene_id: &SceneId, items: &[ItemId], clone: bool) {
        let facts = self
            .studio
            .collection
            .with_untracked(|c| c.as_ref().and_then(|c| Facts::of(c, scene_id, items)));
        let Some(facts) = facts else { return };
        let nested = matches!(facts.kind, SourceKind::Scene { .. });
        if facts.items.len() == 1 && facts.picture && !nested {
            self.duplicate(scene_id.clone(), &facts, clone);
        }
    }

    /// Groups the selected `items` of `scene_id`, or makes an empty group on top with
    /// none, and selects the group.
    pub fn group(self, scene_id: &SceneId, items: &[ItemId]) {
        let Some(s) = self.session.get() else { return };
        let ids = GroupIds {
            scene: SceneId::new(self.studio.fresh_id(&s, "scene")),
            source: SourceId::new(self.studio.fresh_id(&s, "source")),
            item: ItemId::new(self.studio.fresh_id(&s, "item")),
        };
        let item = ids.item.clone();
        let edits = self.studio.collection.with_untracked(|c| {
            c.as_ref()
                .map(|c| group_edits(c, scene_id, items, ids))
                .unwrap_or_default()
        });
        if !edits.is_empty() {
            self.studio.apply(&s, edits);
            self.studio.select_item(Some(item));
        }
    }

    /// Ungroups the nested scene `item_id` places.
    fn ungroup(self, item_id: &ItemId) {
        let edits = self
            .studio
            .collection
            .with_untracked(|c| c.as_ref().and_then(|c| ungroup_edits(c, item_id)));
        if let Some(Ok(edits)) = edits {
            self.send(edits);
        }
    }

    /// Removes the selected `items` from their scene (Del).
    pub fn remove(self, items: &[ItemId]) {
        self.send(
            items
                .iter()
                .map(|id| Edit::ItemRemove {
                    item_id: id.clone(),
                })
                .collect(),
        );
    }

    /// Places the primary item's source again, or a copy of it, 40 pixels right and
    /// down and one place above it, and selects the new item.
    fn duplicate(self, scene_id: SceneId, facts: &Facts, clone: bool) {
        let Some(s) = self.session.get() else { return };
        let Some(item_id) = facts.items.first() else {
            return;
        };
        let item_info = self.studio.collection.with_untracked(|c| {
            c.as_ref()
                .and_then(|c| c.item(item_id).map(|(_, i)| (i.geometry, i.crop)))
        });
        let Some((geometry, crop)) = item_info else {
            return;
        };
        let new_item = ItemId::new(self.studio.fresh_id(&s, "item"));
        let mut edits = Vec::new();
        let source_id = if clone {
            let copy = SourceId::new(self.studio.fresh_id(&s, "source"));
            edits.push(Edit::SourceDuplicate {
                source_id: facts.source_id.clone(),
                new_source_id: copy.clone(),
                name: format!("{} copy", facts.name),
            });
            copy
        } else {
            facts.source_id.clone()
        };
        edits.push(Edit::ItemAdd {
            scene_id: scene_id.clone(),
            item_id: new_item.clone(),
            source_id,
            transform: None,
            geometry: geometry
                .and_then(|g| nudge(&g, DUPLICATE_OFFSET, DUPLICATE_OFFSET).or(Some(g))),
            opacity: None,
            blend: None,
        });
        if crop != Crop::NONE {
            edits.push(Edit::ItemSetCrop {
                item_id: new_item.clone(),
                scene_id: Some(scene_id),
                crop,
            });
        }
        // `item.add` puts it on top; move it to just above the original.
        let above = facts
            .order
            .iter()
            .position(|id| id == item_id)
            .map_or(facts.order.len(), |at| at + 1);
        if above < facts.order.len() {
            edits.push(Edit::ItemReorder {
                item_id: new_item.clone(),
                index: u32::try_from(above).unwrap_or(u32::MAX),
            });
        }
        self.studio.apply(&s, edits);
        self.studio.select_item(Some(new_item));
    }
}

/// The reference ↔ clone block for the one selected item (spec-workspace.md 5.2.10): a
/// head, then "Reference to original" and "Independent clone", the current one checked.
fn sharing_entries(cx: Context, facts: &Facts) -> Vec<Entry> {
    let item = facts.items[0].clone();
    let source = facts.source_id.clone();
    // Switching changes the parent's item, which a keyframe doesn't own.
    let keyframe = facts.keyframe;
    let (head, clone) = match &facts.sharing {
        Sharing::Own => return Vec::new(),
        Sharing::Clone { name, .. } => (format!("Duplicate of {name}"), true),
        Sharing::Reference => ("Source".to_owned(), false),
    };
    let reference = {
        let sharing = facts.sharing.clone();
        let (item, source) = (item.clone(), source.clone());
        Entry::item("Reference to original", move || {
            if let Sharing::Clone {
                original, alone, ..
            } = &sharing
            {
                cx.send(to_reference(&item, &source, original, *alone));
            }
        })
        .checked(!clone)
        .disabled_if(keyframe)
    };
    let independent = {
        let name = facts.name.clone();
        Entry::item("Independent clone", move || {
            if clone {
                return;
            }
            let Some(s) = cx.session.get() else { return };
            let copy = SourceId::new(cx.studio.fresh_id(&s, "source"));
            cx.send(to_clone(&item, &source, &format!("{name} copy"), copy));
        })
        .checked(clone)
        .disabled_if(keyframe)
    };
    vec![Entry::Head(head), reference, independent, Entry::Sep]
}

/// The menu for the selected `items` of `scene_id`, the primary one first; empty when
/// none of them is in the scene.
pub fn entries(cx: Context, scene_id: &SceneId, items: &[ItemId], scope: Scope) -> Vec<Entry> {
    let Some(facts) = cx
        .studio
        .collection
        .with_untracked(|c| c.as_ref().and_then(|c| Facts::of(c, scene_id, items)))
    else {
        return Vec::new();
    };
    let facts = Rc::new(facts);
    let many = facts.items.len() > 1;
    let keyframe = facts.keyframe;
    let primary = facts.items[0].clone();

    let properties = {
        let id = primary.clone();
        Entry::item("Properties…", move || cx.properties(id.clone())).hint("Double-click")
    };
    let filters = {
        let id = primary.clone();
        Entry::item("Edit filters…", move || cx.filters(id.clone()))
    };
    let pop_out = Entry::submenu("Pop out", Vec::new());
    let audio = facts.audio.map(|on| {
        let source_id = facts.source_id.clone();
        Entry::item("Audio", move || {
            cx.send(vec![Edit::SourceSetAudio {
                source_id: source_id.clone(),
                enabled: Some(!on),
                follow: None,
                gain_db: None,
                muted: None,
            }]);
        })
        .checked(on)
    });
    let remove = {
        let ids = facts.items.clone();
        Entry::item("Remove from scene", move || cx.remove(&ids))
            .hint("Del")
            .disabled_if(keyframe)
    };

    // A nested scene (5.2.8).
    if let (false, SourceKind::Scene { scene_id: nested }) = (many, &facts.kind) {
        let into = nested.clone();
        let open = nested.clone();
        let studio = cx.studio;
        let ungroup = cx.studio.collection.with_untracked(|c| {
            let c = c.as_ref()?;
            if c.scene(nested)?.role != SceneRole::Group {
                return None;
            }
            let kept = ungroup_edits(c, &primary)?.err();
            let id = primary.clone();
            let entry = Entry::item("Ungroup", move || cx.ungroup(&id));
            Some(match kept {
                Some(kept) => entry.disabled_if(true).hint(kept.reason()),
                None => entry.disabled_if(keyframe),
            })
        });
        let mut list = vec![properties, filters, pop_out, Entry::Sep];
        list.extend(audio);
        list.extend([
            Entry::item("Add source…", move || cx.add_source(into.clone()))
                .disabled_if(cx.adder.is_none()),
            Entry::Sep,
            Entry::item("Open nested scene", move || {
                studio.select_scene(open.clone())
            }),
        ]);
        list.extend(ungroup);
        list.push(remove);
        return list;
    }
    // A source with no picture (5.2.9).
    if !many && !facts.picture {
        let mut list = vec![properties, filters, Entry::Sep];
        if let Some(audio) = audio {
            list.extend([audio, Entry::Sep]);
        }
        list.push(remove);
        return list;
    }

    // One or more visual items (5.2.10, 5.2.11).
    let mut list = Vec::new();
    if many {
        list.extend([
            Entry::Head(format!("{} items selected", facts.items.len())),
            {
                let (scene, ids) = (scene_id.clone(), facts.items.clone());
                Entry::item("Group selection", move || cx.group(&scene, &ids)).disabled_if(keyframe)
            },
            Entry::Sep,
        ]);
    } else {
        if let Some(audio) = audio {
            list.extend([audio, Entry::Sep]);
        }
        list.extend([properties, filters, pop_out, Entry::Sep]);
        list.extend(sharing_entries(cx, &facts));
    }
    let layer = |label: &str, layer: Layer| {
        let facts = Rc::clone(&facts);
        Entry::item(label, move || {
            cx.send(
                restack(&facts.order, &facts.items, layer)
                    .into_iter()
                    .map(|(item_id, index)| Edit::ItemReorder { item_id, index })
                    .collect(),
            );
        })
        .disabled_if(keyframe)
    };
    let scene = scene_id.clone();
    let arrange = |label: &str, how: fn(&Facts) -> Vec<(ItemId, ItemGeometry)>| {
        let none = facts.placed.is_empty();
        let facts = Rc::clone(&facts);
        let scene = scene.clone();
        Entry::item(label, move || {
            cx.send(
                how(&facts)
                    .into_iter()
                    .map(|(item_id, geometry)| Edit::ItemSetGeometry {
                        item_id,
                        scene_id: Some(scene.clone()),
                        geometry,
                    })
                    .collect(),
            );
        })
        .disabled_if(none)
    };
    // Clearing a crop restores the full extent, as the prototype does.
    let clear_crop = {
        let ids = facts.items.clone();
        let scene = scene_id.clone();
        let studio = cx.studio;
        Entry::item("Clear crop", move || {
            let edits = studio.collection.with_untracked(|c| {
                c.as_ref().map(|c| {
                    studio
                        .sizes
                        .with_untracked(|sizes| crate::canvas::crop::clear(c, sizes, &scene, &ids))
                })
            });
            cx.send(edits.unwrap_or_default());
        })
        .disabled_if(!facts.cropped)
    };
    let crop_mode = cx.studio.crop_mode;
    let crop = Entry::item(
        if crop_mode.get_untracked() {
            "Leave crop mode"
        } else {
            "Crop"
        },
        move || crop_mode.update(|on| *on = !*on),
    )
    .hint("C")
    .disabled_if(many);
    let duplicate = |label: &str, clone: bool| {
        let facts = Rc::clone(&facts);
        let scene = scene_id.clone();
        Entry::item(label, move || cx.duplicate(scene.clone(), &facts, clone))
            .disabled_if(many || keyframe)
    };
    list.extend([
        layer("Bring to front", Layer::Front),
        layer("Bring forward", Layer::Forward),
        layer("Send backward", Layer::Backward),
        layer("Send to back", Layer::Back),
        Entry::Sep,
        arrange("Center on canvas", |f| centered(&f.placed, f.canvas)),
        arrange("Fit to canvas", |f| f.each(fitted)),
        arrange("Stretch to canvas", |f| f.each(stretched)),
        arrange("Reset rotation", |f| f.each(|g, _| Some(unrotated(g)))),
        clear_crop,
        Entry::Sep,
        crop,
        duplicate("Duplicate as reference", false).hint("Ctrl+D"),
        duplicate("Duplicate as clone", true).hint("Ctrl+Shift+D"),
        remove,
    ]);
    match scope {
        Scope::Canvas(select_all) => {
            list.push(Entry::item("Select all", move || select_all()).hint("Ctrl+A"));
        }
        Scope::Objects => {
            let scene = scene_id.clone();
            let (into, ids) = (scene_id.clone(), facts.items.clone());
            list.extend([
                Entry::Sep,
                Entry::item("Add source…", move || cx.add_source(scene.clone()))
                    .disabled_if(cx.adder.is_none() || keyframe),
                Entry::item("New group", move || cx.group(&into, &ids)).disabled_if(keyframe),
            ]);
        }
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{Size, Vec2};

    fn ids(names: &[&str]) -> Vec<ItemId> {
        names.iter().map(|n| ItemId::new(*n)).collect()
    }

    /// Applies `moves` to `order` the way the engine applies `item.reorder`.
    fn applied(order: &[ItemId], moves: &[(ItemId, u32)]) -> Vec<ItemId> {
        let mut stack = order.to_vec();
        for (id, to) in moves {
            let from = stack.iter().position(|s| s == id).expect("in the scene");
            let id = stack.remove(from);
            stack.insert(*to as usize, id);
        }
        stack
    }

    #[test]
    fn layer_moves_keep_the_selection_in_order() {
        let order = ids(&["a", "b", "c", "d", "e"]);
        let chosen = ids(&["b", "d"]);
        let cases = [
            (Layer::Front, ["a", "c", "e", "b", "d"]),
            (Layer::Back, ["b", "d", "a", "c", "e"]),
            (Layer::Forward, ["a", "c", "b", "e", "d"]),
            (Layer::Backward, ["b", "a", "d", "c", "e"]),
        ];
        for (layer, want) in cases {
            let moves = restack(&order, &chosen, layer);
            assert_eq!(applied(&order, &moves), ids(&want), "{layer:?}");
        }
    }

    #[test]
    fn a_step_stops_at_the_edge_and_at_a_stuck_neighbour() {
        let order = ids(&["a", "b", "c"]);
        assert!(restack(&order, &ids(&["b", "c"]), Layer::Forward).is_empty());
        assert!(restack(&order, &ids(&["a", "b"]), Layer::Backward).is_empty());
        assert!(restack(&order, &ids(&["c"]), Layer::Front).is_empty());
        assert!(restack(&order, &ids(&["a"]), Layer::Back).is_empty());
    }

    fn geometry(cx: f32, cy: f32, w: f32, h: f32) -> ItemGeometry {
        ItemGeometry::try_new(
            Vec2::new(cx, cy).expect("finite"),
            Size::try_new(w, h).expect("positive"),
        )
        .expect("valid")
    }

    const CANVAS: Canvas = Canvas {
        width: 1920,
        height: 1080,
    };

    #[test]
    fn centring_moves_the_selection_as_one() {
        let placed = vec![
            (ItemId::new("a"), geometry(100.0, 100.0, 100.0, 100.0)),
            (ItemId::new("b"), geometry(350.0, 100.0, 100.0, 100.0)),
        ];
        let moved = centered(&placed, CANVAS);
        // The pair spans 50..400 by 50..150, so it moves by (735, 440).
        assert_eq!(moved[0].1.center.x.get(), 835.0);
        assert_eq!(moved[1].1.center.x.get(), 1085.0);
        assert_eq!(moved[0].1.center.y.get(), 540.0);
    }

    #[test]
    fn fit_keeps_the_shape_and_stretch_fills_the_canvas() {
        let mut square = geometry(10.0, 10.0, 200.0, 200.0);
        square.rotation_deg = Finite32::new(30.0).expect("finite");
        let fit = fitted(&square, CANVAS).expect("fits");
        assert_eq!(
            (fit.size.width.get(), fit.size.height.get()),
            (1080.0, 1080.0)
        );
        assert_eq!((fit.center.x.get(), fit.center.y.get()), (960.0, 540.0));
        assert_eq!(fit.rotation_deg, Finite32::ZERO);
        let full = stretched(&square, CANVAS).expect("fits");
        assert_eq!(
            (full.size.width.get(), full.size.height.get()),
            (1920.0, 1080.0)
        );
    }

    fn placing(id: &str, source: &str) -> ItemInfo {
        ItemInfo {
            item_id: id.into(),
            source_id: source.into(),
            placement: jstream_control::protocol::PlacementForm::Legacy,
            transform: jstream_control::protocol::ItemTransform::IDENTITY,
            geometry: None,
            crop: Crop::NONE,
            visible: true,
            effects: Vec::new(),
            opacity: Finite32::ONE,
            blend: Default::default(),
            transition: Default::default(),
        }
    }

    fn source(id: &str, from: Option<&str>) -> jstream_control::protocol::SourceInfo {
        jstream_control::protocol::SourceInfo {
            source_id: id.into(),
            name: id.to_uppercase(),
            kind: SourceKind::Builtin {
                kind: "image".into(),
            },
            settings: Default::default(),
            duplicated_from: from.map(SourceId::new),
            audio: None,
            filters: Default::default(),
            meta: Default::default(),
        }
    }

    fn studio(items: Vec<ItemInfo>) -> Collection {
        Collection {
            sources: vec![
                source("logo", None),
                source("copy", Some("logo")),
                source("orphan", Some("deleted")),
            ],
            scenes: vec![jstream_control::protocol::SceneInfo {
                scene_id: "main".into(),
                name: "Main".into(),
                role: Default::default(),
                transition_in: None,
                transition_out: None,
                folder: None,
                meta: Default::default(),
                variant_of: None,
                overrides: Vec::new(),
                items,
                filters: Default::default(),
            }],
            ..Collection::default()
        }
    }

    #[test]
    fn sharing_tells_a_reference_from_a_clone() {
        let c = studio(vec![
            placing("a", "logo"),
            placing("b", "logo"),
            placing("c", "copy"),
            placing("d", "orphan"),
        ]);
        let item = |id: &str| c.item(&ItemId::new(id)).expect("placed").1.clone();
        assert_eq!(Sharing::of(&c, &item("a")), Sharing::Reference);
        assert_eq!(
            Sharing::of(&c, &item("c")),
            Sharing::Clone {
                original: SourceId::new("logo"),
                name: "LOGO".into(),
                alone: true,
            }
        );
        // A copy whose original is gone is a source of its own.
        assert_eq!(Sharing::of(&c, &item("d")), Sharing::Own);
        let alone = studio(vec![placing("a", "logo")]);
        let a = alone.item(&ItemId::new("a")).expect("placed").1.clone();
        assert_eq!(Sharing::of(&alone, &a), Sharing::Own);
        let twice = studio(vec![placing("c", "copy"), placing("e", "copy")]);
        let e = twice.item(&ItemId::new("e")).expect("placed").1.clone();
        assert!(matches!(
            Sharing::of(&twice, &e),
            Sharing::Clone { alone: false, .. }
        ));
    }

    /// A collection with scene `main` holding `items` (bottom first) and a group scene
    /// `grp` holding `kids`, shown by the source `grp_src`.
    fn grouped(items: Vec<ItemInfo>, kids: Vec<ItemInfo>) -> Collection {
        let mut c = studio(items);
        let mut group = c.scenes[0].clone();
        group.scene_id = "grp".into();
        group.name = "Group 1".into();
        group.role = SceneRole::Group;
        group.items = kids;
        c.scenes.push(group);
        let mut src = source("grp_src", None);
        src.kind = SourceKind::Scene {
            scene_id: "grp".into(),
        };
        c.sources.push(src);
        c
    }

    fn ids_of(edits: &[Edit]) -> Vec<String> {
        edits
            .iter()
            .map(|e| match e {
                Edit::SceneCreate { scene_id, name, .. } => format!("scene {scene_id} {name}"),
                Edit::SourceCreate { source_id, .. } => format!("source {source_id}"),
                Edit::ItemMove {
                    item_id,
                    scene_id,
                    index,
                } => format!("move {item_id} {scene_id} {index}"),
                Edit::ItemAdd { item_id, .. } => format!("add {item_id}"),
                Edit::ItemReorder { item_id, index } => format!("reorder {item_id} {index}"),
                Edit::ItemRemove { item_id } => format!("remove {item_id}"),
                Edit::SourceDelete { source_id } => format!("delete source {source_id}"),
                Edit::SceneDelete { scene_id } => format!("delete scene {scene_id}"),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn grouping_moves_the_items_in_order_and_puts_the_group_where_the_top_one_was() {
        let c = grouped(
            vec![
                placing("a", "logo"),
                placing("b", "logo"),
                placing("c", "logo"),
                placing("d", "logo"),
            ],
            Vec::new(),
        );
        let ids = GroupIds {
            scene: "g".into(),
            source: "gs".into(),
            item: "gi".into(),
        };
        let edits = group_edits(&c, &SceneId::new("main"), &ids_list(&["c", "a"]), ids);
        assert_eq!(
            ids_of(&edits),
            [
                "scene g Group 2",
                "source gs",
                "move a g 0",
                "move c g 1",
                "add gi",
                // b stays below where c was; d above.
                "reorder gi 1",
            ]
        );
        let on_top = group_edits(
            &c,
            &SceneId::new("main"),
            &ids_list(&["d"]),
            GroupIds {
                scene: "g".into(),
                source: "gs".into(),
                item: "gi".into(),
            },
        );
        assert_eq!(ids_of(&on_top).last().map(String::as_str), Some("add gi"));
    }

    fn ids_list(names: &[&str]) -> Vec<ItemId> {
        names.iter().map(|n| ItemId::new(*n)).collect()
    }

    #[test]
    fn ungrouping_puts_the_children_back_and_drops_an_unplaced_group() {
        let c = grouped(
            vec![
                placing("a", "logo"),
                placing("g1", "grp_src"),
                placing("b", "logo"),
            ],
            vec![placing("x", "logo"), placing("y", "copy")],
        );
        let edits = ungroup_edits(&c, &ItemId::new("g1"))
            .expect("a nested scene")
            .expect("at its default placement");
        assert_eq!(
            ids_of(&edits),
            [
                "move x main 1",
                "move y main 2",
                "remove g1",
                "delete source grp_src",
                "delete scene grp",
            ]
        );
        // Placed elsewhere too, ungrouping one placement would empty the other (N1).
        let twice = grouped(
            vec![placing("g1", "grp_src"), placing("g2", "grp_src")],
            vec![placing("x", "logo")],
        );
        assert_eq!(
            ungroup_edits(&twice, &ItemId::new("g1")),
            Some(Err(Kept::PlacedElsewhere))
        );
        assert!(ungroup_edits(&c, &ItemId::new("a")).is_none());
    }

    fn boxed(id: &str, source: &str, x: f32, y: f32, w: f32, h: f32, turn: f32) -> ItemInfo {
        let mut item = placing(id, source);
        item.placement = jstream_control::protocol::PlacementForm::Geometry;
        item.geometry = Some(ItemGeometry {
            rotation_deg: Finite32::new(turn).expect("finite"),
            ..ItemGeometry::try_new(
                Vec2::new(x, y).expect("finite"),
                Size::try_new(w, h).expect("positive"),
            )
            .expect("a box")
        });
        item
    }

    fn boxes(edits: &[Edit]) -> Vec<(String, [f32; 5])> {
        edits
            .iter()
            .filter_map(|e| match e {
                Edit::ItemSetGeometry {
                    item_id, geometry, ..
                } => Some((
                    item_id.to_string(),
                    [
                        geometry.center.x.get(),
                        geometry.center.y.get(),
                        geometry.size.width.get(),
                        geometry.size.height.get(),
                        geometry.rotation_deg.get(),
                    ],
                )),
                _ => None,
            })
            .collect()
    }

    /// B1 on #871: a moved, scaled or turned group's items keep where they draw.
    #[test]
    fn ungrouping_a_moved_group_carries_its_placement_onto_its_items() {
        let kids = || {
            vec![
                boxed("x", "logo", 960.0, 540.0, 200.0, 100.0, 0.0),
                boxed("y", "copy", 1060.0, 640.0, 100.0, 100.0, 30.0),
            ]
        };
        let at = |group: ItemInfo| {
            let c = grouped(vec![group], kids());
            ungroup_edits(&c, &ItemId::new("g1")).expect("a nested scene")
        };
        // Half size in the top-left quarter.
        let half = at(boxed("g1", "grp_src", 480.0, 270.0, 960.0, 540.0, 0.0)).expect("carried");
        assert_eq!(
            ids_of(&half)[..2],
            ["move x main 0".to_owned(), "move y main 1".to_owned()]
        );
        assert_eq!(
            boxes(&half),
            [
                ("x".to_owned(), [480.0, 270.0, 100.0, 50.0, 0.0]),
                ("y".to_owned(), [530.0, 320.0, 50.0, 50.0, 30.0]),
            ]
        );
        // Turned a quarter clockwise about the canvas centre.
        let turned =
            at(boxed("g1", "grp_src", 960.0, 540.0, 1920.0, 1080.0, 90.0)).expect("carried");
        let b = boxes(&turned);
        assert_eq!(b[0].1, [960.0, 540.0, 200.0, 100.0, 90.0]);
        let y = b[1].1;
        assert!(
            (y[0] - 860.0).abs() < 1e-3 && (y[1] - 640.0).abs() < 1e-3,
            "{y:?}"
        );
        assert_eq!(y[4], 120.0);
        // At its default placement nothing is rewritten.
        let plain = at(boxed("g1", "grp_src", 960.0, 540.0, 1920.0, 1080.0, 0.0)).expect("plain");
        assert!(boxes(&plain).is_empty());
        // A faded group, or a stretch of a turned item, can't be carried.
        let mut faded = boxed("g1", "grp_src", 480.0, 270.0, 960.0, 540.0, 0.0);
        faded.opacity = Finite32::new(0.5).expect("finite");
        assert_eq!(at(faded), Err(Kept::Look));
        let mut wide = boxed("g1", "grp_src", 960.0, 540.0, 1920.0, 540.0, 0.0);
        if let Some(g) = &mut wide.geometry {
            g.fit_mode = FitMode::Stretch;
        }
        assert_eq!(at(wide), Err(Kept::Look));
    }

    /// N10 on #871: the Fit scale of a box of another shape, the axis swap for a
    /// quarter-turned item under a stretch, and each refusal.
    #[test]
    fn ungrouping_scales_by_the_fitted_canvas_and_refuses_what_would_change() {
        let ungroup = |group: ItemInfo, kids: Vec<ItemInfo>| {
            let c = grouped(vec![group], kids);
            ungroup_edits(&c, &ItemId::new("g1")).expect("a nested scene")
        };
        let child = || boxed("x", "logo", 1060.0, 540.0, 200.0, 100.0, 0.0);
        // A wide Fit box shows the group's canvas at the smaller scale, centred.
        let fit = ungroup(
            boxed("g1", "grp_src", 960.0, 540.0, 1920.0, 540.0, 0.0),
            vec![child()],
        )
        .expect("carried");
        assert_eq!(boxes(&fit)[0].1, [1010.0, 540.0, 100.0, 50.0, 0.0]);
        // A stretch swaps its axes on a quarter-turned stretched item.
        let mut turned = boxed("x", "logo", 960.0, 540.0, 200.0, 100.0, 90.0);
        let mut stretch = boxed("g1", "grp_src", 960.0, 540.0, 1920.0, 540.0, 0.0);
        for g in [&mut turned.geometry, &mut stretch.geometry]
            .into_iter()
            .flatten()
        {
            g.fit_mode = FitMode::Stretch;
        }
        let swapped = ungroup(stretch.clone(), vec![turned]).expect("carried");
        assert_eq!(boxes(&swapped)[0].1, [960.0, 540.0, 100.0, 100.0, 90.0]);
        // B3: a stretch of a fitted item would change its content's shape.
        assert_eq!(ungroup(stretch, vec![child()]), Err(Kept::Look));
        // A moved group refuses for the group item's crop, blend, filters and mirror,
        // and for an item turned in 3D or partly off the group's canvas (N9).
        let half = || boxed("g1", "grp_src", 480.0, 270.0, 960.0, 540.0, 0.0);
        let mut cropped = half();
        cropped.crop.left = 10;
        let mut blended = half();
        blended.blend = jstream_control::protocol::BlendMode::Multiply;
        let mut filtered = half();
        filtered.effects.push(effect());
        let mut mirrored = half();
        if let Some(g) = &mut mirrored.geometry {
            g.flip_h = true;
        }
        for group in [cropped, blended, filtered, mirrored] {
            assert_eq!(ungroup(group, vec![child()]), Err(Kept::Look));
        }
        let mut tilted = child();
        if let Some(g) = &mut tilted.geometry {
            g.tilt_deg = Finite32::new(20.0).expect("finite");
        }
        assert_eq!(ungroup(half(), vec![tilted]), Err(Kept::Look));
        let off = boxed("x", "logo", 1900.0, 540.0, 200.0, 100.0, 0.0);
        assert_eq!(ungroup(half(), vec![off.clone()]), Err(Kept::Look));
        // At its default placement nothing is clipped, so the same item moves out as is.
        let plain = boxed("g1", "grp_src", 960.0, 540.0, 1920.0, 1080.0, 0.0);
        assert!(ungroup(plain, vec![off]).is_ok());
    }

    /// B3 on #871: a hidden group hides its items, and its scene's filters keep it.
    #[test]
    fn ungrouping_keeps_a_hidden_group_hidden_and_a_filtered_one_whole() {
        let mut hidden = placing("g1", "grp_src");
        hidden.visible = false;
        let mut off = placing("y", "copy");
        off.visible = false;
        let c = grouped(vec![hidden], vec![placing("x", "logo"), off]);
        let edits = ungroup_edits(&c, &ItemId::new("g1"))
            .expect("a nested scene")
            .expect("hidden is fine");
        let hides: Vec<&ItemId> = edits
            .iter()
            .filter_map(|e| match e {
                Edit::ItemSetVisible {
                    item_id,
                    visible: false,
                    ..
                } => Some(item_id),
                _ => None,
            })
            .collect();
        assert_eq!(hides, [&ItemId::new("x")]);
        let mut c = grouped(vec![placing("g1", "grp_src")], vec![placing("x", "logo")]);
        if let Some(group) = c.scenes.iter_mut().find(|s| s.scene_id.as_str() == "grp") {
            group.filters.video.push(effect());
        }
        assert_eq!(ungroup_edits(&c, &ItemId::new("g1")), Some(Err(Kept::Look)));
    }

    fn effect() -> jstream_control::protocol::FilterInfo {
        jstream_control::protocol::FilterInfo {
            filter_id: "f1".into(),
            kind: "blur".into(),
            enabled: true,
            settings: Settings::new(),
        }
    }

    #[test]
    fn switching_keeps_the_item_and_drops_an_unused_copy() {
        let (item, logo, copy) = (ItemId::new("a"), SourceId::new("logo"), SourceId::new("c2"));
        assert_eq!(
            to_clone(&item, &logo, "Logo", copy.clone()),
            vec![
                Edit::SourceDuplicate {
                    source_id: logo.clone(),
                    new_source_id: copy.clone(),
                    name: "Logo".into(),
                },
                Edit::ItemSetSource {
                    item_id: item.clone(),
                    source_id: copy.clone(),
                },
            ]
        );
        let back = to_reference(&item, &copy, &logo, true);
        assert_eq!(
            back,
            vec![
                Edit::ItemSetSource {
                    item_id: item.clone(),
                    source_id: logo.clone(),
                },
                Edit::SourceDelete {
                    source_id: copy.clone(),
                },
            ]
        );
        assert_eq!(to_reference(&item, &copy, &logo, false).len(), 1);
    }

    #[test]
    fn reset_rotation_clears_tilt_and_turn_too() {
        let mut g = geometry(10.0, 10.0, 20.0, 20.0);
        g.rotation_deg = Finite32::new(45.0).expect("finite");
        g.tilt_deg = Finite32::new(10.0).expect("finite");
        g.turn_deg = Finite32::new(-5.0).expect("finite");
        let flat = unrotated(&g);
        assert!(flat.is_neutral_3d());
        assert_eq!(flat.rotation_deg, Finite32::ZERO);
        assert_eq!(flat.center, g.center);
    }
}
