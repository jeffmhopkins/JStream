//! Copy and paste (KB-05, decision register D43). The clipboard is the page's own, not
//! the system's: Ctrl+C on the canvas or a Scene objects row copies the selected items,
//! and on a scene row copies the scene. Ctrl+V, or Paste in the empty canvas's and the
//! Scene objects panel's menus, places the copied items' sources again in the shown
//! scene as references, each with the copied item's box, crop, visibility, opacity,
//! blend, transition settings and effects. Into the scene they came from they land 40
//! pixels right and down, as a duplicate does. Ctrl+V in the Scenes panel, or Paste
//! scene, duplicates the copied scene, as Duplicate scene does: a scene named
//! "\<name\> copy" just below it, in its folder, with its labels, transitions, filters
//! and items. Its keyframes and hotkey stay with the original, since a chord runs one
//! action. Each paste is one batch, so one undo step.
//!
//! Docs: docs/UI-FEATURES.md#keyboard

use crate::app::SessionContext;
use crate::store::{Collection, Studio};
use jstream_control::protocol::{
    Crop, Edit, FilterChain, FilterId, FilterInfo, FilterOwner, Finite32, ItemId, ItemInfo,
    SceneId, SourceKind,
};
use leptos::prelude::*;

/// How far items pasted into the scene they were copied from sit from their originals,
/// right and down, in canvas pixels: the duplicate's offset.
const PASTE_OFFSET: f32 = 40.0;

/// What Ctrl+C took.
#[derive(Clone, Debug, PartialEq)]
pub enum Clip {
    /// Items, bottom first, as they were when copied, and the scene they came from.
    Items {
        /// The scene that held them.
        from: SceneId,
        /// The items.
        items: Vec<ItemInfo>,
    },
    /// A scene, by ID: Paste scene duplicates it as it is then.
    Scene(SceneId),
}

/// The page's clipboard, shared by the canvas and the panels.
#[derive(Clone, Copy)]
pub struct Clipboard(pub RwSignal<Option<Clip>>);

impl Clipboard {
    /// An empty clipboard.
    pub fn new() -> Clipboard {
        Clipboard(RwSignal::new(None))
    }

    /// Copies the `items` of `scene_id` that are still in it, in their stacking order.
    /// Nothing is copied, and the clipboard keeps what it had, when none of them is.
    pub fn copy_items(&self, collection: &Collection, scene_id: &SceneId, items: &[ItemId]) {
        if let Some(clip) = items_clip(collection, scene_id, items) {
            self.0.set(Some(clip));
        }
    }

    /// Copies a scene, unless it is a keyframe scene, which has no items of its own.
    pub fn copy_scene(&self, collection: &Collection, scene_id: &SceneId) {
        if collection.scene(scene_id).is_some() && !collection.is_keyframe(scene_id) {
            self.0.set(Some(Clip::Scene(scene_id.clone())));
        }
    }

    /// Whether Paste has items to place.
    pub fn has_items(&self) -> bool {
        self.0.with(|c| matches!(c, Some(Clip::Items { .. })))
    }

    /// The scene Paste scene would duplicate.
    pub fn scene(&self) -> Option<SceneId> {
        self.0.with(|c| match c {
            Some(Clip::Scene(id)) => Some(id.clone()),
            _ => None,
        })
    }
}

/// Copy and paste as the page does them, through its contexts.
#[derive(Clone, Copy)]
pub struct Pasting {
    studio: Studio,
    session: SessionContext,
    /// The clipboard, for the menus' Paste entries.
    pub clipboard: Clipboard,
}

impl Pasting {
    /// The page's contexts, or `None` outside the shell.
    pub fn from_page() -> Option<Pasting> {
        Some(Pasting {
            studio: use_context()?,
            session: use_context()?,
            clipboard: use_context()?,
        })
    }

    /// Copies the `items` of `scene_id` (Ctrl+C on the canvas or an object row).
    pub fn copy_items(self, scene_id: &SceneId, items: &[ItemId]) {
        self.studio.collection.with_untracked(|c| {
            if let Some(c) = c {
                self.clipboard.copy_items(c, scene_id, items);
            }
        });
    }

    /// Copies a scene (Ctrl+C on a scene row).
    pub fn copy_scene(self, scene_id: &SceneId) {
        self.studio.collection.with_untracked(|c| {
            if let Some(c) = c {
                self.clipboard.copy_scene(c, scene_id);
            }
        });
    }

