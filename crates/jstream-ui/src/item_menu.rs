//! The right-click menu of the selected items, shared by the canvas and the Scene
//! objects panel (spec-workspace.md 5.2.8 to 5.2.12): Properties, the Audio check,
//! layer order, Center, Fit and Stretch to canvas, Reset rotation, Clear crop, Crop
//! (crop mode), the two duplicates and Remove from scene. Each choice sends one batch, so one undo step.
//!
//! Docs: docs/UI-FEATURES.md#menus

use crate::app::SessionContext;
use crate::canvas::snap::Bounds;
use crate::canvas::transform::{nudge, with_box};
use crate::dock::PanelId;
use crate::menu::Entry;
use crate::panels::add_source::{AddSourceDialog, has_picture};
use crate::panels::source_properties::{self, PropertiesDialog};
use crate::store::{Collection, Studio};
use crate::workspace::DockState;
use jstream_control::protocol::{
    Canvas, Crop, Edit, Finite32, ItemGeometry, ItemId, SceneId, SourceId, SourceKind,
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
        Entry::item("Remove from scene", move || {
            cx.send(
                ids.iter()
                    .map(|id| Edit::ItemRemove {
                        item_id: id.clone(),
                    })
                    .collect(),
            );
        })
        .hint("Del")
    };

    // A nested scene (5.2.8).
    if let (false, SourceKind::Scene { scene_id: nested }) = (many, &facts.kind) {
        let into = nested.clone();
        let open = nested.clone();
        let studio = cx.studio;
        let mut list = vec![properties, filters, pop_out, Entry::Sep];
        list.extend(audio);
        list.extend([
            Entry::item("Add source…", move || cx.add_source(into.clone()))
                .disabled_if(cx.adder.is_none()),
            Entry::Sep,
            Entry::item("Open nested scene", move || {
                studio.select_scene(open.clone())
            }),
            remove,
        ]);
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
            Entry::unavailable("Group selection"),
            Entry::Sep,
        ]);
    } else {
        if let Some(audio) = audio {
            list.extend([audio, Entry::Sep]);
        }
        list.extend([properties, filters, pop_out, Entry::Sep]);
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
        Entry::item(label, move || cx.duplicate(scene.clone(), &facts, clone)).disabled_if(many)
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
            list.extend([
                Entry::Sep,
                Entry::item("Add source…", move || cx.add_source(scene.clone()))
                    .disabled_if(cx.adder.is_none()),
                Entry::unavailable("New group"),
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