    /// Pastes the copied items into `target` and selects them (Ctrl+V, Paste).
    pub fn paste(self, target: &SceneId) {
        let Some(s) = self.session.get() else { return };
        let Some(clip) = self.clipboard.0.get_untracked() else {
            return;
        };
        let studio = self.studio;
        let (edits, placed) = studio.collection.with_untracked(|c| {
            c.as_ref().map_or_else(Default::default, |c| {
                paste_edits(c, &clip, target, &mut |prefix| studio.fresh_id(&s, prefix))
            })
        });
        if edits.is_empty() {
            return;
        }
        let note = match placed.len() {
            1 => crate::panels::add_source::REFERENCE_ADDED.to_owned(),
            n => format!(
                "Added {n} references. Only their position, size, rotation and crop are their own."
            ),
        };
        studio.apply_noting(&s, edits, note);
        studio.select_items(placed);
    }

    /// Duplicates `scene_id` (Duplicate scene).
    pub fn duplicate_scene(self, scene_id: &SceneId) {
        let Some(s) = self.session.get() else { return };
        let studio = self.studio;
        let new_id = SceneId::new(studio.fresh_id(&s, "scene"));
        let edits = studio.collection.with_untracked(|c| {
            c.as_ref().map_or_else(Vec::new, |c| {
                duplicate_scene_edits(c, scene_id, new_id, &mut |prefix| {
                    studio.fresh_id(&s, prefix)
                })
            })
        });
        if !edits.is_empty() {
            studio.apply(&s, edits);
        }
    }

    /// Duplicates the copied scene (Ctrl+V in the Scenes panel, Paste scene).
    pub fn paste_scene(self) {
        if let Some(scene_id) = self.clipboard.scene() {
            self.duplicate_scene(&scene_id);
        }
    }
}

impl Default for Clipboard {
    fn default() -> Clipboard {
        Clipboard::new()
    }
}

/// The clip of the `items` of `scene_id`, bottom first, or `None` if none is in it.
pub fn items_clip(collection: &Collection, scene_id: &SceneId, items: &[ItemId]) -> Option<Clip> {
    let scene = collection.scene(scene_id)?;
    let copied: Vec<ItemInfo> = scene
        .items
        .iter()
        .filter(|i| items.contains(&i.item_id))
        .cloned()
        .collect();
    (!copied.is_empty()).then(|| Clip::Items {
        from: scene_id.clone(),
        items: copied,
    })
}

/// Whether scene `inner` shows scene `outer`, itself or through any depth of nesting,
/// so placing `inner` in `outer` would make `outer` contain itself.
fn shows(collection: &Collection, inner: &SceneId, outer: &SceneId) -> bool {
    let mut seen: Vec<SceneId> = Vec::new();
    let mut todo = vec![inner.clone()];
    while let Some(id) = todo.pop() {
        if id == *outer {
            return true;
        }
        if seen.contains(&id) {
            continue;
        }
        let Some(scene) = collection.scene(&id) else {
            continue;
        };
        if let Some(parent) = &scene.variant_of {
            todo.push(parent.clone());
        }
        for item in &scene.items {
            if let Some(SourceKind::Scene { scene_id }) =
                collection.source(&item.source_id).map(|s| &s.kind)
            {
                todo.push(scene_id.clone());
            }
        }
        seen.push(id);
    }
    false
}

/// The edits that place `items` in `target`, bottom first, each on top of what is
/// there, with new IDs from `fresh` (called with the prefix `item` or `filter`). Items
/// whose source is gone, or whose nested scene shows `target`, are left out. Each new
/// item's ID is in `placed`, in order. With `offset`, every box moves right and down by
/// the duplicate's offset.
fn place_edits(
    collection: &Collection,
    items: &[ItemInfo],
    target: &SceneId,
    offset: bool,
    fresh: &mut dyn FnMut(&str) -> String,
    placed: &mut Vec<ItemId>,
) -> Vec<Edit> {
    let mut edits = Vec::new();
    for item in items {
        let Some(source) = collection.source(&item.source_id) else {
            continue;
        };
        if let SourceKind::Scene { scene_id } = &source.kind
            && shows(collection, scene_id, target)
        {
            continue;
        }
        let item_id = ItemId::new(fresh("item"));
        let geometry = item.geometry.map(|g| {
            if offset {
                crate::canvas::transform::nudge(&g, PASTE_OFFSET, PASTE_OFFSET).unwrap_or(g)
            } else {
                g
            }
        });
        edits.push(Edit::ItemAdd {
            scene_id: target.clone(),
            item_id: item_id.clone(),
            source_id: item.source_id.clone(),
            // A legacy item's box follows its source's size; its transform moves it.
            transform: item.geometry.is_none().then_some(item.transform),
            geometry,
            opacity: (item.opacity != Finite32::ONE).then_some(item.opacity),
            blend: (!item.blend.is_normal()).then_some(item.blend),
        });
        if item.crop != Crop::NONE {
            edits.push(Edit::ItemSetCrop {
                item_id: item_id.clone(),
                scene_id: None,
                crop: item.crop,
            });
        }
        if !item.visible {
            edits.push(Edit::ItemSetVisible {
                item_id: item_id.clone(),
                scene_id: None,
                visible: false,
            });
        }
        if !item.transition.is_default() {
            edits.push(Edit::ItemSetTransition {
                item_id: item_id.clone(),
                transition: item.transition,
            });
        }
        let owner = FilterOwner::Item {
            item_id: item_id.clone(),
        };
        edits.extend(filter_edits(
            &owner,
            FilterChain::Video,
            &item.effects,
            fresh,
        ));
        placed.push(item_id);
    }
    edits
}

/// `filter.add` for each of `filters`, in order, on `owner`'s `chain`, with new IDs.
fn filter_edits(
    owner: &FilterOwner,
    chain: FilterChain,
    filters: &[FilterInfo],
    fresh: &mut dyn FnMut(&str) -> String,
) -> Vec<Edit> {
    filters
        .iter()
        .map(|f| Edit::FilterAdd {
            owner: owner.clone(),
            chain,
            filter_id: FilterId::new(fresh("filter")),
            kind: f.kind.clone(),
            settings: f.settings.clone(),
            enabled: (!f.enabled).then_some(false),
            index: None,
        })
        .collect()
}

/// The batch that pastes copied `items` into `target`, and the new items' IDs. They move
/// by the duplicate's offset when `target` is the scene they were copied from. Empty
/// when `target` is a keyframe scene, whose items are its parent's, or nothing can be
/// placed.
pub fn paste_edits(
    collection: &Collection,
    clip: &Clip,
    target: &SceneId,
    fresh: &mut dyn FnMut(&str) -> String,
) -> (Vec<Edit>, Vec<ItemId>) {
    let Clip::Items { from, items } = clip else {
        return (Vec::new(), Vec::new());
    };
    if collection.scene(target).is_none() || collection.is_keyframe(target) {
        return (Vec::new(), Vec::new());
    }
    let mut placed = Vec::new();
    let edits = place_edits(
        collection,
        items,
        target,
        from == target,
        fresh,
        &mut placed,
    );
    (edits, placed)
}

/// The batch that duplicates `scene_id` (Duplicate scene, Paste scene) as `new_id`, and
/// the copy's name. Empty for a missing scene or a keyframe scene.
pub fn duplicate_scene_edits(
    collection: &Collection,
    scene_id: &SceneId,
    new_id: SceneId,
    fresh: &mut dyn FnMut(&str) -> String,
) -> Vec<Edit> {
    let Some(scene) = collection.scene(scene_id) else {
        return Vec::new();
    };
    if scene.variant_of.is_some() {
        return Vec::new();
    }
    let mut edits = vec![Edit::SceneCreate {
        scene_id: new_id.clone(),
        name: format!("{} copy", scene.name),
        role: (scene.role != Default::default()).then_some(scene.role),
    }];
    // `scene.create` puts it last; move it to just below the original.
    let at = collection
        .scenes
        .iter()
        .position(|s| s.scene_id == *scene_id);
    if let Some(at) = at.filter(|at| at + 1 < collection.scenes.len()) {
        edits.push(Edit::SceneReorder {
            scene_id: new_id.clone(),
            index: u32::try_from(at + 1).unwrap_or(u32::MAX),
        });
    }
    if scene.folder.is_some() {
        edits.push(Edit::SceneSetFolder {
            scene_id: new_id.clone(),
            folder_id: scene.folder.clone(),
        });
    }
    if !scene.meta.is_default() {
        edits.push(Edit::SceneSetMeta {
            scene_id: new_id.clone(),
            meta: scene.meta.clone(),
        });
    }
    if !scene.options.is_default() {
        edits.push(Edit::SceneSetOptions {
            scene_id: new_id.clone(),
            options: scene.options.clone(),
        });
    }
    if scene.transition_in.is_some() || scene.transition_out.is_some() {
        edits.push(Edit::SceneSetTransitions {
            scene_id: new_id.clone(),
            transition_in: scene.transition_in.clone(),
            transition_out: scene.transition_out.clone(),
        });
    }
    let owner = FilterOwner::Scene {
        scene_id: new_id.clone(),
    };
    edits.extend(filter_edits(
        &owner,
        FilterChain::Video,
        &scene.filters.video,
        fresh,
    ));
    let mut placed = Vec::new();
    edits.extend(place_edits(
        collection,
        &scene.items,
        &new_id,
        false,
        fresh,
        &mut placed,
    ));
    edits
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{
        FilterChains, FolderId, ItemGeometry, ItemTransform, PlacementForm, SceneInfo, Settings,
        Size, SourceInfo, TransitionPresetId, Vec2,
    };

    fn item(id: &str, source: &str, x: f32) -> ItemInfo {
        ItemInfo {
            item_id: id.into(),
            source_id: source.into(),
            placement: PlacementForm::Geometry,
            transform: ItemTransform::IDENTITY,
            geometry: Some(
                ItemGeometry::try_new(
                    Vec2::new(x, 100.0).expect("finite"),
                    Size::try_new(200.0, 100.0).expect("positive"),
                )
                .expect("a box"),
            ),
            crop: Crop::NONE,
            visible: true,
            effects: Vec::new(),
            opacity: Finite32::ONE,
            blend: Default::default(),
            transition: Default::default(),
        }
    }

    fn source(id: &str, kind: SourceKind) -> SourceInfo {
        SourceInfo {
            source_id: id.into(),
            name: id.to_uppercase(),
            kind,
            settings: Default::default(),
            duplicated_from: None,
            audio: None,
            filters: Default::default(),
            meta: Default::default(),
        }
    }

    fn scene(id: &str, items: Vec<ItemInfo>) -> SceneInfo {
        SceneInfo {
            scene_id: id.into(),
            name: id.to_uppercase(),
            role: Default::default(),
            transition_in: None,
            transition_out: None,
            folder: None,
            meta: Default::default(),
            options: Default::default(),
            variant_of: None,
            overrides: Vec::new(),
            items,
            filters: Default::default(),
        }
    }

    fn blur() -> FilterInfo {
        FilterInfo {
            filter_id: "f1".into(),
            kind: "blur".into(),
            enabled: false,
            settings: Settings::new(),
        }
    }

    /// `main` holds an image `a` and the nested scene `inner` (`b`); `inner` holds the
    /// image `c`; `other` is empty; `kf` is a keyframe of `main`.
    fn collection() -> Collection {
        let image = || SourceKind::Builtin {
            kind: "image".into(),
        };
        let mut a = item("a", "logo", 300.0);
        a.crop = Crop {
            left: 10,
            ..Crop::NONE
        };
        a.visible = false;
        a.opacity = Finite32::new(0.5).expect("finite");
        a.effects = vec![blur()];
        let mut kf = scene("kf", Vec::new());
        kf.variant_of = Some("main".into());
        Collection {
            sources: vec![
                source("logo", image()),
                source(
                    "inner_src",
                    SourceKind::Scene {
                        scene_id: "inner".into(),
                    },
                ),
            ],
            scenes: vec![
                scene("main", vec![a, item("b", "inner_src", 900.0)]),
                scene("inner", vec![item("c", "logo", 500.0)]),
                scene("other", Vec::new()),
                kf,
            ],
            ..Collection::default()
        }
    }

    fn counter() -> impl FnMut(&str) -> String {
        let mut n = 0;
        move |prefix: &str| {
            n += 1;
            format!("{prefix}{n}")
        }
    }

    fn x_of(edit: &Edit) -> Option<f32> {
        match edit {
            Edit::ItemAdd { geometry, .. } => geometry.map(|g| g.center.x.get()),
            _ => None,
        }
    }

    #[test]
    fn copying_takes_the_selected_items_bottom_first() {
        let c = collection();
        let clip = items_clip(&c, &"main".into(), &["b".into(), "a".into(), "gone".into()]);
        let Some(Clip::Items { from, items }) = clip else {
            panic!("items")
        };
        assert_eq!(from, SceneId::new("main"));
        let ids: Vec<&str> = items.iter().map(|i| i.item_id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(items_clip(&c, &"main".into(), &["gone".into()]), None);
    }

    #[test]
    fn pasting_places_references_with_every_placement_setting() {
        let c = collection();
        let clip = items_clip(&c, &"main".into(), &["a".into()]).expect("copied");
        let (edits, placed) = paste_edits(&c, &clip, &"other".into(), &mut counter());
        assert_eq!(placed, [ItemId::new("item1")]);
        assert_eq!(
            edits,
            vec![
                Edit::ItemAdd {
                    scene_id: "other".into(),
                    item_id: "item1".into(),
                    source_id: "logo".into(),
                    transform: None,
                    geometry: c.scenes[0].items[0].geometry,
                    opacity: Some(Finite32::new(0.5).expect("finite")),
                    blend: None,
                },
                Edit::ItemSetCrop {
                    item_id: "item1".into(),
                    scene_id: None,
                    crop: c.scenes[0].items[0].crop,
                },
                Edit::ItemSetVisible {
                    item_id: "item1".into(),
                    scene_id: None,
                    visible: false,
                },
                Edit::FilterAdd {
                    owner: FilterOwner::Item {
                        item_id: "item1".into(),
                    },
                    chain: FilterChain::Video,
                    filter_id: "filter2".into(),
                    kind: "blur".into(),
                    settings: Settings::new(),
                    enabled: Some(false),
                    index: None,
                },
            ]
        );
    }

    #[test]
    fn pasting_into_the_same_scene_offsets_and_a_keyframe_takes_nothing() {
        let c = collection();
        let clip = items_clip(&c, &"main".into(), &["a".into(), "b".into()]).expect("copied");
        let (edits, placed) = paste_edits(&c, &clip, &"main".into(), &mut counter());
        assert_eq!(placed.len(), 2);
        let xs: Vec<f32> = edits.iter().filter_map(x_of).collect();
        assert_eq!(xs, [340.0, 940.0]);
        let (edits, placed) = paste_edits(&c, &clip, &"kf".into(), &mut counter());
        assert!(edits.is_empty() && placed.is_empty());
        let scene_clip = Clip::Scene("main".into());
        assert!(
            paste_edits(&c, &scene_clip, &"other".into(), &mut counter())
                .0
                .is_empty()
        );
    }

    #[test]
    fn pasting_leaves_out_a_nested_scene_that_would_contain_itself() {
        let c = collection();
        // `b` shows `inner`: pasted into `inner`, it would show itself.
        let clip = items_clip(&c, &"main".into(), &["a".into(), "b".into()]).expect("copied");
        let (_, placed) = paste_edits(&c, &clip, &"inner".into(), &mut counter());
        assert_eq!(placed.len(), 1);
        // Pasting `c` (an image) into `main` is fine, and so is a missing source left out.
        let mut gone = items_clip(&c, &"inner".into(), &["c".into()]).expect("copied");
        if let Clip::Items { items, .. } = &mut gone {
            items.push(item("d", "deleted", 0.0));
        }
        let (_, placed) = paste_edits(&c, &gone, &"main".into(), &mut counter());
        assert_eq!(placed.len(), 1);
    }

    #[test]
    fn duplicating_a_scene_copies_its_settings_and_items_below_it() {
        let mut c = collection();
        c.scenes[0].folder = Some(FolderId::new("fld"));
        c.scenes[0].transition_in = Some(TransitionPresetId::new("fade"));
        c.scenes[0].options.include_audio = false;
        c.scenes[0].filters = FilterChains {
            video: vec![blur()],
            audio: Vec::new(),
        };
        let edits = duplicate_scene_edits(&c, &"main".into(), "copy".into(), &mut counter());
        let names: Vec<&str> = edits
            .iter()
            .map(|e| match e {
                Edit::SceneCreate { name, .. } => {
                    assert_eq!(name, "MAIN copy");
                    "create"
                }
                Edit::SceneReorder { index, .. } => {
                    assert_eq!(*index, 1);
                    "reorder"
                }
                Edit::SceneSetFolder { .. } => "folder",
                Edit::SceneSetOptions { scene_id, options } => {
                    assert_eq!((scene_id.as_str(), options.include_audio), ("copy", false));
                    "options"
                }
                Edit::SceneSetTransitions { .. } => "transitions",
                Edit::FilterAdd {
                    owner: FilterOwner::Scene { scene_id },
                    ..
                } => {
                    assert_eq!(scene_id.as_str(), "copy");
                    "scene filter"
                }
                Edit::ItemAdd { scene_id, .. } => {
                    assert_eq!(scene_id.as_str(), "copy");
                    "item"
                }
                Edit::ItemSetCrop { .. } => "crop",
                Edit::ItemSetVisible { .. } => "hidden",
                Edit::FilterAdd { .. } => "effect",
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            names,
            [
                "create",
                "reorder",
                "folder",
                "options",
                "transitions",
                "scene filter",
                "item",
                "crop",
                "hidden",
                "effect",
                "item"
            ]
        );
        // The boxes stay where they are, and a keyframe isn't duplicated.
        let xs: Vec<f32> = edits.iter().filter_map(x_of).collect();
        assert_eq!(xs, [300.0, 900.0]);
        assert!(duplicate_scene_edits(&c, &"kf".into(), "k2".into(), &mut counter()).is_empty());
    }
}
